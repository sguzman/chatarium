//! Replayable audit of quarantine decisions, without erasing or suppressing journal evidence.
//!
//! Recording a decision is not applying it. The authoritative journal remains complete;
//! effective-event projection and exclusive-writer admission are separate acceptance gates.

use crate::EventEnvelope;
use chatarium_core::EventKind;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "chatarium-source-quarantine-audit";
const VERSION: u64 = 1;
const SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";

/// Immutable provenance and exact event span for a source-quarantine decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSourceQuarantineRecord {
    /// SHA-256 of the source's content-addressed archive.
    pub source_sha256: String,
    /// SHA-256 of the original journal bytes at preflight.
    pub preflight_journal_sha256: String,
    /// Count of source-owned journal events verified at decision time.
    pub matching_event_count: u64,
    /// First source-owned journal sequence (import start).
    pub first_sequence: u64,
    /// Last source-owned journal sequence (import completion).
    pub last_sequence: u64,
    /// Durable sequence of the quarantine decision itself.
    pub decision_sequence: u64,
}

/// Validate and replay recorded source quarantine decisions.
///
/// These facts are non-operative until a separately verified effective-event
/// projection and exclusive-writer apply path have been implemented.
pub fn replay_import_source_quarantine_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ImportSourceQuarantineRecord>, String> {
    let mut decisions = Vec::new();
    let mut decided_sources = BTreeSet::new();
    let mut protected_scopes = BTreeMap::<String, u64>::new();

    for (position, event) in events.iter().enumerate() {
        if event.kind != EventKind::ImportSourceQuarantineRecorded {
            continue;
        }
        if event.scope.is_some() {
            return Err(format!(
                "quarantine decision at sequence {} must be unscoped",
                event.sequence
            ));
        }
        let payload: Value = serde_json::from_str(&event.payload)
            .map_err(|_| format!("malformed quarantine audit at sequence {}", event.sequence))?;
        if payload.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || payload.get("version").and_then(Value::as_u64) != Some(VERSION)
            || payload.get("record").and_then(Value::as_str) != Some("source_quarantine_recorded")
            || payload.get("source_format").and_then(Value::as_str) != Some(SOURCE_FORMAT)
        {
            return Err(format!(
                "unsupported quarantine audit schema at sequence {}",
                event.sequence
            ));
        }
        let source_sha256 = required_hash(&payload, "source_sha256")?;
        let preflight_journal_sha256 = required_hash(&payload, "preflight_journal_sha256")?;
        let matching_event_count = required_u64(&payload, "matching_event_count")?;
        let first_sequence = required_u64(&payload, "first_sequence")?;
        let last_sequence = required_u64(&payload, "last_sequence")?;
        if !decided_sources.insert(source_sha256.clone()) {
            return Err("duplicate source quarantine decision".to_owned());
        }
        if first_sequence == 0 || last_sequence < first_sequence || last_sequence >= event.sequence
        {
            return Err("invalid source quarantine event sequence range".to_owned());
        }

        // The decision may not claim arbitrary/forged source events. Reconstruct
        // its exact original source span from the immutable preceding evidence.
        let mut matches = Vec::new();
        let mut scopes = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for original in &events[..position] {
            let Some(source) = source_provenance(original) else {
                continue;
            };
            if source != source_sha256 {
                continue;
            }
            if !valid_import_kind(original.kind) {
                return Err("unrecognized event kind claims quarantined source".to_owned());
            }
            let value: Value = serde_json::from_str(&original.payload)
                .map_err(|_| "malformed source event in quarantine evidence".to_owned())?;
            let key = value
                .pointer("/source/event_key")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing event key in source evidence".to_owned())?;
            if !keys.insert(key.to_owned()) {
                return Err("duplicated imported source event key".to_owned());
            }
            if let Some(scope) = original.scope.as_deref() {
                scopes.insert(scope);
            }
            matches.push(original);
        }
        if matches.len() as u64 != matching_event_count
            || matches.first().map(|record| record.sequence) != Some(first_sequence)
            || matches.last().map(|record| record.sequence) != Some(last_sequence)
            || matches.first().map(|record| record.kind) != Some(EventKind::ImportStarted)
            || matches.last().map(|record| record.kind) != Some(EventKind::ImportCompleted)
        {
            return Err("quarantine decision does not match complete import evidence".to_owned());
        }
        if events[..position].iter().any(|original| {
            original
                .scope
                .as_deref()
                .is_some_and(|scope| scopes.contains(scope))
                && source_provenance(original).as_deref() != Some(source_sha256.as_str())
        }) {
            return Err("foreign event shares quarantine source scope".to_owned());
        }
        if matching_event_count < 2 {
            return Err("quarantine source has no complete import framing".to_owned());
        }

        let first: Value = serde_json::from_str(&matches[0].payload)
            .map_err(|_| "malformed import start".to_owned())?;
        let last: Value = serde_json::from_str(&matches[matches.len() - 1].payload)
            .map_err(|_| "malformed import completion".to_owned())?;
        if matches[0].scope.is_some()
            || matches[matches.len() - 1].scope.is_some()
            || first.pointer("/source/event_key").and_then(Value::as_str) != Some("import:start")
            || last.pointer("/source/event_key").and_then(Value::as_str) != Some("import:complete")
            || last
                .pointer("/details/semantic_event_count")
                .and_then(Value::as_u64)
                != Some(matching_event_count - 2)
        {
            return Err("invalid quarantine import framing or semantic count".to_owned());
        }
        let archive = format!("imports/flight-recorder/{source_sha256}.json");
        if first.pointer("/details/archive").and_then(Value::as_str) != Some(archive.as_str()) {
            return Err("source quarantine archive provenance mismatch".to_owned());
        }

        for scope in scopes {
            if protected_scopes
                .insert(scope.to_owned(), event.sequence)
                .is_some()
            {
                return Err("quarantine sources share an event scope".to_owned());
            }
        }

        decisions.push(ImportSourceQuarantineRecord {
            source_sha256,
            preflight_journal_sha256,
            matching_event_count,
            first_sequence,
            last_sequence,
            decision_sequence: event.sequence,
        });
    }

    // A later importer cannot silently restore a source behind an existing decision.
    let decisions_by_sha = decisions
        .iter()
        .map(|record| (record.source_sha256.as_str(), record.decision_sequence))
        .collect::<BTreeMap<_, _>>();
    for event in events {
        if let Some(source) = source_provenance(event) {
            if decisions_by_sha
                .get(source.as_str())
                .is_some_and(|decision_sequence| event.sequence > *decision_sequence)
            {
                return Err("source re-imported after quarantine decision".to_owned());
            }
        }
        if event.scope.as_deref().is_some_and(|scope| {
            protected_scopes
                .get(scope)
                .is_some_and(|decision_sequence| event.sequence > *decision_sequence)
        }) {
            return Err("quarantined source scope reused after decision".to_owned());
        }
    }

    Ok(decisions)
}

