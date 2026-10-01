//! Durable P3 mirror snapshots imported from validated C02 conversation fetches.
//!
//! The append-only journal remains authoritative. The exact fetched JSON body
//! is retained only in local durable state; public protocol fixtures stay
//! sanitized. Replay re-runs the evidence-gated parser and verifies that the
//! durable selection, identity binding, and read-observation provenance existed
//! before the snapshot was imported.

use crate::remote_mirror_execution::{
    RemoteMirrorExecutionPlan, derive_remote_mirror_execution_plan,
};
use crate::remote_mirror_readiness::{RemoteMirrorBlocked, RemoteMirrorReady};
use crate::{EventEnvelope, EventStore};
use chatarium_core::remote::{ProtocolObservationRevision, RemoteConversationId};
use chatarium_core::{EventKind, LocalConversationId, RemoteReadObservationId};
use chatarium_protocol::conversation_fetch::{
    ConversationFetchEnvelope, ConversationFetchParseError, parse_conversation_fetch_response,
};
use serde_json::{Value, json};
use std::fmt;

const REMOTE_MIRROR_SNAPSHOT_SCHEMA: &str = "chatarium-remote-conversation-snapshot";
const REMOTE_MIRROR_SNAPSHOT_VERSION: u64 = 1;

/// One restart-replayable local mirror snapshot of a remote conversation.
///
/// The protocol envelope is typed. raw_body preserves the exact fetched response
/// locally, including fields the current parser intentionally does not interpret.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteConversationSnapshotAuditRecord {
    pub local_conversation_id: LocalConversationId,
    pub remote_conversation_id: RemoteConversationId,
    pub protocol_revision: ProtocolObservationRevision,
    pub read_observation_id: RemoteReadObservationId,
    pub binding_sequence: u64,
    pub read_observation_sequence: u64,
    pub envelope: ConversationFetchEnvelope,
    pub raw_body: Value,
    pub imported_sequence: u64,
}

/// Result of one idempotent durable mirror import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteConversationSnapshotImportResult {
    /// Existing or newly appended durable journal sequence.
    pub sequence: u64,
    /// Whether this invocation appended a new snapshot event.
    pub appended: bool,
}

/// Fail-closed error from validated remote conversation import.
#[derive(Debug)]
pub enum RemoteConversationSnapshotImportError {
    /// Durable journal history could not be replayed consistently.
    InvalidJournal(String),
    /// The local conversation is not selected for mirroring.
    Unselected,
    /// Durable protocol evidence does not currently permit a semantic fetch.
    ProtocolBlocked(RemoteMirrorBlocked),
    /// The fetched JSON does not match the validated C02 parser contract.
    Parse(ConversationFetchParseError),
    /// Appending the validated snapshot to durable storage failed.
    Persistence(std::io::Error),
}

impl fmt::Display for RemoteConversationSnapshotImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJournal(detail) => {
                write!(formatter, "invalid durable mirror history: {detail}")
            }
            Self::Unselected => write!(formatter, "conversation is not selected for mirroring"),
            Self::ProtocolBlocked(blocked) => {
                write!(
                    formatter,
                    "remote mirror protocol gate is blocked: {blocked:?}"
                )
            }
            Self::Parse(error) => write!(formatter, "conversation-fetch parse failed: {error}"),
            Self::Persistence(error) => {
                write!(formatter, "persist remote conversation snapshot: {error}")
            }
        }
    }
}

impl std::error::Error for RemoteConversationSnapshotImportError {}

