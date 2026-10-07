//! Explicit per-conversation context decisions for immutable local memory.
//!
//! Memory existence and memory use are separate facts. An artifact defaults to
//! context-excluded until a destination conversation explicitly admits it.

use crate::local_memory_audit::replay_local_memory_audit;
use crate::local_memory_supersession_audit::{
    replay_local_memory_supersession_audit, superseded_memory_ids,
};
use crate::{EventEnvelope, EventStore};
use chatarium_core::local_memory::LocalMemoryId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-memory-context-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMemoryContextDecision {
    Admit,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMemoryContextRecord {
    pub memory_id: LocalMemoryId,
    pub source_conversation_id: LocalConversationId,
    pub destination_conversation_id: LocalConversationId,
    pub artifact_sequence: u64,
    pub decision: LocalMemoryContextDecision,
    pub first_decision_sequence: u64,
    pub last_decision_sequence: u64,
}

impl LocalMemoryContextRecord {
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(self.decision, LocalMemoryContextDecision::Admit)
    }
}

pub fn record_local_memory_context_decision(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    destination_conversation_id: LocalConversationId,
    decision: LocalMemoryContextDecision,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(memory_context_scope(destination_conversation_id, memory_id)),
        EventKind::LocalMemoryContextDecisionRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_context_decision",
            "memory_id": memory_id.get(),
            "destination_conversation_id": destination_conversation_id.to_string(),
            "decision": decision_name(decision),
        }),
    )
}

pub fn replay_local_memory_context_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalMemoryContextRecord>, String> {
    let mut records =
        BTreeMap::<(LocalConversationId, LocalMemoryId), LocalMemoryContextRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::LocalMemoryContextDecisionRecorded {
            continue;
        }
        let value = typed_payload(event)?;
        let memory_id = LocalMemoryId::new(required_u64(&value, "memory_id")?);
        let destination_conversation_id =
            LocalConversationId::from_str(required_string(&value, "destination_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "local memory context decision at sequence {} has invalid destination conversation id: {error}",
                        event.sequence
                    )
                })?;
        let decision = parse_decision(required_string(&value, "decision")?)?;
        validate_scope(event, destination_conversation_id, memory_id)?;

        let artifact = replay_local_memory_audit(&events[..index])?
            .into_iter()
            .find(|record| record.memory_id == memory_id)
            .ok_or_else(|| {
                format!(
                    "local memory context decision at sequence {} references memory {} before artifact creation",
                    event.sequence,
                    memory_id.get()
                )
            })?;

        let key = (destination_conversation_id, memory_id);
        match records.get_mut(&key) {
            Some(record) => {
                if record.source_conversation_id != artifact.source_conversation_id
                    || record.artifact_sequence != artifact.recorded_sequence
                {
                    return Err(format!(
                        "local memory {} context provenance conflicts with earlier decision",
                        memory_id.get()
                    ));
                }
                record.decision = decision;
                record.last_decision_sequence = event.sequence;
            }
            None => {
                records.insert(
                    key,
                    LocalMemoryContextRecord {
                        memory_id,
                        source_conversation_id: artifact.source_conversation_id,
                        destination_conversation_id,
                        artifact_sequence: artifact.recorded_sequence,
                        decision,
                        first_decision_sequence: event.sequence,
                        last_decision_sequence: event.sequence,
                    },
                );
            }
        }
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.first_decision_sequence);
    Ok(values)
}

pub fn replay_admitted_local_memory_context(
    events: &[EventEnvelope],
    destination_conversation_id: LocalConversationId,
) -> Result<Vec<LocalMemoryContextRecord>, String> {
    let supersessions = replay_local_memory_supersession_audit(events)?;
    let superseded = superseded_memory_ids(&supersessions);
    Ok(replay_local_memory_context_audit(events)?
        .into_iter()
        .filter(|record| {
            record.destination_conversation_id == destination_conversation_id
                && record.is_admitted()
                && !superseded.contains(&record.memory_id)
        })
        .collect())
}

#[must_use]
pub fn memory_context_scope(
    destination_conversation_id: LocalConversationId,
    memory_id: LocalMemoryId,
) -> String {
    format!(
        "local-memory-context:{destination_conversation_id}:{}",
        memory_id.get()
    )
}

const fn decision_name(decision: LocalMemoryContextDecision) -> &'static str {
    match decision {
        LocalMemoryContextDecision::Admit => "admit",
        LocalMemoryContextDecision::Exclude => "exclude",
    }
}

fn parse_decision(value: &str) -> Result<LocalMemoryContextDecision, String> {
    match value {
        "admit" => Ok(LocalMemoryContextDecision::Admit),
        "exclude" => Ok(LocalMemoryContextDecision::Exclude),
        _ => Err(format!(
            "unsupported local memory context decision '{value}'"
        )),
    }
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
            "malformed local memory context decision at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local memory context decision at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local memory context decision at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_memory_context_decision") {
        return Err(format!(
            "local memory context decision at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    destination_conversation_id: LocalConversationId,
    memory_id: LocalMemoryId,
) -> Result<(), String> {
    let expected = memory_context_scope(destination_conversation_id, memory_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local memory context decision at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed local memory context decision is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed local memory context decision is missing string field '{field}'")
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

    #[test]
    fn admit_and_exclude_are_reversible_per_destination() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let other = LocalConversationId::new();
        let memory_id = LocalMemoryId::new(1);
        let mut store = MemoryEventStore::default();

        record_local_memory_artifact(&mut store, memory_id, source, "memory").unwrap();
        record_local_memory_context_decision(
            &mut store,
            memory_id,
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap();
        assert_eq!(
            replay_admitted_local_memory_context(store.events(), destination)
                .unwrap()
                .len(),
            1
        );
        assert!(
            replay_admitted_local_memory_context(store.events(), other)
                .unwrap()
                .is_empty()
        );

        record_local_memory_context_decision(
            &mut store,
            memory_id,
            destination,
            LocalMemoryContextDecision::Exclude,
        )
        .unwrap();
        assert!(
            replay_admitted_local_memory_context(store.events(), destination)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn superseded_admission_remains_auditable_but_is_not_effective_context() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let predecessor = LocalMemoryId::new(1);
        let successor = LocalMemoryId::new(2);
        let mut store = MemoryEventStore::default();

        record_local_memory_artifact(&mut store, predecessor, source, "old").unwrap();
        record_local_memory_artifact(&mut store, successor, source, "new").unwrap();
        record_local_memory_context_decision(
            &mut store,
            predecessor,
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap();
        crate::local_memory_supersession_audit::record_local_memory_superseded(
            &mut store,
            predecessor,
            successor,
        )
        .unwrap();

        let raw = replay_local_memory_context_audit(store.events()).unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].memory_id, predecessor);
        assert_eq!(raw[0].decision, LocalMemoryContextDecision::Admit);
        assert!(
            replay_admitted_local_memory_context(store.events(), destination)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn decision_requires_prior_artifact() {
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_context_decision(
            &mut store,
            LocalMemoryId::new(99),
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap();

        assert!(
            replay_local_memory_context_audit(store.events())
                .unwrap_err()
                .contains("before artifact creation")
        );
    }
}
