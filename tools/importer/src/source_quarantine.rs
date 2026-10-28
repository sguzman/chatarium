//! Fail-closed, non-mutating preflight for source-scoped quarantine.
//!
//! Attribution alone cannot authorize removal: active readers derive from the authoritative
//! journal in more than one place. This report verifies on-disk source bytes, import framing,
//! event provenance and direct scope collisions before any future exclusion can be considered.
//! It never claims to have resolved cross-scope semantic dependencies.

use crate::source_audit::{audit_source, matched_source};
use chatarium_core::EventKind;
use chatarium_store::{EventEnvelope, inspect_jsonl_journal};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const FLIGHT_SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";
const FLIGHT_SOURCE_VERSION: u64 = 3;

#[derive(Debug, PartialEq, Eq)]
pub struct QuarantinePreflight {
    pub source_sha256: String,
    pub journal_sha256: String,
    pub journal_events: usize,
    pub matching_events: usize,
    pub matching_scope_count: usize,
    pub source_bytes: usize,
    pub blockers: Vec<&'static str>,
}

impl QuarantinePreflight {
    pub fn candidate_isolated(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Read and validate an existing archive. Never create a directory, open for write,
/// truncate a torn journal, or touch a disposable projection.
pub fn preflight(data_dir: &Path, source_sha256: &str) -> Result<QuarantinePreflight, String> {
    let attribution = audit_source(data_dir, source_sha256)?;
    let hash = attribution.source_sha256;
    let path = data_dir.join("journal.jsonl");
    let before =
        fs::read(&path).map_err(|error| format!("read journal for source preflight: {error}"))?;
    let inspection = inspect_jsonl_journal(&path)
        .map_err(|error| format!("inspect journal for source preflight: {error}"))?;
    if inspection.unterminated_tail_bytes != 0 {
        return Err("quarantine preflight refuses a torn journal tail".to_owned());
    }
    let after = fs::read(&path)
        .map_err(|error| format!("recheck journal for source preflight: {error}"))?;
    if before != after {
        return Err("journal changed during quarantine preflight".to_owned());
    }
    let journal_sha256 = hash_bytes(&after);

    let kinds = inspection
        .events
        .iter()
        .filter_map(|event| matched_source(event, &hash))
        .collect::<BTreeSet<_>>();
    let mut blockers = Vec::new();
    let source_bytes = if kinds.len() != 1 || !kinds.contains("flight-recorder") {
        blockers.push("unsupported_or_mixed_source_types");
        0
    } else {
        let source_path = data_dir
            .join("imports")
            .join("flight-recorder")
            .join(format!("{hash}.json"));
        let source = fs::read(source_path).map_err(|_| {
            "source archive missing or unreadable; cannot validate quarantine".to_owned()
        })?;
        if hash_bytes(&source) != hash {
            return Err("source archive SHA-256 mismatch; quarantine refused".to_owned());
        }
        let parsed: Value = serde_json::from_slice(&source)
            .map_err(|_| "source archive contains malformed JSON".to_owned())?;
        if parsed.get("format").and_then(Value::as_str) != Some(FLIGHT_SOURCE_FORMAT)
            || parsed.get("version").and_then(Value::as_u64) != Some(FLIGHT_SOURCE_VERSION)
        {
            return Err("source archive has an unsupported format or version".to_owned());
        }
        source.len()
    };

    let matching = inspection
        .events
        .iter()
        .filter(|event| matched_source(event, &hash).is_some())
        .collect::<Vec<_>>();
    let scopes = matching
        .iter()
        .filter_map(|event| event.scope.as_deref())
        .collect::<BTreeSet<_>>();

    // Unrecognized claims to the same source must not be silently ignored. The
    // source audit intentionally counts only recognized importer formats; the
    // preflight also inspects known provenance shapes for impostors/corruption.
    if inspection
        .events
        .iter()
        .any(|event| claims_hash(event, &hash) && matched_source(event, &hash).is_none())
    {
        blockers.push("unrecognized_source_provenance");
    }
    if matching.iter().any(|event| !valid_flight_kind(event.kind)) {
        blockers.push("unexpected_import_event_kind");
    }

    // A different event at the same exact scope could be manually authored,
    // derived from another source, or part of a later native turn. Never assume
    // such a scope is exclusively owned by the candidate import.
    if inspection.events.iter().any(|event| {
        event
            .scope
            .as_deref()
            .is_some_and(|scope| scopes.contains(scope))
            && matched_source(event, &hash).is_none()
    }) {
        blockers.push("foreign_event_shares_import_scope");
    }

    let mut keys = BTreeSet::new();
    let mut started = 0;
    let mut completed = 0;
    let mut semantic_count = None;
    for event in &matching {
        let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
            blockers.push("malformed_import_payload");
            continue;
        };
        let Some(key) = payload.pointer("/source/event_key").and_then(Value::as_str) else {
            blockers.push("missing_import_event_key");
            continue;
        };
        if !keys.insert(key.to_owned()) {
            blockers.push("duplicate_import_event_key");
        }
        if event.kind == EventKind::ImportStarted {
            started += 1;
            if event.scope.is_some() || key != "import:start" {
                blockers.push("invalid_import_start");
            }
            let archive = format!("imports/flight-recorder/{hash}.json");
            if payload.pointer("/details/archive").and_then(Value::as_str) != Some(&archive) {
                blockers.push("import_archive_reference_mismatch");
            }
        }
        if event.kind == EventKind::ImportCompleted {
            completed += 1;
            if event.scope.is_some() || key != "import:complete" {
                blockers.push("invalid_import_completion");
            }
            semantic_count = payload
                .pointer("/details/semantic_event_count")
                .and_then(Value::as_u64);
        }
    }
    if started != 1 || completed != 1 {
        blockers.push("incomplete_or_duplicated_import_markers");
    }
    if semantic_count != Some(matching.len().saturating_sub(2) as u64) {
        blockers.push("import_semantic_count_mismatch");
    }
    if let (Some(first), Some(last)) = (matching.first(), matching.last()) {
        if first.kind != EventKind::ImportStarted || last.kind != EventKind::ImportCompleted {
            blockers.push("import_marker_order_mismatch");
        }
    }

    // Any source-backed event with a declared conversation scope is potentially
    // referenced by other projections; a successful report is only a candidate,
    // NOT authorization to change effective journal replay.
    blockers.sort_unstable();
    blockers.dedup();
    Ok(QuarantinePreflight {
        source_sha256: hash,
        journal_sha256,
        journal_events: inspection.events.len(),
        matching_events: matching.len(),
        matching_scope_count: scopes.len(),
        source_bytes,
        blockers,
    })
}

fn claims_hash(event: &EventEnvelope, source_sha256: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(&event.payload) else {
        return false;
    };
    [
        value.pointer("/source/sha256"),
        value.get("source_sha256"),
        value.pointer("/provenance/source_sha256"),
    ]
    .iter()
    .any(|candidate| candidate.and_then(Value::as_str) == Some(source_sha256))
}

fn valid_flight_kind(kind: EventKind) -> bool {
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

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_store::{EventStore, JsonlEventStore};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(label: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "chatarium-quarantine-plan-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("imports/flight-recorder")).unwrap();
        let source = json!({
            "format": FLIGHT_SOURCE_FORMAT,
            "version": FLIGHT_SOURCE_VERSION
        })
        .to_string();
        let hash = hash_bytes(source.as_bytes());
        fs::write(
            dir.join(format!("imports/flight-recorder/{hash}.json")),
            source,
        )
        .unwrap();
        let mut journal = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        journal
            .append(
                EventKind::ImportStarted,
                payload(&hash, "import:start", None),
            )
            .unwrap();
        journal
            .append_scoped(
                Some("remote:test".to_owned()),
                EventKind::DraftChanged,
                payload(&hash, "draft:1", None),
            )
            .unwrap();
        journal
            .append(
                EventKind::ImportCompleted,
                payload(&hash, "import:complete", Some(1)),
            )
            .unwrap();
        (dir, hash)
    }

    fn payload(hash: &str, key: &str, count: Option<u64>) -> String {
        let mut details = json!({"archive": format!("imports/flight-recorder/{hash}.json")});
        if let Some(count) = count {
            details["semantic_event_count"] = json!(count);
        }
        json!({
            "schema": "chatarium-imported-observation",
            "version": 1,
            "source": {
                "format": FLIGHT_SOURCE_FORMAT,
                "version": FLIGHT_SOURCE_VERSION,
                "sha256": hash,
                "event_key": key
            },
            "details": details
        })
        .to_string()
    }

    #[test]
    fn verified_isolated_source_is_read_only() {
        let (dir, hash) = fixture("valid");
        let before = fs::read(dir.join("journal.jsonl")).unwrap();
        let plan = preflight(&dir, &hash.to_ascii_uppercase()).unwrap();
        assert!(plan.candidate_isolated(), "{:?}", plan.blockers);
        assert_eq!(plan.matching_events, 3);
        assert_eq!(plan.matching_scope_count, 1);
        assert_eq!(plan.journal_events, 3);
        assert_eq!(plan.journal_sha256, hash_bytes(&before));
        assert_eq!(before, fs::read(dir.join("journal.jsonl")).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn foreign_event_at_same_scope_blocks_quarantine() {
        let (dir, hash) = fixture("foreign");
        let mut store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        store
            .append_scoped(
                Some("remote:test".to_owned()),
                EventKind::UserMessageCommitted,
                "native-user-authored".to_owned(),
            )
            .unwrap();
        drop(store);
        let plan = preflight(&dir, &hash).unwrap();
        assert!(plan.blockers.contains(&"foreign_event_shares_import_scope"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hash_collision_claim_and_truncated_tail_fail_closed() {
        let (dir, hash) = fixture("collision");
        let mut store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        store
            .append(
                EventKind::DraftChanged,
                json!({"source_sha256": hash}).to_string(),
            )
            .unwrap();
        drop(store);
        let plan = preflight(&dir, &hash).unwrap();
        assert!(plan.blockers.contains(&"unrecognized_source_provenance"));
        fs::remove_dir_all(dir).unwrap();

        let (dir, hash) = fixture("torn");
        let path = dir.join("journal.jsonl");
        let mut raw = fs::read(&path).unwrap();
        raw.extend_from_slice(b"torn");
        fs::write(&path, &raw).unwrap();
        assert!(preflight(&dir, &hash).is_err());
        assert_eq!(raw, fs::read(&path).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_tamper_and_missing_archive_fail_closed() {
        let (dir, hash) = fixture("tamper");
        let path = dir.join(format!("imports/flight-recorder/{hash}.json"));
        fs::write(&path, b"bad").unwrap();
        assert!(preflight(&dir, &hash).is_err());
        fs::remove_file(&path).unwrap();
        assert!(preflight(&dir, &hash).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_with_duplicate_or_missing_completion_is_blocked() {
        let (dir, hash) = fixture("duplicate");
        let mut store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        store
            .append(
                EventKind::ImportCompleted,
                payload(&hash, "import:complete", Some(1)),
            )
            .unwrap();
        drop(store);
        let plan = preflight(&dir, &hash).unwrap();
        assert!(plan.blockers.contains(&"duplicate_import_event_key"));
        assert!(
            plan.blockers
                .contains(&"incomplete_or_duplicated_import_markers")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
