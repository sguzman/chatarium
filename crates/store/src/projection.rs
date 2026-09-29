//! Rebuildable SQLite projections derived from the authoritative JSONL journal.
//!
//! SQLite is never authoritative in Chatarium. It exists only to make durable journal history
//! queryable. A projection may be deleted and rebuilt without losing authorship or evidence.

use crate::EventEnvelope;
use crate::turn_projection::{
    AuthoredTurnRow, assistant_evidence_name, derive_authored_turns, local_evidence_name,
    parse_assistant_evidence, parse_local_evidence, parse_remote_evidence, remote_evidence_name,
};
use chatarium_core::{EventKind, LocalConversationId, LocalMessageId, LocalTurnId, TurnEvidence};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const PROJECTION_SCHEMA_VERSION: u32 = 2;

/// One error produced by the SQLite projection layer.
#[derive(Debug)]
pub enum ProjectionError {
    /// Filesystem work around the projection file failed.
    Io(std::io::Error),
    /// SQLite rejected an operation.
    Sqlite(rusqlite::Error),
    /// Journal evidence was internally inconsistent and cannot be projected safely.
    InvalidJournal(String),
    /// Derived SQLite projection contents are malformed or internally inconsistent.
    InvalidProjection(String),
    /// The projection schema is newer than this build understands.
    UnsupportedSchema(u32),
    /// A numeric journal value cannot be represented by SQLite's signed integer type.
    NumericOverflow(&'static str),
    #[cfg(test)]
    /// Test-only injected failure used to prove transactional rebuild rollback.
    InjectedFailure,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "projection I/O error: {error}"),
            Self::Sqlite(error) => write!(formatter, "projection SQLite error: {error}"),
            Self::InvalidJournal(detail) => {
                write!(formatter, "invalid journal for projection: {detail}")
            }
            Self::InvalidProjection(detail) => {
                write!(formatter, "invalid SQLite projection: {detail}")
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported projection schema version {version}")
            }
            Self::NumericOverflow(field) => {
                write!(
                    formatter,
                    "journal field '{field}' exceeds SQLite integer range"
                )
            }
            #[cfg(test)]
            Self::InjectedFailure => write!(formatter, "injected projection rebuild failure"),
        }
    }
}

impl Error for ProjectionError {}

impl From<std::io::Error> for ProjectionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for ProjectionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// Queryable, disposable SQLite projection of durable journal events.
#[derive(Debug)]
pub struct SqliteProjection {
    path: PathBuf,
    connection: Connection,
}