/// Import one already-fetched C02 response into durable local mirror state.
///
/// This function performs no network I/O and creates no authentication authority.
/// The caller obtains the body through the authenticated-session execution
/// boundary. Import independently re-derives selection and protocol readiness
/// from durable history, so possession of a response body cannot bypass them.
pub fn import_validated_remote_conversation_snapshot(
    store: &mut impl EventStore,
    local_conversation_id: LocalConversationId,
    body: &Value,
) -> Result<RemoteConversationSnapshotImportResult, RemoteConversationSnapshotImportError> {
    let existing = replay_remote_conversation_snapshot_audit(store.events())
        .map_err(RemoteConversationSnapshotImportError::InvalidJournal)?;
    let ready = require_ready_execution_plan(store.events(), local_conversation_id)?;

    let envelope = parse_conversation_fetch_response(
        ready.protocol_revision.as_str(),
        body,
        Some(ready.remote_conversation_id.as_str()),
    )
    .map_err(RemoteConversationSnapshotImportError::Parse)?;

    let candidate = RemoteConversationSnapshotAuditRecord {
        local_conversation_id,
        remote_conversation_id: ready.remote_conversation_id.clone(),
        protocol_revision: ready.protocol_revision.clone(),
        read_observation_id: ready.read_observation_id,
        binding_sequence: ready.binding_sequence,
        read_observation_sequence: ready.read_observation_sequence,
        envelope,
        raw_body: body.clone(),
        imported_sequence: 0,
    };

    if let Some(record) = existing
        .iter()
        .find(|record| same_snapshot(record, &candidate))
    {
        return Ok(RemoteConversationSnapshotImportResult {
            sequence: record.imported_sequence,
            appended: false,
        });
    }

    let payload = encode_snapshot_payload(&candidate)
        .map_err(RemoteConversationSnapshotImportError::Persistence)?;
    let sequence = store
        .append_scoped(
            Some(remote_conversation_snapshot_scope(local_conversation_id)),
            EventKind::RemoteConversationSnapshotImported,
            payload,
        )
        .map_err(RemoteConversationSnapshotImportError::Persistence)?;

    Ok(RemoteConversationSnapshotImportResult {
        sequence,
        appended: true,
    })
}

