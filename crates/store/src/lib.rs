//! Durable-storage boundary for Chatarium.
//!
//! The first persistent substrate is an append-only JSON-lines journal. SQLite projections can
//! be added later without making mutable projection state authoritative over the event history.

use chatarium_core::EventKind;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_WRITE_VERSION: u64 = 2;

/// One durable local event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    /// Monotonic local sequence assigned by a concrete store.
    pub sequence: u64,
    /// Local observation time in Unix milliseconds.
    pub at_unix_ms: u64,
    /// Optional conversation/scope identifier.
    pub scope: Option<String>,
    /// Semantic event kind.
    pub kind: EventKind,
    /// Exact textual or JSON payload when applicable.
    pub payload: String,
}

/// Append/read contract required by the application core.
pub trait EventStore {
    /// Store one scoped event and return its durable sequence.
    ///
    /// Persistent implementations must not return success until the record has crossed their
    /// durability boundary.
    fn append_scoped(
        &mut self,
        scope: Option<String>,
        kind: EventKind,
        payload: String,
    ) -> io::Result<u64>;

    /// Store one unscoped event and return its durable sequence.
    fn append(&mut self, kind: EventKind, payload: String) -> io::Result<u64> {
        self.append_scoped(None, kind, payload)
    }

    /// Return events in durable sequence order.
    fn events(&self) -> &[EventEnvelope];
}

/// In-memory reference store used by unit tests and non-durable experiments.
#[derive(Debug, Default)]
pub struct MemoryEventStore {
    events: Vec<EventEnvelope>,
}

impl EventStore for MemoryEventStore {
    fn append_scoped(
        &mut self,
        scope: Option<String>,
        kind: EventKind,
        payload: String,
    ) -> io::Result<u64> {
        let sequence = next_sequence(&self.events)?;
        self.events.push(EventEnvelope {
            sequence,
            at_unix_ms: unix_ms()?,
            scope,
            kind,
            payload,
        });
        Ok(sequence)
    }

    fn events(&self) -> &[EventEnvelope] {
        &self.events
    }
}

/// Append-only JSON-lines event store.
///
/// Each successful append writes exactly one newline-terminated JSON record, flushes the file,
/// and calls [`File::sync_data`] before updating the in-memory projection. On open, an
/// unterminated final fragment is treated as a torn last write and truncated. Malformed complete
/// records remain hard errors.
///
/// Journal v2 adds the optional `scope` field. V1 records remain readable and are projected as
/// unscoped events.
#[derive(Debug)]
pub struct JsonlEventStore {
    path: PathBuf,
    file: File,
    events: Vec<EventEnvelope>,
}

impl JsonlEventStore {
    /// Open or create a journal and rebuild its in-memory event projection.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let mut recovery_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let mut bytes = Vec::new();
        recovery_file.read_to_end(&mut bytes)?;

        let complete_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        if complete_len < bytes.len() {
            recovery_file.set_len(u64::try_from(complete_len).map_err(invalid_data)?)?;
            recovery_file.sync_data()?;
            bytes.truncate(complete_len);
        }
        drop(recovery_file);

        let text = std::str::from_utf8(&bytes).map_err(invalid_data)?;
        let mut events = Vec::new();
        for (line_index, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            let event = decode_event(line).map_err(|error| {
                invalid_data(format!("journal line {}: {error}", line_index + 1))
            })?;
            let expected = next_sequence(&events)?;
            if event.sequence != expected {
                return Err(invalid_data(format!(
                    "journal line {} has sequence {}, expected {expected}",
                    line_index + 1,
                    event.sequence
                )));
            }
            events.push(event);
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file, events })
    }

    /// Filesystem path backing this journal.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl EventStore for JsonlEventStore {
    fn append_scoped(
        &mut self,
        scope: Option<String>,
        kind: EventKind,
        payload: String,
    ) -> io::Result<u64> {
        let sequence = next_sequence(&self.events)?;
        let event = EventEnvelope {
            sequence,
            at_unix_ms: unix_ms()?,
            scope,
            kind,
            payload,
        };
        let encoded = encode_event(&event)?;

        self.file.write_all(encoded.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        self.file.sync_data()?;

        self.events.push(event);
        Ok(sequence)
    }

    fn events(&self) -> &[EventEnvelope] {
        &self.events
    }
}

fn next_sequence(events: &[EventEnvelope]) -> io::Result<u64> {
    u64::try_from(events.len())
        .map_err(invalid_data)?
        .checked_add(1)
        .ok_or_else(|| invalid_data("event sequence overflow"))
}

fn unix_ms() -> io::Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(invalid_data)?
        .as_millis();
    u64::try_from(millis).map_err(invalid_data)
}

