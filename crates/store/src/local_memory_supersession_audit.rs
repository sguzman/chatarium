//! Durable explicit supersession between immutable local memory artifacts.
//!
//! Supersession preserves both artifacts. It records that one older artifact is
//! stale in favor of one newer artifact; it does not delete either artifact and
//! does not migrate context admission.

use crate::local_memory_audit::replay_local_memory_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::local_memory::LocalMemoryId;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "chatarium-local-memory-supersession-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMemorySupersessionRecord {
    pub predecessor_memory_id: LocalMemoryId,
    pub successor_memory_id: LocalMemoryId,
    pub recorded_sequence: u64,
}

/// Append one explicit old -> newer memory lineage edge.
pub fn record_local_memory_superseded(
    store: &mut impl EventStore,
    predecessor_memory_id: LocalMemoryId,
    successor_memory_id: LocalMemoryId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(memory_supersession_scope(
            predecessor_memory_id,
            successor_memory_id,
        )),
        EventKind::LocalMemoryArtifactSuperseded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_artifact_superseded",
            "predecessor_memory_id": predecessor_memory_id.get(),
            "successor_memory_id": successor_memory_id.get(),
        }),
    )
}

/// Replay all explicit memory lineage edges.
///
/// Each predecessor can have only one successor and each successor only one
/// predecessor. Successors must have been recorded later than predecessors and
/// both artifacts must share source-conversation provenance. This creates linear
/// forward-only lineages and mechanically prevents cycles.
pub fn replay_local_memory_supersession_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalMemorySupersessionRecord>, String> {
    let mut successor_by_predecessor = BTreeMap::<LocalMemoryId, LocalMemoryId>::new();
    let mut predecessor_by_successor = BTreeMap::<LocalMemoryId, LocalMemoryId>::new();
    let mut records = Vec::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::LocalMemoryArtifactSuperseded {
            continue;
        }
        let value = typed_payload(event)?;
        let predecessor_memory_id =
            LocalMemoryId::new(required_u64(&value, "predecessor_memory_id")?);
        let successor_memory_id = LocalMemoryId::new(required_u64(&value, "successor_memory_id")?);
        validate_scope(event, predecessor_memory_id, successor_memory_id)?;

        if predecessor_memory_id == successor_memory_id {
            return Err(format!(
                "local memory supersession at sequence {} cannot self-supersede memory {}",
                event.sequence,
                predecessor_memory_id.get()
            ));
        }
        if let Some(existing) = successor_by_predecessor.get(&predecessor_memory_id) {
            return Err(format!(
                "local memory {} is already superseded by {}; cannot also supersede it with {} at sequence {}",
                predecessor_memory_id.get(),
                existing.get(),
                successor_memory_id.get(),
                event.sequence
            ));
        }
        if let Some(existing) = predecessor_by_successor.get(&successor_memory_id) {
            return Err(format!(
                "local memory {} already succeeds memory {}; cannot also succeed memory {} at sequence {}",
                successor_memory_id.get(),
                existing.get(),
                predecessor_memory_id.get(),
                event.sequence
            ));
        }

        let artifacts = replay_local_memory_audit(&events[..index])?;
        let predecessor = artifacts
            .iter()
            .find(|record| record.memory_id == predecessor_memory_id)
            .ok_or_else(|| {
                format!(
                    "local memory supersession at sequence {} references predecessor memory {} before artifact creation",
                    event.sequence,
                    predecessor_memory_id.get()
                )
            })?;
        let successor = artifacts
            .iter()
            .find(|record| record.memory_id == successor_memory_id)
            .ok_or_else(|| {
                format!(
                    "local memory supersession at sequence {} references successor memory {} before artifact creation",
                    event.sequence,
                    successor_memory_id.get()
                )
            })?;

        if predecessor.source_conversation_id != successor.source_conversation_id {
            return Err(format!(
                "local memory supersession at sequence {} crosses source conversations {} and {}",
                event.sequence,
                predecessor.source_conversation_id,
                successor.source_conversation_id
            ));
        }
        if successor.recorded_sequence <= predecessor.recorded_sequence {
            return Err(format!(
                "local memory {} cannot supersede memory {} because successor artifact #{} is not newer than predecessor artifact #{}",
                successor_memory_id.get(),
                predecessor_memory_id.get(),
                successor.recorded_sequence,
                predecessor.recorded_sequence
            ));
        }

        successor_by_predecessor.insert(predecessor_memory_id, successor_memory_id);
        predecessor_by_successor.insert(successor_memory_id, predecessor_memory_id);
        records.push(LocalMemorySupersessionRecord {
            predecessor_memory_id,
            successor_memory_id,
            recorded_sequence: event.sequence,
        });
    }

    Ok(records)
}