impl SqliteProjection {
    /// Open or create a projection database and migrate it to the current schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProjectionError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(&path)?;
        let mut projection = Self { path, connection };
        projection.migrate()?;
        Ok(projection)
    }

    /// Filesystem path backing this disposable projection.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current projection schema version.
    pub fn schema_version(&self) -> Result<u32, ProjectionError> {
        let version = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
        Ok(version)
    }

    /// Highest journal sequence represented by the projection.
    pub fn last_sequence(&self) -> Result<Option<u64>, ProjectionError> {
        let value =
            self.connection
                .query_row("SELECT MAX(sequence) FROM projected_events", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })?;
        value.map(i64_to_u64).transpose()
    }

    /// Number of projected event rows.
    pub fn event_count(&self) -> Result<u64, ProjectionError> {
        let count =
            self.connection
                .query_row("SELECT COUNT(*) FROM projected_events", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        i64_to_u64(count)
    }

    /// Schema version against which the current materialized rows were last rebuilt.
    pub fn materialized_schema_version(&self) -> Result<Option<u32>, ProjectionError> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM projection_meta WHERE key = 'last_rebuild_schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|raw| {
                raw.parse::<u32>().map_err(|error| {
                    ProjectionError::InvalidProjection(format!(
                        "invalid last_rebuild_schema_version value '{raw}': {error}"
                    ))
                })
            })
            .transpose()
    }

    /// Whether the projection covers exactly the supplied durable event sequence and current
    /// materialized schema.
    pub fn is_current_with(&self, events: &[EventEnvelope]) -> Result<bool, ProjectionError> {
        validate_sequences(events)?;
        let expected_last = events.last().map(|event| event.sequence);
        let expected_count = u64::try_from(events.len())
            .map_err(|_| ProjectionError::NumericOverflow("event_count"))?;
        Ok(
            self.materialized_schema_version()? == Some(PROJECTION_SCHEMA_VERSION)
                && self.last_sequence()? == expected_last
                && self.event_count()? == expected_count,
        )
    }

    /// Rebuild the entire SQLite projection transactionally from authoritative journal events.
    pub fn rebuild(&mut self, events: &[EventEnvelope]) -> Result<(), ProjectionError> {
        self.rebuild_impl(events, None)
    }

    /// Read all typed authored turns in durable commit-sequence order.
    pub fn authored_turns(&self) -> Result<Vec<AuthoredTurnRow>, ProjectionError> {
        self.query_authored_turns(
            "SELECT conversation_id, turn_id, message_id, exact_user_text,
                    commit_sequence, last_sequence, local_evidence, remote_evidence,
                    assistant_evidence
             FROM projected_authored_turns
             ORDER BY commit_sequence",
            [],
        )
    }

    /// Read one typed authored turn by local turn identity.
    pub fn authored_turn(
        &self,
        turn_id: LocalTurnId,
    ) -> Result<Option<AuthoredTurnRow>, ProjectionError> {
        let raw = turn_id.to_string();
        let mut rows = self.query_authored_turns(
            "SELECT conversation_id, turn_id, message_id, exact_user_text,
                    commit_sequence, last_sequence, local_evidence, remote_evidence,
                    assistant_evidence
             FROM projected_authored_turns
             WHERE turn_id = ?1
             ORDER BY commit_sequence",
            [raw.as_str()],
        )?;
        Ok(rows.pop())
    }

    /// Read typed authored turns for one local conversation in commit-sequence order.
    pub fn authored_turns_for_conversation(
        &self,
        conversation_id: LocalConversationId,
    ) -> Result<Vec<AuthoredTurnRow>, ProjectionError> {
        let raw = conversation_id.to_string();
        self.query_authored_turns(
            "SELECT conversation_id, turn_id, message_id, exact_user_text,
                    commit_sequence, last_sequence, local_evidence, remote_evidence,
                    assistant_evidence
             FROM projected_authored_turns
             WHERE conversation_id = ?1
             ORDER BY commit_sequence",
            [raw.as_str()],
        )
    }

    /// Read every projected event in durable sequence order.
    pub fn events(&self) -> Result<Vec<EventEnvelope>, ProjectionError> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, at_unix_ms, scope, kind, payload
             FROM projected_events
             ORDER BY sequence",
        )?;
        let rows = statement.query_map([], |row| {
            let sequence: i64 = row.get(0)?;
            let at_unix_ms: i64 = row.get(1)?;
            let scope: Option<String> = row.get(2)?;
            let kind_name: String = row.get(3)?;
            let payload: String = row.get(4)?;
            Ok((sequence, at_unix_ms, scope, kind_name, payload))
        })?;

        collect_projected_rows(rows)
    }

    /// Read projected events for one exact scope in durable sequence order.
    pub fn events_for_scope(&self, scope: &str) -> Result<Vec<EventEnvelope>, ProjectionError> {
        self.query_events(
            "SELECT sequence, at_unix_ms, scope, kind, payload
             FROM projected_events
             WHERE scope = ?1
             ORDER BY sequence",
            [scope],
        )
    }

    /// Read projected events of one semantic kind in durable sequence order.
    pub fn events_of_kind(&self, kind: EventKind) -> Result<Vec<EventEnvelope>, ProjectionError> {
        self.query_events(
            "SELECT sequence, at_unix_ms, scope, kind, payload
             FROM projected_events
             WHERE kind = ?1
             ORDER BY sequence",
            [kind.stable_name()],
        )
    }

    fn query_authored_turns<const N: usize>(
        &self,
        sql: &str,
        parameters: [&str; N],
    ) -> Result<Vec<AuthoredTurnRow>, ProjectionError> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut projected = Vec::new();
        for row in rows {
            let (
                conversation_id,
                turn_id,
                message_id,
                exact_user_text,
                commit_sequence,
                last_sequence,
                local_evidence,
                remote_evidence,
                assistant_evidence,
            ) = row?;
            projected.push(AuthoredTurnRow {
                conversation_id: parse_projection_id::<LocalConversationId>(
                    &conversation_id,
                    "conversation_id",
                )?,
                turn_id: parse_projection_id::<LocalTurnId>(&turn_id, "turn_id")?,
                message_id: parse_projection_id::<LocalMessageId>(&message_id, "message_id")?,
                exact_user_text,
                commit_sequence: i64_to_u64(commit_sequence)?,
                last_sequence: i64_to_u64(last_sequence)?,
                evidence: TurnEvidence {
                    local: parse_local_evidence(&local_evidence).ok_or_else(|| {
                        ProjectionError::InvalidProjection(format!(
                            "unknown local evidence '{local_evidence}'"
                        ))
                    })?,
                    remote: parse_remote_evidence(&remote_evidence).ok_or_else(|| {
                        ProjectionError::InvalidProjection(format!(
                            "unknown remote evidence '{remote_evidence}'"
                        ))
                    })?,
                    assistant: parse_assistant_evidence(&assistant_evidence).ok_or_else(|| {
                        ProjectionError::InvalidProjection(format!(
                            "unknown assistant evidence '{assistant_evidence}'"
                        ))
                    })?,
                },
            });
        }
        Ok(projected)
    }

    fn query_events<const N: usize>(
        &self,
        sql: &str,
        parameters: [&str; N],
    ) -> Result<Vec<EventEnvelope>, ProjectionError> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
            let sequence: i64 = row.get(0)?;
            let at_unix_ms: i64 = row.get(1)?;
            let scope: Option<String> = row.get(2)?;
            let kind_name: String = row.get(3)?;
            let payload: String = row.get(4)?;
            Ok((sequence, at_unix_ms, scope, kind_name, payload))
        })?;
        collect_projected_rows(rows)
    }

    fn migrate(&mut self) -> Result<(), ProjectionError> {
        let current = self.schema_version()?;
        match current {
            0 => {
                let transaction = self.connection.transaction()?;
                transaction.execute_batch(
                    "CREATE TABLE projection_meta (
                        key TEXT PRIMARY KEY NOT NULL,
                        value TEXT NOT NULL
                    );
                    CREATE TABLE projected_events (
                        sequence INTEGER PRIMARY KEY NOT NULL,
                        at_unix_ms INTEGER NOT NULL,
                        scope TEXT,
                        kind TEXT NOT NULL,
                        payload TEXT NOT NULL
                    );
                    CREATE INDEX projected_events_scope_sequence
                        ON projected_events(scope, sequence);
                    CREATE INDEX projected_events_kind_sequence
                        ON projected_events(kind, sequence);
                    CREATE TABLE projected_authored_turns (
                        turn_id TEXT PRIMARY KEY NOT NULL,
                        conversation_id TEXT NOT NULL,
                        message_id TEXT NOT NULL UNIQUE,
                        exact_user_text TEXT NOT NULL,
                        commit_sequence INTEGER NOT NULL UNIQUE,
                        last_sequence INTEGER NOT NULL,
                        local_evidence TEXT NOT NULL,
                        remote_evidence TEXT NOT NULL,
                        assistant_evidence TEXT NOT NULL
                    );
                    CREATE INDEX projected_authored_turns_conversation_commit
                        ON projected_authored_turns(conversation_id, commit_sequence);
                    INSERT INTO projection_meta(key, value)
                        VALUES ('schema_version', '2');
                    PRAGMA user_version = 2;",
                )?;
                transaction.commit()?;
                Ok(())
            }
            1 => {
                let transaction = self.connection.transaction()?;
                transaction.execute_batch(
                    "CREATE TABLE projected_authored_turns (
                        turn_id TEXT PRIMARY KEY NOT NULL,
                        conversation_id TEXT NOT NULL,
                        message_id TEXT NOT NULL UNIQUE,
                        exact_user_text TEXT NOT NULL,
                        commit_sequence INTEGER NOT NULL UNIQUE,
                        last_sequence INTEGER NOT NULL,
                        local_evidence TEXT NOT NULL,
                        remote_evidence TEXT NOT NULL,
                        assistant_evidence TEXT NOT NULL
                    );
                    CREATE INDEX projected_authored_turns_conversation_commit
                        ON projected_authored_turns(conversation_id, commit_sequence);
                    INSERT INTO projection_meta(key, value)
                        VALUES ('schema_version', '2')
                        ON CONFLICT(key) DO UPDATE SET value = excluded.value;
                    DELETE FROM projection_meta
                        WHERE key = 'last_rebuild_schema_version';
                    PRAGMA user_version = 2;",
                )?;
                transaction.commit()?;
                Ok(())
            }
            PROJECTION_SCHEMA_VERSION => Ok(()),
            newer => Err(ProjectionError::UnsupportedSchema(newer)),
        }
    }

    fn rebuild_impl(
        &mut self,
        events: &[EventEnvelope],
        fail_after: Option<usize>,
    ) -> Result<(), ProjectionError> {
        validate_sequences(events)?;
        let authored_turns =
            derive_authored_turns(events).map_err(ProjectionError::InvalidJournal)?;

        let transaction = self.connection.transaction()?;
        replace_projected_events(&transaction, events, fail_after)?;
        replace_authored_turns(&transaction, &authored_turns)?;
        transaction.execute(
            "INSERT INTO projection_meta(key, value)
             VALUES ('last_rebuild_event_count', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [events.len().to_string()],
        )?;
        transaction.execute(
            "INSERT INTO projection_meta(key, value)
             VALUES ('last_rebuild_schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [PROJECTION_SCHEMA_VERSION.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn rebuild_with_injected_failure(
        &mut self,
        events: &[EventEnvelope],
        fail_after: usize,
    ) -> Result<(), ProjectionError> {
        self.rebuild_impl(events, Some(fail_after))
    }
}

fn replace_projected_events(
    transaction: &Transaction<'_>,
    events: &[EventEnvelope],
    fail_after: Option<usize>,
) -> Result<(), ProjectionError> {
    transaction.execute("DELETE FROM projected_events", [])?;
    let mut statement = transaction.prepare(
        "INSERT INTO projected_events(sequence, at_unix_ms, scope, kind, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;

    #[cfg(not(test))]
    let _ = fail_after;

    for (index, event) in events.iter().enumerate() {
        statement.execute(params![
            u64_to_i64(event.sequence, "sequence")?,
            u64_to_i64(event.at_unix_ms, "at_unix_ms")?,
            event.scope.as_deref(),
            event.kind.stable_name(),
            event.payload.as_str(),
        ])?;

        #[cfg(test)]
        if fail_after == Some(index.saturating_add(1)) {
            return Err(ProjectionError::InjectedFailure);
        }
    }
    Ok(())
}

fn replace_authored_turns(
    transaction: &Transaction<'_>,
    rows: &[AuthoredTurnRow],
) -> Result<(), ProjectionError> {
    transaction.execute("DELETE FROM projected_authored_turns", [])?;
    let mut statement = transaction.prepare(
        "INSERT INTO projected_authored_turns(
            turn_id, conversation_id, message_id, exact_user_text,
            commit_sequence, last_sequence, local_evidence, remote_evidence,
            assistant_evidence
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;

    for row in rows {
        statement.execute(params![
            row.turn_id.to_string(),
            row.conversation_id.to_string(),
            row.message_id.to_string(),
            row.exact_user_text.as_str(),
            u64_to_i64(row.commit_sequence, "commit_sequence")?,
            u64_to_i64(row.last_sequence, "last_sequence")?,
            local_evidence_name(row.evidence.local),
            remote_evidence_name(row.evidence.remote),
            assistant_evidence_name(row.evidence.assistant),
        ])?;
    }
    Ok(())
}

fn parse_projection_id<T>(raw: &str, field: &str) -> Result<T, ProjectionError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse::<T>().map_err(|error| {
        ProjectionError::InvalidProjection(format!("invalid {field} value '{raw}': {error}"))
    })
}

fn collect_projected_rows<T>(
    rows: rusqlite::MappedRows<'_, T>,
) -> Result<Vec<EventEnvelope>, ProjectionError>
where
    T: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64, Option<String>, String, String)>,
{
    let mut projected = Vec::new();
    for row in rows {
        let (sequence, at_unix_ms, scope, kind_name, payload) = row?;
        let kind = EventKind::from_stable_name(&kind_name).ok_or_else(|| {
            ProjectionError::InvalidJournal(format!(
                "projection contains unknown event kind '{kind_name}'"
            ))
        })?;
        projected.push(EventEnvelope {
            sequence: i64_to_u64(sequence)?,
            at_unix_ms: i64_to_u64(at_unix_ms)?,
            scope,
            kind,
            payload,
        });
    }
    Ok(projected)
}

fn validate_sequences(events: &[EventEnvelope]) -> Result<(), ProjectionError> {
    for (index, event) in events.iter().enumerate() {
        let expected = u64::try_from(index)
            .map_err(|_| ProjectionError::NumericOverflow("sequence_index"))?
            .checked_add(1)
            .ok_or(ProjectionError::NumericOverflow("sequence"))?;
        if event.sequence != expected {
            return Err(ProjectionError::InvalidJournal(format!(
                "event at index {index} has sequence {}, expected {expected}",
                event.sequence
            )));
        }
    }
    Ok(())
}

fn u64_to_i64(value: u64, field: &'static str) -> Result<i64, ProjectionError> {
    i64::try_from(value).map_err(|_| ProjectionError::NumericOverflow(field))
}

fn i64_to_u64(value: i64) -> Result<u64, ProjectionError> {
    u64::try_from(value).map_err(|_| {
        ProjectionError::InvalidJournal(format!(
            "projection contains negative integer value {value}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authored::{commit_user_message, local_turn_scope};
    use crate::{EventStore, MemoryEventStore};
    use chatarium_core::{
        AssistantEvidence, AuthoredUserMessage, LocalConversationId, LocalEvidence, LocalMessageId,
        LocalTurnId, RemoteEvidence,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-projection-{label}-{}-{nonce}.sqlite3",
            std::process::id()
        ))
    }

    fn sample_events() -> Vec<EventEnvelope> {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some("conversation:local-a".to_owned()),
                EventKind::DraftChanged,
                "draft".to_owned(),
            )
            .expect("append draft");
        store
            .append_scoped(
                Some("conversation:local-a".to_owned()),
                EventKind::UserMessageCommitted,
                "exact message".to_owned(),
            )
            .expect("append message");
        store.events().to_vec()
    }

    fn authored_message(text: &str) -> AuthoredUserMessage {
        AuthoredUserMessage::new(
            LocalConversationId::new(),
            LocalTurnId::new(),
            LocalMessageId::new(),
            text,
        )
    }

    fn typed_turn_events(
        message: &AuthoredUserMessage,
        following: &[EventKind],
    ) -> Vec<EventEnvelope> {
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, message).expect("commit typed message");
        let scope = local_turn_scope(message.turn_id);
        for kind in following {
            store
                .append_scoped(Some(scope.clone()), *kind, String::new())
                .expect("append turn event");
        }
        store.events().to_vec()
    }

    fn create_v1_projection(path: &Path, events: &[EventEnvelope]) {
        let connection = Connection::open(path).expect("open v1 fixture");
        connection
            .execute_batch(
                "CREATE TABLE projection_meta (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                );
                CREATE TABLE projected_events (
                    sequence INTEGER PRIMARY KEY NOT NULL,
                    at_unix_ms INTEGER NOT NULL,
                    scope TEXT,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL
                );
                CREATE INDEX projected_events_scope_sequence
                    ON projected_events(scope, sequence);
                CREATE INDEX projected_events_kind_sequence
                    ON projected_events(kind, sequence);
                INSERT INTO projection_meta(key, value)
                    VALUES ('schema_version', '1');
                PRAGMA user_version = 1;",
            )
            .expect("create v1 schema");
        for event in events {
            connection
                .execute(
                    "INSERT INTO projected_events(sequence, at_unix_ms, scope, kind, payload)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        i64::try_from(event.sequence).unwrap(),
                        i64::try_from(event.at_unix_ms).unwrap(),
                        event.scope.as_deref(),
                        event.kind.stable_name(),
                        event.payload.as_str(),
                    ],
                )
                .expect("insert v1 projected event");
        }
    }

    #[test]
    fn empty_projection_rebuilds_and_reports_schema() {
        let path = temp_path("empty");
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        assert_eq!(
            projection.schema_version().unwrap(),
            PROJECTION_SCHEMA_VERSION
        );
        projection.rebuild(&[]).expect("empty rebuild");
        assert_eq!(projection.event_count().unwrap(), 0);
        assert_eq!(projection.last_sequence().unwrap(), None);
        assert!(projection.is_current_with(&[]).unwrap());
        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rebuild_preserves_event_shape_and_scope() {
        let path = temp_path("shape");
        let events = sample_events();
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        projection.rebuild(&events).expect("rebuild");
        assert_eq!(projection.events().unwrap(), events);
        assert!(projection.is_current_with(&events).unwrap());
        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn projection_queries_by_scope_and_kind() {
        let path = temp_path("queries");
        let events = sample_events();
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        projection.rebuild(&events).expect("rebuild");

        assert_eq!(
            projection
                .events_for_scope("conversation:local-a")
                .expect("scope query"),
            events
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::UserMessageCommitted)
                .expect("kind query"),
            vec![events[1].clone()]
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn deleting_projection_and_rebuilding_is_lossless() {
        let path = temp_path("delete-rebuild");
        let events = sample_events();

        {
            let mut projection = SqliteProjection::open(&path).expect("open projection");
            projection.rebuild(&events).expect("rebuild");
        }
        fs::remove_file(&path).expect("delete projection");

        let mut rebuilt = SqliteProjection::open(&path).expect("recreate projection");
        rebuilt.rebuild(&events).expect("rebuild from journal");
        assert_eq!(rebuilt.events().unwrap(), events);
        drop(rebuilt);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rebuild_is_idempotent() {
        let path = temp_path("idempotent");
        let events = sample_events();
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        projection.rebuild(&events).expect("first rebuild");
        let first = projection.events().unwrap();
        projection.rebuild(&events).expect("second rebuild");
        assert_eq!(projection.events().unwrap(), first);
        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn authored_turn_projection_preserves_exact_typed_commit_and_queries() {
        let path = temp_path("authored-turn");
        let message = authored_message(" exact text\nwith spacing ");
        let events = typed_turn_events(&message, &[]);
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        projection.rebuild(&events).expect("rebuild");

        let rows = projection.authored_turns().expect("authored turns");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.conversation_id, message.conversation_id);
        assert_eq!(row.turn_id, message.turn_id);
        assert_eq!(row.message_id, message.message_id);
        assert_eq!(row.exact_user_text, message.text);
        assert_eq!(row.commit_sequence, 1);
        assert_eq!(row.last_sequence, 1);
        assert_eq!(row.evidence.local, LocalEvidence::MessageCommitted);
        assert_eq!(row.evidence.remote, RemoteEvidence::NotAttempted);
        assert_eq!(row.evidence.assistant, AssistantEvidence::None);
        assert_eq!(
            projection.authored_turn(message.turn_id).unwrap(),
            Some(row.clone())
        );
        assert_eq!(
            projection
                .authored_turns_for_conversation(message.conversation_id)
                .unwrap(),
            rows
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn authored_turn_projection_replays_ambiguous_and_partial_states() {
        let path = temp_path("authored-evidence");
        let first = authored_message("first");
        let second = authored_message("second");
        let mut store = MemoryEventStore::default();

        commit_user_message(&mut store, &first).unwrap();
        let first_scope = local_turn_scope(first.turn_id);
        for kind in [
            EventKind::DispatchAttempted,
            EventKind::TransportInterrupted,
        ] {
            store
                .append_scoped(Some(first_scope.clone()), kind, String::new())
                .unwrap();
        }

        commit_user_message(&mut store, &second).unwrap();
        let second_scope = local_turn_scope(second.turn_id);
        for kind in [
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::AssistantStreamStarted,
            EventKind::AssistantDeltaObserved,
            EventKind::TransportInterrupted,
        ] {
            store
                .append_scoped(Some(second_scope.clone()), kind, String::new())
                .unwrap();
        }

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        let rows = projection.authored_turns().unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].evidence.remote, RemoteEvidence::OutcomeUnknown);
        assert_eq!(rows[0].evidence.assistant, AssistantEvidence::None);
        assert_eq!(rows[0].last_sequence, 3);
        assert_eq!(rows[1].evidence.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(
            rows[1].evidence.assistant,
            AssistantEvidence::PartialInterrupted
        );
        assert_eq!(rows[1].last_sequence, 9);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn authored_turn_projection_observes_later_completion() {
        let path = temp_path("authored-complete");
        let message = authored_message("turn");
        let events = typed_turn_events(
            &message,
            &[
                EventKind::DispatchAttempted,
                EventKind::RemoteAcceptanceObserved,
                EventKind::AssistantStreamStarted,
                EventKind::TransportInterrupted,
                EventKind::AssistantCompletionObserved,
            ],
        );
        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(&events).unwrap();

        let row = projection.authored_turn(message.turn_id).unwrap().unwrap();
        assert_eq!(row.evidence.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(row.evidence.assistant, AssistantEvidence::CompletedObserved);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_commit_remains_event_only() {
        let path = temp_path("legacy-authored");
        let events = sample_events();
        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(&events).unwrap();

        assert_eq!(projection.events().unwrap(), events);
        assert!(projection.authored_turns().unwrap().is_empty());

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn deleting_projection_rebuilds_identical_authored_turns() {
        let path = temp_path("authored-delete-rebuild");
        let message = authored_message("durable");
        let events = typed_turn_events(
            &message,
            &[
                EventKind::DispatchAttempted,
                EventKind::RemoteAcceptanceObserved,
                EventKind::AssistantCompletionObserved,
            ],
        );

        let first = {
            let mut projection = SqliteProjection::open(&path).unwrap();
            projection.rebuild(&events).unwrap();
            projection.authored_turns().unwrap()
        };
        fs::remove_file(&path).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(&events).unwrap();
        assert_eq!(projection.authored_turns().unwrap(), first);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn v1_projection_migration_requires_materialized_rebuild() {
        let path = temp_path("v1-migrate");
        let message = authored_message("migrate");
        let events = typed_turn_events(&message, &[]);
        create_v1_projection(&path, &events);

        let mut projection = SqliteProjection::open(&path).expect("migrate projection");
        assert_eq!(projection.schema_version().unwrap(), 2);
        assert_eq!(projection.materialized_schema_version().unwrap(), None);
        assert!(!projection.is_current_with(&events).unwrap());
        assert!(projection.authored_turns().unwrap().is_empty());

        projection.rebuild(&events).unwrap();
        assert_eq!(projection.materialized_schema_version().unwrap(), Some(2));
        assert!(projection.is_current_with(&events).unwrap());
        assert_eq!(projection.authored_turns().unwrap().len(), 1);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_typed_commit_rebuild_preserves_previous_projection() {
        let path = temp_path("authored-rollback");
        let original_message = authored_message("original");
        let original = typed_turn_events(&original_message, &[]);
        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(&original).unwrap();
        let original_rows = projection.authored_turns().unwrap();
        let original_events = projection.events().unwrap();

        let malformed_message = authored_message("malformed");
        let mut changed = original.clone();
        changed.push(EventEnvelope {
            sequence: 2,
            at_unix_ms: 2,
            scope: Some(local_turn_scope(malformed_message.turn_id)),
            kind: EventKind::UserMessageCommitted,
            payload: serde_json::json!({
                "schema": "chatarium-user-message-commit",
                "version": 1,
                "conversation_id": "not-a-uuid",
                "turn_id": malformed_message.turn_id.to_string(),
                "message_id": malformed_message.message_id.to_string(),
                "text": malformed_message.text,
            })
            .to_string(),
        });

        let error = projection.rebuild(&changed).expect_err("malformed rebuild");
        assert!(matches!(error, ProjectionError::InvalidJournal(_)));
        assert_eq!(projection.events().unwrap(), original_events);
        assert_eq!(projection.authored_turns().unwrap(), original_rows);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_invariant_failure_preserves_previous_projection() {
        let path = temp_path("authored-replay-rollback");
        let original_message = authored_message("original");
        let original = typed_turn_events(&original_message, &[]);
        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(&original).unwrap();
        let original_rows = projection.authored_turns().unwrap();

        let broken_message = authored_message("broken");
        let scope = local_turn_scope(broken_message.turn_id);
        let mut broken_store = MemoryEventStore::default();
        broken_store
            .append_scoped(
                Some(scope.clone()),
                EventKind::DispatchAttempted,
                String::new(),
            )
            .unwrap();
        commit_user_message(&mut broken_store, &broken_message).unwrap();

        let error = projection
            .rebuild(broken_store.events())
            .expect_err("dispatch before commit");
        assert!(matches!(error, ProjectionError::InvalidJournal(_)));
        assert_eq!(projection.authored_turns().unwrap(), original_rows);
        assert_eq!(projection.events().unwrap(), original);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_sequence_is_rejected_before_projection_mutation() {
        let path = temp_path("sequence");
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        let mut events = sample_events();
        events[1].sequence = 3;
        let error = projection.rebuild(&events).expect_err("reject gap");
        assert!(matches!(error, ProjectionError::InvalidJournal(_)));
        assert_eq!(projection.event_count().unwrap(), 0);
        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn injected_rebuild_failure_rolls_back_previous_projection() {
        let path = temp_path("rollback");
        let original = sample_events();
        let mut projection = SqliteProjection::open(&path).expect("open projection");
        projection.rebuild(&original).expect("initial rebuild");

        let mut changed = original.clone();
        changed.push(EventEnvelope {
            sequence: 3,
            at_unix_ms: 3,
            scope: Some("conversation:local-a".to_owned()),
            kind: EventKind::AssistantSnapshotObserved,
            payload: "assistant".to_owned(),
        });

        let error = projection
            .rebuild_with_injected_failure(&changed, 1)
            .expect_err("injected failure");
        assert!(matches!(error, ProjectionError::InjectedFailure));
        assert_eq!(projection.events().unwrap(), original);
        drop(projection);
        let _ = fs::remove_file(path);
    }
}