/// Replay all durable remote conversation mirror snapshots.
///
/// Every snapshot must have been preceded by a selected-and-ready durable mirror
/// plan with exactly matching identity and read-observation provenance. The
/// private body is parsed again against its named protocol revision.
pub fn replay_remote_conversation_snapshot_audit(
    events: &[EventEnvelope],
) -> Result<Vec<RemoteConversationSnapshotAuditRecord>, String> {
    let mut records = Vec::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::RemoteConversationSnapshotImported {
            continue;
        }

        let payload = typed_payload(event)?;
        let local_conversation_id = required_string(&payload, "local_conversation_id")?
            .parse::<LocalConversationId>()
            .map_err(|error| {
                format!(
                    "invalid remote mirror local_conversation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        validate_scope(event, local_conversation_id)?;

        let remote_conversation_id =
            RemoteConversationId::new(required_string(&payload, "remote_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "invalid remote mirror remote_conversation_id at sequence {}: {error}",
                        event.sequence
                    )
                })?;
        let protocol_revision =
            ProtocolObservationRevision::new(required_string(&payload, "protocol_revision")?)
                .map_err(|error| {
                    format!(
                        "invalid remote mirror protocol_revision at sequence {}: {error}",
                        event.sequence
                    )
                })?;
        let read_observation_id = required_string(&payload, "read_observation_id")?
            .parse::<RemoteReadObservationId>()
            .map_err(|error| {
                format!(
                    "invalid remote mirror read_observation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        let binding_sequence = required_u64(&payload, "binding_sequence")?;
        let read_observation_sequence = required_u64(&payload, "read_observation_sequence")?;
        let raw_body = payload
            .get("body")
            .ok_or_else(|| {
                format!(
                    "remote mirror snapshot at sequence {} is missing field 'body'",
                    event.sequence
                )
            })?
            .clone();

        let ready = require_ready_execution_plan_for_replay(
            &events[..index],
            local_conversation_id,
            event.sequence,
        )?;
        validate_provenance(
            event.sequence,
            &ready,
            &remote_conversation_id,
            &protocol_revision,
            read_observation_id,
            binding_sequence,
            read_observation_sequence,
        )?;

        let envelope = parse_conversation_fetch_response(
            protocol_revision.as_str(),
            &raw_body,
            Some(remote_conversation_id.as_str()),
        )
        .map_err(|error| {
            format!(
                "remote mirror snapshot at sequence {} does not parse against revision {:?}: {error}",
                event.sequence,
                protocol_revision.as_str()
            )
        })?;

        let record = RemoteConversationSnapshotAuditRecord {
            local_conversation_id,
            remote_conversation_id,
            protocol_revision,
            read_observation_id,
            binding_sequence,
            read_observation_sequence,
            envelope,
            raw_body,
            imported_sequence: event.sequence,
        };

        if records
            .iter()
            .any(|existing| same_snapshot(existing, &record))
        {
            return Err(format!(
                "redundant remote mirror snapshot for local conversation {} at sequence {}",
                local_conversation_id, event.sequence
            ));
        }

        records.push(record);
    }

    Ok(records)
}

/// Stable local-only journal scope for one remote mirror snapshot lineage.
#[must_use]
pub fn remote_conversation_snapshot_scope(local_conversation_id: LocalConversationId) -> String {
    format!("remote-conversation-snapshot:{local_conversation_id}")
}

fn require_ready_execution_plan(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Result<RemoteMirrorReady, RemoteConversationSnapshotImportError> {
    let plan = derive_remote_mirror_execution_plan(events, local_conversation_id)
        .map_err(RemoteConversationSnapshotImportError::InvalidJournal)?;
    match plan {
        RemoteMirrorExecutionPlan::Unselected => {
            Err(RemoteConversationSnapshotImportError::Unselected)
        }
        RemoteMirrorExecutionPlan::ProtocolBlocked(blocked) => Err(
            RemoteConversationSnapshotImportError::ProtocolBlocked(blocked),
        ),
        RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready) => Ok(ready),
    }
}

fn require_ready_execution_plan_for_replay(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
    sequence: u64,
) -> Result<RemoteMirrorReady, String> {
    let plan = derive_remote_mirror_execution_plan(events, local_conversation_id)?;
    match plan {
        RemoteMirrorExecutionPlan::Unselected => Err(format!(
            "remote mirror snapshot at sequence {sequence} was imported before conversation {local_conversation_id} was selected"
        )),
        RemoteMirrorExecutionPlan::ProtocolBlocked(blocked) => Err(format!(
            "remote mirror snapshot at sequence {sequence} was imported while protocol readiness was blocked: {blocked:?}"
        )),
        RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready) => Ok(ready),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_provenance(
    sequence: u64,
    ready: &RemoteMirrorReady,
    remote_conversation_id: &RemoteConversationId,
    protocol_revision: &ProtocolObservationRevision,
    read_observation_id: RemoteReadObservationId,
    binding_sequence: u64,
    read_observation_sequence: u64,
) -> Result<(), String> {
    if ready.remote_conversation_id.as_str() != remote_conversation_id.as_str() {
        return Err(format!(
            "remote mirror snapshot at sequence {sequence} has remote identity {:?}, expected {:?}",
            remote_conversation_id.as_str(),
            ready.remote_conversation_id.as_str()
        ));
    }
    if ready.protocol_revision.as_str() != protocol_revision.as_str() {
        return Err(format!(
            "remote mirror snapshot at sequence {sequence} has protocol revision {:?}, expected {:?}",
            protocol_revision.as_str(),
            ready.protocol_revision.as_str()
        ));
    }
    if ready.read_observation_id != read_observation_id {
        return Err(format!(
            "remote mirror snapshot at sequence {sequence} references read observation {}, expected {}",
            read_observation_id, ready.read_observation_id
        ));
    }
    if ready.binding_sequence != binding_sequence {
        return Err(format!(
            "remote mirror snapshot at sequence {sequence} references binding sequence {binding_sequence}, expected {}",
            ready.binding_sequence
        ));
    }
    if ready.read_observation_sequence != read_observation_sequence {
        return Err(format!(
            "remote mirror snapshot at sequence {sequence} references read sequence {read_observation_sequence}, expected {}",
            ready.read_observation_sequence
        ));
    }
    Ok(())
}

fn encode_snapshot_payload(
    record: &RemoteConversationSnapshotAuditRecord,
) -> std::io::Result<String> {
    serde_json::to_string(&json!({
        "schema": REMOTE_MIRROR_SNAPSHOT_SCHEMA,
        "version": REMOTE_MIRROR_SNAPSHOT_VERSION,
        "record": "remote_conversation_snapshot_imported",
        "local_conversation_id": record.local_conversation_id.to_string(),
        "remote_conversation_id": record.remote_conversation_id.as_str(),
        "protocol_revision": record.protocol_revision.as_str(),
        "read_observation_id": record.read_observation_id.to_string(),
        "binding_sequence": record.binding_sequence,
        "read_observation_sequence": record.read_observation_sequence,
        "body": record.raw_body,
    }))
    .map_err(invalid_data)
}

fn same_snapshot(
    left: &RemoteConversationSnapshotAuditRecord,
    right: &RemoteConversationSnapshotAuditRecord,
) -> bool {
    left.local_conversation_id == right.local_conversation_id
        && left.remote_conversation_id == right.remote_conversation_id
        && left.protocol_revision == right.protocol_revision
        && left.read_observation_id == right.read_observation_id
        && left.binding_sequence == right.binding_sequence
        && left.read_observation_sequence == right.read_observation_sequence
        && left.raw_body == right.raw_body
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed remote conversation snapshot payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(REMOTE_MIRROR_SNAPSHOT_SCHEMA) {
        return Err(format!(
            "remote conversation snapshot at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "remote conversation snapshot at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != REMOTE_MIRROR_SNAPSHOT_VERSION {
        return Err(format!(
            "unsupported remote conversation snapshot version {version} at sequence {}",
            event.sequence
        ));
    }
    if required_string(&value, "record")? != "remote_conversation_snapshot_imported" {
        return Err(format!(
            "remote conversation snapshot at sequence {} has wrong record kind",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    local_conversation_id: LocalConversationId,
) -> Result<(), String> {
    let expected = remote_conversation_snapshot_scope(local_conversation_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "remote conversation snapshot at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed remote conversation snapshot is missing string field '{field}'")
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed remote conversation snapshot is missing integer field '{field}'")
    })
}

fn invalid_data(error: impl fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::remote_identity_audit::record_remote_conversation_bound;
    use crate::remote_mirror_selection_audit::record_remote_mirror_selection_changed;
    use crate::remote_read_audit::record_remote_read_observation;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::remote::RemoteConversationBinding;
    use chatarium_protocol::read::{JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const REVISION: &str = "2026-10-01.001";
    const REMOTE: &str = "fixture-id-1";

    fn materialized_fixture() -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/2026-10-01.001/c02-open-conversation.json"
        ))
        .expect("fixture JSON");
        let body = fixture
            .pointer("/read_responses/0/body")
            .expect("fixture body")
            .clone();

        fn materialize(value: Value) -> Value {
            match value {
                Value::Object(map) => Value::Object(
                    map.into_iter()
                        .map(|(key, value)| (key, materialize(value)))
                        .collect(),
                ),
                Value::Array(items) => Value::Array(items.into_iter().map(materialize).collect()),
                Value::String(value) if value == "<empty-string>" => Value::String(String::new()),
                Value::String(value) if value == "<redacted-text>" => {
                    Value::String("fixture-redacted-text".to_owned())
                }
                Value::String(value) if value == "<string>" => {
                    Value::String("fixture-string".to_owned())
                }
                Value::String(value) if value == "<number>" => json!(1.0),
                Value::String(value) if value == "<bool>" => json!(true),
                Value::String(value) if value == "<url>" => {
                    Value::String("https://example.invalid/".to_owned())
                }
                Value::String(value) if value.starts_with("<id:") && value.ends_with('>') => {
                    let id = value.trim_start_matches("<id:").trim_end_matches('>');
                    Value::String(format!("fixture-id-{id}"))
                }
                other => other,
            }
        }

        materialize(body)
    }

    fn ready_store(
        selected: bool,
        revision: &str,
        remote: &str,
    ) -> (
        MemoryEventStore,
        LocalConversationId,
        RemoteReadObservationId,
    ) {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new(remote).unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        if selected {
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        }
        let read_observation_id = RemoteReadObservationId::new();
        let observation = ReadObservation::new(
            revision,
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();
        record_remote_read_observation(&mut store, read_observation_id, &observation).unwrap();
        (store, local, read_observation_id)
    }

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-mirror-snapshot-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn imports_validated_c02_into_typed_durable_snapshot() {
        let (mut store, local, read_observation_id) = ready_store(true, REVISION, REMOTE);
        let body = materialized_fixture();

        let result = import_validated_remote_conversation_snapshot(&mut store, local, &body)
            .expect("import");
        assert!(result.appended);

        let records = replay_remote_conversation_snapshot_audit(store.events()).expect("replay");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.local_conversation_id, local);
        assert_eq!(record.remote_conversation_id.as_str(), REMOTE);
        assert_eq!(record.protocol_revision.as_str(), REVISION);
        assert_eq!(record.read_observation_id, read_observation_id);
        assert_eq!(record.envelope.messages.len(), 5);
        assert_eq!(record.envelope.current_node, "fixture-id-10");
        assert_eq!(
            record.envelope.messages[2].parent_id.as_deref(),
            Some("fixture-id-7")
        );
        assert_eq!(
            record.envelope.messages[4].parent_id.as_deref(),
            Some("fixture-id-8")
        );
        assert_eq!(record.raw_body, body);
    }

    #[test]
    fn duplicate_import_is_idempotent_across_real_journal_reopen() {
        let path = temp_path("reopen", "jsonl");
        let body = materialized_fixture();
        let local;
        let first_sequence;

        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            local = LocalConversationId::new();
            let binding = RemoteConversationBinding::new(
                local,
                RemoteConversationId::new(REMOTE).unwrap(),
                ProtocolObservationRevision::new(REVISION).unwrap(),
            );
            record_remote_conversation_bound(&mut store, &binding).unwrap();
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
            let observation = ReadObservation::new(
                REVISION,
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/conversations/<id>",
                vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            )
            .unwrap();
            record_remote_read_observation(
                &mut store,
                RemoteReadObservationId::new(),
                &observation,
            )
            .unwrap();

            let first =
                import_validated_remote_conversation_snapshot(&mut store, local, &body).unwrap();
            assert!(first.appended);
            first_sequence = first.sequence;
        }

        let mut reopened = JsonlEventStore::open(&path).unwrap();
        let second =
            import_validated_remote_conversation_snapshot(&mut reopened, local, &body).unwrap();
        assert!(!second.appended);
        assert_eq!(second.sequence, first_sequence);
        assert_eq!(
            reopened
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::RemoteConversationSnapshotImported)
                .count(),
            1
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn identity_mismatch_is_rejected_before_persistence() {
        let (mut store, local, _) = ready_store(true, REVISION, "different-remote");
        let before = store.events().len();

        assert!(matches!(
            import_validated_remote_conversation_snapshot(
                &mut store,
                local,
                &materialized_fixture()
            ),
            Err(RemoteConversationSnapshotImportError::Parse(
                ConversationFetchParseError::IdentityMismatch { .. }
            ))
        ));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn malformed_response_is_rejected_before_persistence() {
        let (mut store, local, _) = ready_store(true, REVISION, REMOTE);
        let mut body = materialized_fixture();
        body.as_object_mut().unwrap().remove("messages");
        let before = store.events().len();

        assert!(matches!(
            import_validated_remote_conversation_snapshot(&mut store, local, &body),
            Err(RemoteConversationSnapshotImportError::Parse(
                ConversationFetchParseError::MissingField(field)
            )) if field == "messages"
        ));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn unselected_conversation_cannot_import() {
        let (mut store, local, _) = ready_store(false, REVISION, REMOTE);
        let before = store.events().len();

        assert!(matches!(
            import_validated_remote_conversation_snapshot(
                &mut store,
                local,
                &materialized_fixture()
            ),
            Err(RemoteConversationSnapshotImportError::Unselected)
        ));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn stale_unvalidated_revision_remains_protocol_blocked() {
        let (mut store, local, _) = ready_store(true, "future-c02-observation", REMOTE);
        let before = store.events().len();

        assert!(matches!(
            import_validated_remote_conversation_snapshot(
                &mut store,
                local,
                &materialized_fixture()
            ),
            Err(RemoteConversationSnapshotImportError::ProtocolBlocked(_))
        ));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn sqlite_projection_rebuild_preserves_snapshot_as_replayable_journal_state() {
        let (mut store, local, _) = ready_store(true, REVISION, REMOTE);
        import_validated_remote_conversation_snapshot(&mut store, local, &materialized_fixture())
            .unwrap();

        let path = temp_path("projection", "sqlite3");
        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        let projected_events = projection.events().unwrap();
        let records = replay_remote_conversation_snapshot_audit(&projected_events).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].local_conversation_id, local);
        assert_eq!(records[0].envelope.messages.len(), 5);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_rejects_tampered_provenance() {
        let (mut store, local, _) = ready_store(true, REVISION, REMOTE);
        let ready = require_ready_execution_plan(store.events(), local).expect("ready");
        let body = materialized_fixture();
        let payload = serde_json::to_string(&json!({
            "schema": REMOTE_MIRROR_SNAPSHOT_SCHEMA,
            "version": REMOTE_MIRROR_SNAPSHOT_VERSION,
            "record": "remote_conversation_snapshot_imported",
            "local_conversation_id": local.to_string(),
            "remote_conversation_id": REMOTE,
            "protocol_revision": REVISION,
            "read_observation_id": ready.read_observation_id.to_string(),
            "binding_sequence": ready.binding_sequence + 1,
            "read_observation_sequence": ready.read_observation_sequence,
            "body": body,
        }))
        .unwrap();
        store
            .append_scoped(
                Some(remote_conversation_snapshot_scope(local)),
                EventKind::RemoteConversationSnapshotImported,
                payload,
            )
            .unwrap();

        let error = replay_remote_conversation_snapshot_audit(store.events()).unwrap_err();
        assert!(error.contains("binding sequence"));
    }
}