#[must_use]
pub fn superseded_memory_ids(records: &[LocalMemorySupersessionRecord]) -> BTreeSet<LocalMemoryId> {
    records
        .iter()
        .map(|record| record.predecessor_memory_id)
        .collect()
}

pub fn current_memory_successor(
    records: &[LocalMemorySupersessionRecord],
    memory_id: LocalMemoryId,
) -> Option<LocalMemoryId> {
    records
        .iter()
        .find(|record| record.predecessor_memory_id == memory_id)
        .map(|record| record.successor_memory_id)
}

#[must_use]
pub fn memory_supersession_scope(
    predecessor_memory_id: LocalMemoryId,
    successor_memory_id: LocalMemoryId,
) -> String {
    format!(
        "local-memory-supersession:{}:{}",
        predecessor_memory_id.get(),
        successor_memory_id.get()
    )
}

fn append_typed(
    store: &mut impl EventStore,
    scope: Option<String>,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(scope, kind, encoded)
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed local memory supersession at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local memory supersession at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local memory supersession at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_memory_artifact_superseded") {
        return Err(format!(
            "local memory supersession at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    predecessor_memory_id: LocalMemoryId,
    successor_memory_id: LocalMemoryId,
) -> Result<(), String> {
    let expected = memory_supersession_scope(predecessor_memory_id, successor_memory_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local memory supersession at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed local memory supersession is missing integer field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::local_memory_audit::record_local_memory_artifact;
    use chatarium_core::LocalConversationId;

    #[test]
    fn linear_same_source_supersession_round_trips() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(1), source, "old").unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(2), source, "new").unwrap();
        record_local_memory_superseded(&mut store, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();

        let records = replay_local_memory_supersession_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].predecessor_memory_id, LocalMemoryId::new(1));
        assert_eq!(records[0].successor_memory_id, LocalMemoryId::new(2));
        assert_eq!(
            current_memory_successor(&records, LocalMemoryId::new(1)),
            Some(LocalMemoryId::new(2))
        );
    }

    #[test]
    fn chains_are_forward_only_and_branching_or_merging_is_rejected() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        for (id, text) in [(1, "a"), (2, "b"), (3, "c")] {
            record_local_memory_artifact(&mut store, LocalMemoryId::new(id), source, text).unwrap();
        }
        record_local_memory_superseded(&mut store, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();
        record_local_memory_superseded(&mut store, LocalMemoryId::new(2), LocalMemoryId::new(3))
            .unwrap();
        let records = replay_local_memory_supersession_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);

        let mut branch = MemoryEventStore::default();
        for (id, text) in [(1, "a"), (2, "b"), (3, "c")] {
            record_local_memory_artifact(&mut branch, LocalMemoryId::new(id), source, text)
                .unwrap();
        }
        record_local_memory_superseded(&mut branch, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();
        record_local_memory_superseded(&mut branch, LocalMemoryId::new(1), LocalMemoryId::new(3))
            .unwrap();
        assert!(
            replay_local_memory_supersession_audit(branch.events())
                .unwrap_err()
                .contains("already superseded")
        );
    }

    #[test]
    fn cross_source_and_backward_supersession_are_rejected() {
        let source = LocalConversationId::new();
        let other = LocalConversationId::new();

        let mut cross = MemoryEventStore::default();
        record_local_memory_artifact(&mut cross, LocalMemoryId::new(1), source, "a").unwrap();
        record_local_memory_artifact(&mut cross, LocalMemoryId::new(2), other, "b").unwrap();
        record_local_memory_superseded(&mut cross, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();
        assert!(
            replay_local_memory_supersession_audit(cross.events())
                .unwrap_err()
                .contains("crosses source conversations")
        );

        let mut backward = MemoryEventStore::default();
        record_local_memory_artifact(&mut backward, LocalMemoryId::new(1), source, "older")
            .unwrap();
        record_local_memory_artifact(&mut backward, LocalMemoryId::new(2), source, "newer")
            .unwrap();
        record_local_memory_superseded(&mut backward, LocalMemoryId::new(2), LocalMemoryId::new(1))
            .unwrap();
        assert!(
            replay_local_memory_supersession_audit(backward.events())
                .unwrap_err()
                .contains("not newer")
        );
    }
}
