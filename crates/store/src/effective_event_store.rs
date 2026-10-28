//! Durable, source-aware replay store for application and worker consumers.
//!
//! This adapter never substitutes a filtered slice for the authoritative JSONL:
//! the underlying store owns original event numbers and performs all writes.
//! Read-side EventStore::events() returns validated effective events. Desktop
//! code must take snapshots from the adapter, not from the raw store.

use crate::effective_journal_view::EffectiveJournalView;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io;

const IMPORT_SCHEMA: &str = "chatarium-imported-observation";
const SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";

/// A worker-facing filtered read view around an unmodified authoritative writer.
#[derive(Debug)]
pub struct EffectiveEventStore<S: EventStore> {
    raw: S,
    // None is a zero-copy pass-through for ordinary non-quarantined journals.
    // Only a journal with a durable source decision materializes filtered rows.
    visible: Option<Vec<EventEnvelope>>,
    protected_scopes: BTreeSet<String>,
    quarantined_sources: BTreeSet<String>,
}

impl<S: EventStore> EffectiveEventStore<S> {
    /// Replay and validate the complete unfiltered journal before admitting it
    /// into a desktop or persistence worker. On failure the raw store remains
    /// owned by the caller only through the returned error, and is dropped.
    pub fn new(raw: S) -> Result<Self, String> {
        // Keep the ordinary desktop startup fast: do not parse or duplicate
        // every payload when there is no source quarantine decision.
        if !raw
            .events()
            .iter()
            .any(|event| event.kind == EventKind::ImportSourceQuarantineRecorded)
        {
            return Ok(Self {
                raw,
                visible: None,
                protected_scopes: BTreeSet::new(),
                quarantined_sources: BTreeSet::new(),
            });
        }
        let selection = EffectiveJournalView::from_raw(raw.events())?;
        let protected_scopes = raw
            .events()
            .iter()
            .filter(|event| selection.excluded_sequences().contains(&event.sequence))
            .filter_map(|event| event.scope.clone())
            .collect();
        let quarantined_sources = selection
            .decisions()
            .iter()
            .map(|decision| decision.source_sha256.clone())
            .collect();
        let visible = selection
            .visible_events()
            .iter()
            .map(|event| (*event).clone())
            .collect();
        Ok(Self {
            raw,
            visible: Some(visible),
            protected_scopes,
            quarantined_sources,
        })
    }

    /// Raw authoritative journal for integrity/rebuild operations only.
    pub fn authoritative_events(&self) -> &[EventEnvelope] {
        self.raw.events()
    }

    /// Number of events excluded from active replay (not deleted).
    pub fn excluded_count(&self) -> usize {
        self.raw.events().len().saturating_sub(self.events().len())
    }

    /// Borrow the original writer only where physical evidence is required.
    pub fn into_authoritative(self) -> S {
        self.raw
    }
}

impl<S: EventStore> EventStore for EffectiveEventStore<S> {
    fn append_scoped(
        &mut self,
        scope: Option<String>,
        kind: EventKind,
        payload: String,
    ) -> io::Result<u64> {
        if kind == EventKind::ImportSourceQuarantineRecorded {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "quarantine decisions require the exclusive importer recording path",
            ));
        }
        if scope
            .as_deref()
            .is_some_and(|candidate| self.protected_scopes.contains(candidate))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "quarantined import scope is protected from new writes",
            ));
        }
        if import_source_hash(&payload)
            .as_deref()
            .is_some_and(|hash| self.quarantined_sources.contains(hash))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "quarantined source cannot re-enter effective journal",
            ));
        }

        let sequence = self.raw.append_scoped(scope, kind, payload)?;
        let Some(appended) = self.raw.events().last() else {
            return Err(io::Error::other(
                "authoritative store reported append without an event",
            ));
        };
        if appended.sequence != sequence {
            return Err(io::Error::other(
                "authoritative store returned a mismatched append sequence",
            ));
        }
        if let Some(visible) = &mut self.visible {
            visible.push(appended.clone());
        }
        Ok(sequence)
    }

    fn events(&self) -> &[EventEnvelope] {
        self.visible.as_deref().unwrap_or_else(|| self.raw.events())
    }
}

