//! Non-mutating, sequence-preserving selection of effective journal events.
//!
//! The raw append-only journal and SQLite physical projection retain all
//! source evidence. Active readers can explicitly select effective events
//! after replay validates the durable source-quarantine decisions. No synthetic
//! sequence renumbering, in-place event mutation, or payload rewriting occurs.

use crate::EventEnvelope;
use crate::import_source_quarantine_audit::{
    ImportSourceQuarantineRecord, replay_import_source_quarantine_audit,
};
use std::collections::{BTreeMap, BTreeSet};

const SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";
const IMPORT_SCHEMA: &str = "chatarium-imported-observation";

#[derive(Debug)]
pub struct EffectiveJournalView<'a> {
    original_event_count: usize,
    visible: Vec<&'a EventEnvelope>,
    excluded_sequences: BTreeSet<u64>,
    quarantine_decisions: Vec<ImportSourceQuarantineRecord>,
}

impl<'a> EffectiveJournalView<'a> {
    /// Derive a separate effective view from a *complete* raw journal replay.
    ///
    /// Fail closed if source quarantine audit validation fails or the raw
    /// sequence is not contiguous. The caller must not substitute the output
    /// for the raw canonical journal when computing evidence hashes or checking
    /// replay integrity.
    pub fn from_raw(events: &'a [EventEnvelope]) -> Result<Self, String> {
        let decisions = replay_import_source_quarantine_audit(events)?;
        for (index, event) in events.iter().enumerate() {
            let expected = u64::try_from(index)
                .map_err(|_| "journal exceeds supported sequence size".to_owned())?
                .checked_add(1)
                .ok_or_else(|| "journal sequence overflow".to_owned())?;
            if event.sequence != expected {
                return Err("noncontiguous raw journal sequence in effective view".to_owned());
            }
        }

        let decisions_by_hash = decisions
            .iter()
            .map(|decision| (decision.source_sha256.as_str(), decision))
            .collect::<BTreeMap<_, _>>();
        let mut visible = Vec::with_capacity(events.len());
        let mut excluded_sequences = BTreeSet::new();
        let mut excluded_counts = BTreeMap::<String, usize>::new();

        for event in events {
            let owned_hash = source_hash(event);
            if let Some(record) = owned_hash
                .as_deref()
                .and_then(|hash| decisions_by_hash.get(hash))
            {
                if event.sequence < record.first_sequence || event.sequence > record.last_sequence {
                    return Err(
                        "quarantined source event outside verified sequence span".to_owned()
                    );
                }
                excluded_sequences.insert(event.sequence);
                *excluded_counts
                    .entry(record.source_sha256.clone())
                    .or_insert(0) += 1;
            } else {
                visible.push(event);
            }
        }

        for decision in &decisions {
            if excluded_counts
                .get(&decision.source_sha256)
                .copied()
                .unwrap_or(0)
                != decision.matching_event_count as usize
            {
                return Err("quarantine effective view count differs from decision".to_owned());
            }
        }

        Ok(Self {
            original_event_count: events.len(),
            visible,
            excluded_sequences,
            quarantine_decisions: decisions,
        })
    }

    /// Original event references, in original sequence order, never renumbered.
    pub fn visible_events(&self) -> &[&'a EventEnvelope] {
        &self.visible
    }

    /// Original journal sequence numbers excluded from effective queries.
    pub fn excluded_sequences(&self) -> &BTreeSet<u64> {
        &self.excluded_sequences
    }

    pub fn original_event_count(&self) -> usize {
        self.original_event_count
    }

    pub fn decisions(&self) -> &[ImportSourceQuarantineRecord] {
        &self.quarantine_decisions
    }
}

fn source_hash(event: &EventEnvelope) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
    if value.get("schema")?.as_str()? != IMPORT_SCHEMA
        || value.pointer("/source/format")?.as_str()? != SOURCE_FORMAT
    {
        return None;
    }
    value
        .pointer("/source/sha256")?
        .as_str()
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, MemoryEventStore};
    use chatarium_core::EventKind;
    use serde_json::json;

    fn hash() -> String {
        "a".repeat(64)
    }

    fn imported(key: &str, extra: serde_json::Value) -> String {
        json!({
            "schema": IMPORT_SCHEMA,
            "source": {"format": SOURCE_FORMAT, "sha256": hash(), "event_key": key},
            "details": extra
        })
        .to_string()
    }

    fn decision() -> String {
        json!({
            "schema": "chatarium-source-quarantine-audit",
            "version": 1,
            "record": "source_quarantine_recorded",
            "source_format": SOURCE_FORMAT,
            "source_sha256": hash(),
            "preflight_journal_sha256": "b".repeat(64),
            "matching_event_count": 3,
            "first_sequence": 1,
            "last_sequence": 4
        })
        .to_string()
    }

    fn source_and_native(with_decision: bool) -> MemoryEventStore {
        let mut store = MemoryEventStore::default();
        store
            .append(
                EventKind::ImportStarted,
                imported(
                    "import:start",
                    json!({"archive": format!("imports/flight-recorder/{}.json", hash())}),
                ),
            )
            .unwrap();
        store
            .append_scoped(
                Some("remote:source".to_owned()),
                EventKind::DraftChanged,
                imported("draft:1", json!({"text": "synthetic source"})),
            )
            .unwrap();
        store
            .append_scoped(
                Some("local:native".to_owned()),
                EventKind::DraftChanged,
                "native text must remain".to_owned(),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportCompleted,
                imported("import:complete", json!({"semantic_event_count": 1})),
            )
            .unwrap();
        if with_decision {
            store
                .append(EventKind::ImportSourceQuarantineRecorded, decision())
                .unwrap();
        }
        store
    }

    #[test]
    fn quarantine_excludes_only_verified_source_events_without_renumbering() {
        let store = source_and_native(true);
        let view = EffectiveJournalView::from_raw(store.events()).unwrap();
        assert_eq!(view.original_event_count(), 5);
        assert_eq!(view.decisions().len(), 1);
        assert_eq!(
            view.excluded_sequences()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 2, 4]
        );
        assert_eq!(
            view.visible_events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 5]
        );
        assert_eq!(view.visible_events()[0].payload, "native text must remain");
        assert_eq!(store.events()[1].sequence, 2);
        assert!(store.events()[1].payload.contains("synthetic source"));
    }

    #[test]
    fn no_decision_preserves_all_events() {
        let store = source_and_native(false);
        let view = EffectiveJournalView::from_raw(store.events()).unwrap();
        assert_eq!(view.visible_events().len(), store.events().len());
        assert!(view.excluded_sequences().is_empty());
    }

    #[test]
    fn malformed_quarantine_decision_refuses_an_effective_view() {
        let mut store = source_and_native(true);
        store
            .append(EventKind::ImportSourceQuarantineRecorded, decision())
            .unwrap();
        assert!(EffectiveJournalView::from_raw(store.events()).is_err());
    }

    #[test]
    fn gaps_do_not_qualify_as_complete_raw_journal() {
        let store = source_and_native(false);
        let mut events = store.events().to_vec();
        events.remove(1);
        assert!(EffectiveJournalView::from_raw(&events).is_err());
    }
}
