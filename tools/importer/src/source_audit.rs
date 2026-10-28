//! Read-only attribution of imported evidence to an exact source SHA-256.
//!
//! This is a preflight/reporting boundary, not a quarantine implementation.
//! It deliberately makes no changes to the authoritative append-only journal or
//! any rebuildable projection. Source matches are evidence for human/automated
//! review, not permission to discard events.

use chatarium_core::EventKind;
use chatarium_store::{EventEnvelope, inspect_jsonl_journal};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct SourceAudit {
    pub source_sha256: String,
    pub journal_events: usize,
    pub matching_events: usize,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub source_types: BTreeMap<&'static str, usize>,
    pub event_kinds: BTreeMap<String, usize>,
}

pub fn audit_source(data_dir: &Path, source_sha256: &str) -> Result<SourceAudit, String> {
    if source_sha256.len() != 64 || !source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("source SHA-256 must be exactly 64 hexadecimal characters".to_owned());
    }
    if !data_dir.is_dir() {
        return Err(format!(
            "explicit existing data directory is required: {}",
            data_dir.display()
        ));
    }
    let journal = data_dir.join("journal.jsonl");
    if !journal.is_file() {
        return Err(format!("journal does not exist: {}", journal.display()));
    }
    // Never open via JsonlEventStore::open here: it can repair a torn tail.
    let inspection = inspect_jsonl_journal(&journal)
        .map_err(|error| format!("inspect {}: {error}", journal.display()))?;
    if inspection.unterminated_tail_bytes != 0 {
        return Err(format!(
            "journal has {} unterminated tail bytes; source audit refused without modifying it",
            inspection.unterminated_tail_bytes
        ));
    }
    let normalized = source_sha256.to_ascii_lowercase();
    audit_events(&inspection.events, &normalized)
}

fn audit_events(events: &[EventEnvelope], source_sha256: &str) -> Result<SourceAudit, String> {
    let mut source_types = BTreeMap::new();
    let mut event_kinds = BTreeMap::new();
    let mut first_sequence = 0;
    let mut last_sequence = 0;
    let mut matching_events = 0;
    for event in events {
        if let Some(source_type) = matched_source(event, source_sha256) {
            *source_types.entry(source_type).or_insert(0) += 1;
            *event_kinds
                .entry(event.kind.stable_name().to_owned())
                .or_insert(0) += 1;
            if matching_events == 0 {
                first_sequence = event.sequence;
            }
            last_sequence = event.sequence;
            matching_events += 1;
        }
    }
    if matching_events == 0 {
        return Err(format!(
            "no recognized imported events match source SHA-256 {source_sha256}"
        ));
    }
    Ok(SourceAudit {
        source_sha256: source_sha256.to_owned(),
        journal_events: events.len(),
        matching_events,
        first_sequence,
        last_sequence,
        source_types,
        event_kinds,
    })
}

pub(crate) fn matched_source(event: &EventEnvelope, source_sha256: &str) -> Option<&'static str> {
    let payload: Value = serde_json::from_str(&event.payload).ok()?;
    let schema = payload.get("schema")?.as_str()?;
    match event.kind {
        EventKind::HistoricalConversationSnapshotImported
            if schema == "chatarium-historical-conversation-snapshot"
                && payload.get("source_sha256")?.as_str()? == source_sha256 =>
        {
            Some("openai-account-export")
        }
        EventKind::RemoteReadObservationRecorded
            if schema == "chatarium-remote-read-observation"
                && payload.pointer("/provenance/source_kind")?.as_str()?
                    == "sanitized_read_fixture"
                && payload.pointer("/provenance/source_sha256")?.as_str()? == source_sha256 =>
        {
            Some("read-fixture")
        }
        _ if schema == "chatarium-imported-observation"
            && payload.pointer("/source/format")?.as_str()?
                == "chatarium-flight-recorder-export"
            && payload.pointer("/source/sha256")?.as_str()? == source_sha256 =>
        {
            Some("flight-recorder")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_store::{EventStore, JsonlEventStore, MemoryEventStore};
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn hash() -> String {
        "a".repeat(64)
    }

    fn flight_payload(source: &str) -> String {
        json!({
            "schema": "chatarium-imported-observation",
            "version": 1,
            "source": {
                "format": "chatarium-flight-recorder-export",
                "sha256": source,
                "event_key": "import:start"
            }
        })
        .to_string()
    }

    #[test]
    fn counts_only_source_owned_evidence_without_leaking_payloads() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::ImportStarted, flight_payload(&hash()))
            .unwrap();
        store
            .append(EventKind::DraftChanged, "native authored text".to_owned())
            .unwrap();
        store
            .append(
                EventKind::HistoricalConversationSnapshotImported,
                json!({
                    "schema": "chatarium-historical-conversation-snapshot",
                    "source_sha256": hash()
                })
                .to_string(),
            )
            .unwrap();
        store
            .append(
                EventKind::RemoteReadObservationRecorded,
                json!({
                    "schema": "chatarium-remote-read-observation",
                    "provenance": {
                        "source_kind": "sanitized_read_fixture",
                        "source_sha256": hash()
                    }
                })
                .to_string(),
            )
            .unwrap();
        store
            .append(EventKind::ImportCompleted, flight_payload(&"b".repeat(64)))
            .unwrap();

        let report = audit_events(store.events(), &hash()).unwrap();
        assert_eq!(report.journal_events, 5);
        assert_eq!(report.matching_events, 3);
        assert_eq!((report.first_sequence, report.last_sequence), (1, 4));
        assert_eq!(report.source_types["flight-recorder"], 1);
        assert_eq!(report.source_types["openai-account-export"], 1);
        assert_eq!(report.source_types["read-fixture"], 1);
        assert!(!format!("{report:?}").contains("native authored text"));
    }

    #[test]
    fn rejects_unrecognized_hash_and_provenance() {
        let mut store = MemoryEventStore::default();
        store
            .append(
                EventKind::DraftChanged,
                json!({
                    "schema": "chatarium-historical-conversation-snapshot",
                    "source_sha256": hash()
                })
                .to_string(),
            )
            .unwrap();
        assert!(audit_events(store.events(), &hash()).is_err());
        assert!(audit_events(store.events(), &"b".repeat(64)).is_err());
    }

    #[test]
    fn audit_never_repairs_or_mutates_journal() {
        let root = std::env::temp_dir().join(format!(
            "chatarium-source-audit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let journal = root.join("journal.jsonl");
        let mut store = JsonlEventStore::open(&journal).unwrap();
        store
            .append(EventKind::ImportStarted, flight_payload(&hash()))
            .unwrap();
        drop(store);

        let before = fs::read(&journal).unwrap();
        let report = audit_source(&root, &hash().to_ascii_uppercase()).unwrap();
        assert_eq!(report.matching_events, 1);
        assert_eq!(fs::read(&journal).unwrap(), before);
        assert!(audit_source(&root, "invalid").is_err());

        let mut torn = before.clone();
        torn.extend_from_slice(b"unfinished");
        fs::write(&journal, &torn).unwrap();
        assert!(audit_source(&root, &hash()).is_err());
        assert_eq!(fs::read(&journal).unwrap(), torn);

        fs::write(&journal, b"not valid JSON\n").unwrap();
        assert!(audit_source(&root, &hash()).is_err());
        assert_eq!(fs::read(&journal).unwrap(), b"not valid JSON\n");
        fs::remove_dir_all(root).unwrap();
    }
}