fn encode_event(event: &EventEnvelope) -> io::Result<String> {
    serde_json::to_string(&json!({
        "v": JOURNAL_WRITE_VERSION,
        "sequence": event.sequence,
        "at_unix_ms": event.at_unix_ms,
        "scope": event.scope,
        "kind": event.kind.stable_name(),
        "payload": event.payload,
    }))
    .map_err(invalid_data)
}

fn decode_event(line: &str) -> Result<EventEnvelope, String> {
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let version = value
        .get("v")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing integer field 'v'".to_owned())?;
    if !matches!(version, 1 | JOURNAL_WRITE_VERSION) {
        return Err(format!("unsupported journal version {version}"));
    }

    let sequence = value
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing integer field 'sequence'".to_owned())?;
    let at_unix_ms = value
        .get("at_unix_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing integer field 'at_unix_ms'".to_owned())?;
    let scope = if version >= 2 {
        match value.get("scope") {
            None | Some(Value::Null) => None,
            Some(Value::String(scope)) => Some(scope.clone()),
            Some(_) => return Err("field 'scope' must be string or null".to_owned()),
        }
    } else {
        None
    };
    let kind_name = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string field 'kind'".to_owned())?;
    let kind = EventKind::from_stable_name(kind_name)
        .ok_or_else(|| format!("unknown event kind '{kind_name}'"))?;
    let payload = value
        .get("payload")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string field 'payload'".to_owned())?
        .to_owned();

    Ok(EventEnvelope {
        sequence,
        at_unix_ms,
        scope,
        kind,
        payload,
    })
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-store-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn memory_store_assigns_monotonic_sequences() {
        let mut store = MemoryEventStore::default();
        assert_eq!(
            store
                .append(EventKind::DraftChanged, "one".to_owned())
                .expect("append"),
            1
        );
        assert_eq!(
            store
                .append(EventKind::DraftChanged, "two".to_owned())
                .expect("append"),
            2
        );
    }

    #[test]
    fn jsonl_store_round_trips_scoped_events() {
        let path = temp_path("round-trip");
        {
            let mut store = JsonlEventStore::open(&path).expect("open");
            store
                .append_scoped(
                    Some("conversation:test".to_owned()),
                    EventKind::DraftChanged,
                    "draft text".to_owned(),
                )
                .expect("draft append");
            store
                .append(EventKind::UserMessageCommitted, "exact send".to_owned())
                .expect("message append");
        }

        let reopened = JsonlEventStore::open(&path).expect("reopen");
        assert_eq!(reopened.events().len(), 2);
        assert_eq!(reopened.events()[0].kind, EventKind::DraftChanged);
        assert_eq!(
            reopened.events()[0].scope.as_deref(),
            Some("conversation:test")
        );
        assert_eq!(reopened.events()[0].payload, "draft text");
        assert_eq!(reopened.events()[1].kind, EventKind::UserMessageCommitted);
        assert_eq!(reopened.events()[1].scope, None);
        assert_eq!(reopened.events()[1].payload, "exact send");
        assert_eq!(reopened.events()[0].sequence, 1);
        assert_eq!(reopened.events()[1].sequence, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn v1_records_remain_readable_as_unscoped() {
        let path = temp_path("v1");
        fs::write(
            &path,
            b"{\"v\":1,\"sequence\":1,\"at_unix_ms\":1,\"kind\":\"draft_changed\",\"payload\":\"legacy\"}\n",
        )
        .expect("write v1");

        let reopened = JsonlEventStore::open(&path).expect("open v1");
        assert_eq!(reopened.events().len(), 1);
        assert_eq!(reopened.events()[0].scope, None);
        assert_eq!(reopened.events()[0].payload, "legacy");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn open_truncates_only_an_unterminated_tail() {
        let path = temp_path("torn-tail");
        {
            let mut store = JsonlEventStore::open(&path).expect("open");
            store
                .append(EventKind::DraftChanged, "survives".to_owned())
                .expect("append");
        }
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open raw");
            file.write_all(br#"{"v":2,"sequence":2"#)
                .expect("partial write");
            file.sync_data().expect("sync partial");
        }

        {
            let mut recovered = JsonlEventStore::open(&path).expect("recover");
            assert_eq!(recovered.events().len(), 1);
            recovered
                .append(EventKind::DraftChanged, "after recovery".to_owned())
                .expect("append after recovery");
        }

        let reopened = JsonlEventStore::open(&path).expect("reopen");
        assert_eq!(reopened.events().len(), 2);
        assert_eq!(reopened.events()[1].sequence, 2);
        assert_eq!(reopened.events()[1].payload, "after recovery");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_record_is_not_silently_discarded() {
        let path = temp_path("malformed");
        fs::write(&path, b"{\"v\":2,\"broken\":true}\n").expect("write malformed");
        let error = JsonlEventStore::open(&path).expect_err("must reject malformed complete line");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(path);
    }
}
