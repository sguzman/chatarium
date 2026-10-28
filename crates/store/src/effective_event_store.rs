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
    visible: Vec<EventEnvelope>,
    protected_scopes: BTreeSet<String>,
    quarantined_sources: BTreeSet<String>,
}

impl<S: EventStore> EffectiveEventStore<S> {
    /// Replay and validate the complete unfiltered journal before admitting it
    /// into a desktop or persistence worker. On failure the raw store remains
    /// owned by the caller only through the returned error, and is dropped.
    pub fn new(raw: S) -> Result<Self, String> {
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
        let visible = selection.visible_events().iter().map(|event| (*event).clone()).collect();
        Ok(Self {
            raw,
            visible,
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
        self.raw.events().len().saturating_sub(self.visible.len())
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
        self.visible.push(appended.clone());
        Ok(sequence)
    }

    fn events(&self) -> &[EventEnvelope] {
        &self.visible
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
