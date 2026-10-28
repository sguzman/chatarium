//! Explicit durable quarantine decision recording for validated effective replay.
//!
//! The strict journal writer lease, exact archived source verification, replay
//! admission, and preflight journal digest are all required before appending a
//! source-quarantine decision. The raw event history is never rewritten.

use crate::source_quarantine::preflight;
use chatarium_core::EventKind;
use chatarium_store::import_source_quarantine_audit::replay_import_source_quarantine_audit;
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use serde_json::json;
use std::path::Path;

const QUARANTINE_SCHEMA: &str = "chatarium-source-quarantine-audit";
const SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    pub source_sha256: String,
    pub recorded_sequence: u64,
    pub matching_events: usize,
    pub already_recorded: bool,
}

/// Durably record an audited source quarantine decision.
///
/// This command never removes raw events or alters SQLite. Eligible desktop
/// and worker effective readers honor the decision on their next startup;
/// physical evidence APIs remain deliberately unfiltered.
pub fn record(data_dir: &Path, source_sha256: &str) -> Result<RecordSummary, String> {
    if source_sha256.len() != 64 || !source_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("source SHA-256 must be exactly 64 hexadecimal characters".to_owned());
    }
    if !data_dir.is_dir() {
        return Err("quarantine record requires an existing explicit data directory".to_owned());
    }
    let source_sha256 = source_sha256.to_ascii_lowercase();
    let journal_path = data_dir.join("journal.jsonl");

    // Strict open refuses missing or torn journals and acquires exclusive
    // ownership before reading any bytes or preparing the appended decision.
    let mut store = JsonlEventStore::open_strict(&journal_path)
        .map_err(|error| format!("strict journal writer ownership refused: {error}"))?;
    let existing = replay_import_source_quarantine_audit(store.events())
        .map_err(|error| format!("replay existing quarantine decisions: {error}"))?;
    if let Some(previous) = existing
        .iter()
        .find(|record| record.source_sha256 == source_sha256)
    {
        return Ok(RecordSummary {
            source_sha256,
            recorded_sequence: previous.decision_sequence,
            matching_events: previous.matching_event_count as usize,
            already_recorded: true,
        });
    }

    // The source hash, source bytes, original journal digest, import markers,
    // exact scopes and foreign provenance claims are verified under the lease.
    let report = preflight(data_dir, &source_sha256)?;
    if !report.candidate_isolated() {
        return Err(format!(
            "quarantine record BLOCKED ({} safety blocker codes); no changes made",
            report.blockers.len()
        ));
    }
    if report.journal_events != store.events().len()
        || store.events().last().map_or(0, |event| event.sequence) != report.journal_events as u64
    {
        return Err(
            "journal changed or sequence accounting failed during quarantine recording".to_owned(),
        );
    }
    let matching_event_count = u64::try_from(report.matching_events)
        .map_err(|_| "matching import event count exceeds u64".to_owned())?;
    let payload = json!({
        "schema": QUARANTINE_SCHEMA,
        "version": 1,
        "record": "source_quarantine_recorded",
        "source_format": SOURCE_FORMAT,
        "source_sha256": report.source_sha256,
        "preflight_journal_sha256": report.journal_sha256,
        "matching_event_count": matching_event_count,
        "first_sequence": report.first_sequence,
        "last_sequence": report.last_sequence,
    })
    .to_string();

    // Fail before appending on every durable replay invariant. The
    // importer itself never changes active views in a running process.
    let next_sequence = store.events().last().map_or(1, |event| event.sequence + 1);
    let mut proposed = store.events().to_vec();
    proposed.push(EventEnvelope {
        sequence: next_sequence,
        at_unix_ms: 0,
        scope: None,
        kind: EventKind::ImportSourceQuarantineRecorded,
        payload: payload.clone(),
    });
    replay_import_source_quarantine_audit(&proposed)
        .map_err(|error| format!("quarantine admission rejected: {error}"))?;
    let recorded_sequence = store
        .append(EventKind::ImportSourceQuarantineRecorded, payload)
        .map_err(|error| format!("append quarantine decision: {error}"))?;
    if recorded_sequence != next_sequence {
        return Err("quarantine decision sequence mismatch after durable append".to_owned());
    }

    Ok(RecordSummary {
        source_sha256,
        recorded_sequence,
        matching_events: report.matching_events,
        already_recorded: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_store::inspect_jsonl_journal;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "chatarium-quarantine-record-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("imports/flight-recorder")).unwrap();
        let source = json!({"format": SOURCE_FORMAT, "version": 3}).to_string();
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        fs::write(
            root.join(format!("imports/flight-recorder/{hash}.json")),
            source,
        )
        .unwrap();
        let mut store = JsonlEventStore::open(root.join("journal.jsonl")).unwrap();
        for (kind, scope, key, details) in [
            (
                EventKind::ImportStarted,
                None,
                "import:start",
                json!({"archive": format!("imports/flight-recorder/{hash}.json")}),
            ),
            (
                EventKind::DraftChanged,
                Some("remote:example".to_owned()),
                "draft:1",
                json!({}),
            ),
            (
                EventKind::ImportCompleted,
                None,
                "import:complete",
                json!({"semantic_event_count": 1}),
            ),
        ] {
            store
                .append_scoped(
                    scope,
                    kind,
                    json!({
                        "schema": "chatarium-imported-observation",
                        "version": 1,
                        "source": {
                            "format": SOURCE_FORMAT,
                            "version": 3,
                            "sha256": hash,
                            "event_key": key
                        },
                        "details": details
                    })
                    .to_string(),
                )
                .unwrap();
        }
        drop(store);
        (root, hash)
    }

    #[test]
    fn durable_record_is_idempotent_and_leaves_source_journal_prefix_intact() {
        let (dir, hash) = fixture();
        let journal = dir.join("journal.jsonl");
        let before = fs::read(&journal).unwrap();
        let first = record(&dir, &hash.to_ascii_uppercase()).unwrap();
        assert!(!first.already_recorded);
        assert_eq!(first.recorded_sequence, 4);
        assert_eq!(first.matching_events, 3);
        let after = fs::read(&journal).unwrap();
        assert!(after.starts_with(&before));
        let events = inspect_jsonl_journal(&journal).unwrap().events;
        let decisions = replay_import_source_quarantine_audit(&events).unwrap();
        assert_eq!(decisions.len(), 1);
        let payload: Value = serde_json::from_str(&events[3].payload).unwrap();
        assert_eq!(
            payload["preflight_journal_sha256"],
            format!("{:x}", Sha256::digest(before))
        );

        let second = record(&dir, &hash).unwrap();
        assert!(second.already_recorded);
        assert_eq!(second.recorded_sequence, 4);
        assert_eq!(fs::read(&journal).unwrap(), after);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_quarantine_flow_preserves_raw_evidence_across_restart_and_sqlite_rebuild() {
        use chatarium_store::effective_event_store::EffectiveEventStore;
        use chatarium_store::projection::SqliteProjection;

        let (dir, hash) = fixture();
        let journal = dir.join("journal.jsonl");
        let source_archive = dir.join(format!("imports/flight-recorder/{hash}.json"));
        let source_bytes = fs::read(&source_archive).unwrap();

        // A native event is interleaved with source evidence in the same
        // authoritative journal. It must survive the decision, replay, and
        // SQLite rebuild without any sequence reassignment.
        let mut initial = JsonlEventStore::open_strict(&journal).unwrap();
        let native_sequence = initial
            .append_scoped(
                Some("local:authored".to_owned()),
                EventKind::DraftChanged,
                "private native draft must remain".to_owned(),
            )
            .unwrap();
        assert_eq!(native_sequence, 4);
        drop(initial);

        let original_bytes = fs::read(&journal).unwrap();
        let preview = preflight(&dir, &hash).unwrap();
        assert!(preview.candidate_isolated(), "{:?}", preview.blockers);
        assert_eq!(preview.matching_events, 3);
        assert_eq!(preview.journal_events, 4);
        assert_eq!((preview.first_sequence, preview.last_sequence), (1, 3));

        let decision = record(&dir, &hash).unwrap();
        assert_eq!(decision.recorded_sequence, 5);
        assert!(!decision.already_recorded);
        let after_decision = fs::read(&journal).unwrap();
        assert!(after_decision.starts_with(&original_bytes));
        assert_eq!(fs::read(&source_archive).unwrap(), source_bytes);

        let raw = JsonlEventStore::open_strict(&journal).unwrap();
        let mut active = EffectiveEventStore::new(raw).unwrap();
        assert_eq!(active.authoritative_events().len(), 5);
        assert_eq!(active.excluded_count(), 3);
        assert_eq!(
            active
                .events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(
            active.events()[0].payload,
            "private native draft must remain"
        );

        let mut sqlite = SqliteProjection::open(dir.join("journal-cache.sqlite3")).unwrap();
        sqlite.rebuild(active.authoritative_events()).unwrap();
        let view = sqlite
            .effective_read_view_verified(active.authoritative_events())
            .unwrap();
        assert!(view.events_for_scope("remote:example").is_empty());
        assert_eq!(view.events_for_scope("local:authored").len(), 1);
        assert_eq!(view.events_of_kind(EventKind::DraftChanged).len(), 1);
        assert_eq!(sqlite.events_for_scope("remote:example").unwrap().len(), 1);
        assert_eq!(sqlite.event_count().unwrap(), 5);

        // New native work still uses the raw monotonic sequence. A previously
        // attested SQLite cache must fail closed until explicitly rebuilt.
        let next = active
            .append_scoped(
                Some("local:authored".to_owned()),
                EventKind::DraftChanged,
                "native draft after quarantine".to_owned(),
            )
            .unwrap();
        assert_eq!(next, 6);
        assert!(
            sqlite
                .effective_read_view_verified(active.authoritative_events())
                .is_err()
        );
        sqlite.rebuild(active.authoritative_events()).unwrap();
        let refreshed = sqlite
            .effective_read_view_verified(active.authoritative_events())
            .unwrap();
        assert_eq!(refreshed.events_for_scope("local:authored").len(), 2);
        assert!(refreshed.events_for_scope("remote:example").is_empty());
        assert_eq!(refreshed.events_of_kind(EventKind::DraftChanged).len(), 2);
        assert_eq!(sqlite.event_count().unwrap(), 6);
        drop(sqlite);
        drop(active);

        // Restart simulation: release the single writer lease, re-open the
        // unchanged journal and rediscover the same effective event selection.
        let already = record(&dir, &hash).unwrap();
        assert!(already.already_recorded);
        assert_eq!(already.recorded_sequence, 5);
        let restarted = JsonlEventStore::open_strict(&journal).unwrap();
        let active = EffectiveEventStore::new(restarted).unwrap();
        assert_eq!(
            active
                .events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
        assert_eq!(active.authoritative_events().len(), 6);
        assert_eq!(active.excluded_count(), 3);
        assert_eq!(fs::read(&source_archive).unwrap(), source_bytes);
        drop(active);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn active_writer_and_incomplete_journal_fail_without_mutation() {
        let (dir, hash) = fixture();
        let journal = dir.join("journal.jsonl");
        let writer = JsonlEventStore::open(&journal).unwrap();
        let bytes = fs::read(&journal).unwrap();
        assert!(record(&dir, &hash).is_err());
        assert_eq!(fs::read(&journal).unwrap(), bytes);
        drop(writer);

        let mut torn = bytes;
        torn.extend_from_slice(b"torn");
        fs::write(&journal, &torn).unwrap();
        assert!(record(&dir, &hash).is_err());
        assert_eq!(fs::read(&journal).unwrap(), torn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn same_scope_native_event_prevents_record_and_keeps_original_journal() {
        let (dir, hash) = fixture();
        let journal = dir.join("journal.jsonl");
        let mut writer = JsonlEventStore::open(&journal).unwrap();
        writer
            .append_scoped(
                Some("remote:example".to_owned()),
                EventKind::DraftChanged,
                "native text".to_owned(),
            )
            .unwrap();
        drop(writer);
        let bytes = fs::read(&journal).unwrap();
        assert!(record(&dir, &hash).is_err());
        assert_eq!(fs::read(&journal).unwrap(), bytes);
        fs::remove_dir_all(dir).unwrap();
    }
}