fn import_source_hash(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
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
    use crate::MemoryEventStore;
    use serde_json::json;

    fn source() -> String {
        "a".repeat(64)
    }

    fn imported(key: &str, details: Value) -> String {
        json!({
            "schema": IMPORT_SCHEMA,
            "source": {
                "format": SOURCE_FORMAT,
                "sha256": source(),
                "event_key": key
            },
            "details": details
        })
        .to_string()
    }

    fn with_decision() -> MemoryEventStore {
        let mut raw = MemoryEventStore::default();
        raw.append(
            EventKind::ImportStarted,
            imported(
                "import:start",
                json!({"archive": format!("imports/flight-recorder/{}.json", source())}),
            ),
        )
        .unwrap();
        raw.append_scoped(
            Some("remote:synthetic".to_owned()),
            EventKind::DraftChanged,
            imported("draft:1", json!({})),
        )
        .unwrap();
        raw.append_scoped(
            Some("local:authored".to_owned()),
            EventKind::UserMessageCommitted,
            "native must remain visible".to_owned(),
        )
        .unwrap();
        raw.append(
            EventKind::ImportCompleted,
            imported("import:complete", json!({"semantic_event_count": 1})),
        )
        .unwrap();
        raw.append(
            EventKind::ImportSourceQuarantineRecorded,
            json!({
                "schema": "chatarium-source-quarantine-audit",
                "version": 1,
                "record": "source_quarantine_recorded",
                "source_format": SOURCE_FORMAT,
                "source_sha256": source(),
                "preflight_journal_sha256": "b".repeat(64),
                "matching_event_count": 3,
                "first_sequence": 1,
                "last_sequence": 4
            })
            .to_string(),
        )
        .unwrap();
        raw
    }

    #[test]
    fn validated_native_replay_preserves_raw_history_and_sequence_numbers() {
        let raw = with_decision();
        let mut effective = EffectiveEventStore::new(raw).unwrap();
        assert_eq!(effective.authoritative_events().len(), 5);
        assert_eq!(effective.excluded_count(), 3);
        assert_eq!(
            effective
                .events()
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 5]
        );
        assert_eq!(effective.events()[0].payload, "native must remain visible");

        let appended = effective
            .append_scoped(
                Some("local:authored".to_owned()),
                EventKind::DraftChanged,
                "next native draft".to_owned(),
            )
            .unwrap();
        assert_eq!(appended, 6);
        assert_eq!(effective.events().last().unwrap().sequence, 6);
        assert_eq!(effective.authoritative_events().len(), 6);
        assert_eq!(effective.authoritative_events()[1].sequence, 2);
        assert_eq!(effective.excluded_count(), 3);
    }

    #[test]
    fn rejects_post_quarantine_scope_or_source_reuse_without_writing() {
        let mut effective = EffectiveEventStore::new(with_decision()).unwrap();
        let before = effective.authoritative_events().len();
        let conflict = effective.append_scoped(
            Some("remote:synthetic".to_owned()),
            EventKind::DraftChanged,
            "native same scope".to_owned(),
        );
        assert_eq!(
            conflict.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let reimport = effective.append(EventKind::DraftChanged, imported("draft:2", json!({})));
        assert_eq!(
            reimport.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(effective.authoritative_events().len(), before);
    }

    #[test]
    fn rejects_untrusted_decision_recording_and_invalid_replay() {
        let mut effective = EffectiveEventStore::new(with_decision()).unwrap();
        let error = effective.append(
            EventKind::ImportSourceQuarantineRecorded,
            "not an importer decision".to_owned(),
        );
        assert_eq!(error.unwrap_err().kind(), io::ErrorKind::PermissionDenied);

        let mut invalid = with_decision();
        invalid
            .append(EventKind::ImportSourceQuarantineRecorded, "{}".to_owned())
            .unwrap();
        assert!(EffectiveEventStore::new(invalid).is_err());
    }

    #[test]
    fn without_decisions_all_raw_events_remain_visible() {
        let mut raw = MemoryEventStore::default();
        raw.append(EventKind::DraftChanged, "native".to_owned())
            .unwrap();
        let effective = EffectiveEventStore::new(raw).unwrap();
        assert_eq!(effective.excluded_count(), 0);
        assert_eq!(effective.events(), effective.authoritative_events());
    }
}