fn required_hash(value: &Value, key: &str) -> Result<String, String> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing quarantine hash field {key}"))?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid quarantine SHA-256 field {key}"));
    }
    Ok(text.to_owned())
}

fn required_u64(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing quarantine integer field {key}"))
}

fn source_provenance(event: &EventEnvelope) -> Option<String> {
    let value: Value = serde_json::from_str(&event.payload).ok()?;
    if value.get("schema")?.as_str()? != "chatarium-imported-observation"
        || value.pointer("/source/format")?.as_str()? != SOURCE_FORMAT
    {
        return None;
    }
    value
        .pointer("/source/sha256")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn valid_import_kind(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::ImportStarted
            | EventKind::ImportCompleted
            | EventKind::DraftChanged
            | EventKind::UserMessageCommitted
            | EventKind::DispatchAttempted
            | EventKind::RemoteAcceptanceObserved
            | EventKind::TranscriptUserMessageObserved
            | EventKind::AssistantSnapshotObserved
            | EventKind::AssistantStatusObserved
            | EventKind::AssistantCompletionObserved
            | EventKind::ClientErrorObserved
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, MemoryEventStore};
    use serde_json::json;

    fn hash() -> String {
        "a".repeat(64)
    }

    fn source(key: &str, count: Option<u64>) -> String {
        let hash = hash();
        let mut details = json!({"archive": format!("imports/flight-recorder/{hash}.json")});
        if let Some(count) = count {
            details["semantic_event_count"] = json!(count);
        }
        json!({
            "schema": "chatarium-imported-observation",
            "source": {"format": SOURCE_FORMAT, "sha256": hash, "event_key": key},
            "details": details
        })
        .to_string()
    }

    fn decision(hash: &str, count: u64) -> String {
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "source_quarantine_recorded",
            "source_format": SOURCE_FORMAT,
            "source_sha256": hash,
            "preflight_journal_sha256": "b".repeat(64),
            "matching_event_count": count,
            "first_sequence": 1,
            "last_sequence": 3
        })
        .to_string()
    }

    fn valid() -> MemoryEventStore {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::ImportStarted, source("import:start", None))
            .unwrap();
        store
            .append_scoped(
                Some("remote:safe".to_owned()),
                EventKind::DraftChanged,
                source("draft:1", None),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportCompleted,
                source("import:complete", Some(1)),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportSourceQuarantineRecorded,
                decision(&hash(), 3),
            )
            .unwrap();
        store
    }

    #[test]
    fn decision_preserves_original_source_evidence_on_replay() {
        let store = valid();
        let decision = replay_import_source_quarantine_audit(store.events()).unwrap();
        assert_eq!(decision.len(), 1);
        assert_eq!(decision[0].decision_sequence, 4);
        assert_eq!(decision[0].matching_event_count, 3);
        assert_eq!(store.events().len(), 4);
    }

    #[test]
    fn rejects_duplicate_decision_and_reimport_after_decision() {
        let mut store = valid();
        store
            .append(
                EventKind::ImportSourceQuarantineRecorded,
                decision(&hash(), 3),
            )
            .unwrap();
        assert!(replay_import_source_quarantine_audit(store.events()).is_err());
        let mut store = valid();
        store
            .append(EventKind::DraftChanged, source("draft:2", None))
            .unwrap();
        assert!(replay_import_source_quarantine_audit(store.events()).is_err());
    }

    #[test]
    fn refuses_forged_scope_and_foreign_scope_ownership() {
        let mut store = valid();
        store
            .append_scoped(
                Some("remote:safe".to_owned()),
                EventKind::DraftChanged,
                "native".to_owned(),
            )
            .unwrap();
        assert!(replay_import_source_quarantine_audit(store.events()).is_err());
        let mut store = MemoryEventStore::default();
        store
            .append(
                EventKind::ImportSourceQuarantineRecorded,
                decision(&hash(), 3),
            )
            .unwrap();
        assert!(replay_import_source_quarantine_audit(store.events()).is_err());
    }

    #[test]
    fn invalid_hash_or_count_fails_closed() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::ImportStarted, source("import:start", None))
            .unwrap();
        store
            .append_scoped(
                Some("remote:safe".to_owned()),
                EventKind::DraftChanged,
                source("draft:1", None),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportCompleted,
                source("import:complete", Some(1)),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportSourceQuarantineRecorded,
                decision(&hash(), 4),
            )
            .unwrap();
        assert!(replay_import_source_quarantine_audit(store.events()).is_err());
    }
}
