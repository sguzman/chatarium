//! Durable immutable local memory artifacts.
//!
//! Recording an artifact creates local memory data only. It does not make the
//! artifact inference context, transcript content, routed content, or lifecycle
//! state.

use crate::{EventEnvelope, EventStore};
use chatarium_core::local_memory::LocalMemoryId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-memory-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemoryArtifactRecord {
    pub memory_id: LocalMemoryId,
    pub source_conversation_id: LocalConversationId,
    pub text: String,
    pub recorded_sequence: u64,
}

pub fn record_local_memory_artifact(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    source_conversation_id: LocalConversationId,
    text: impl Into<String>,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(memory_scope(memory_id)),
        EventKind::LocalMemoryArtifactRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_artifact",
            "memory_id": memory_id.get(),
            "source_conversation_id": source_conversation_id.to_string(),
            "text": text.into(),
        }),
    )
}

pub fn replay_local_memory_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalMemoryArtifactRecord>, String> {
    let mut records = BTreeMap::<LocalMemoryId, LocalMemoryArtifactRecord>::new();

    for event in events {
        if event.kind != EventKind::LocalMemoryArtifactRecorded {
            continue;
        }
        let value = typed_payload(event)?;
        let memory_id = LocalMemoryId::new(required_u64(&value, "memory_id")?);
        let source_conversation_id =
            LocalConversationId::from_str(required_string(&value, "source_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "local memory artifact at sequence {} has invalid source conversation id: {error}",
                        event.sequence
                    )
                })?;
        let text = required_string(&value, "text")?.to_owned();
        validate_scope(event, memory_id)?;

        if text.trim().is_empty() {
            return Err(format!(
                "local memory artifact {} at sequence {} has empty text",
                memory_id.get(),
                event.sequence
            ));
        }
        if records.contains_key(&memory_id) {
            return Err(format!(
                "duplicate local memory artifact {} at sequence {}",
                memory_id.get(),
                event.sequence
            ));
        }

        records.insert(
            memory_id,
            LocalMemoryArtifactRecord {
                memory_id,
                source_conversation_id,
                text,
                recorded_sequence: event.sequence,
            },
        );
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.recorded_sequence);
    Ok(values)
}

#[must_use]
pub fn memory_scope(memory_id: LocalMemoryId) -> String {
    format!("local-memory:{}", memory_id.get())
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
            "malformed local memory artifact at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local memory artifact at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local memory artifact at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_memory_artifact") {
        return Err(format!(
            "local memory artifact at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, memory_id: LocalMemoryId) -> Result<(), String> {
    let expected = memory_scope(memory_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local memory artifact at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed local memory artifact is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed local memory artifact is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;

    #[test]
    fn exact_memory_text_and_source_provenance_round_trip() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(1),
            source,
            " exact memory\nwith spacing ",
        )
        .unwrap();

        let records = replay_local_memory_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].memory_id, LocalMemoryId::new(1));
        assert_eq!(records[0].source_conversation_id, source);
        assert_eq!(records[0].text, " exact memory\nwith spacing ");
    }

    #[test]
    fn duplicate_identity_and_empty_memory_fail_replay() {
        let source = LocalConversationId::new();
        let mut duplicate = MemoryEventStore::default();
        for text in ["one", "two"] {
            record_local_memory_artifact(
                &mut duplicate,
                LocalMemoryId::new(1),
                source,
                text,
            )
            .unwrap();
        }
        assert!(
            replay_local_memory_audit(duplicate.events())
                .unwrap_err()
                .contains("duplicate")
        );

        let mut empty = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut empty,
            LocalMemoryId::new(1),
            source,
            "   ",
        )
        .unwrap();
        assert!(
            replay_local_memory_audit(empty.events())
                .unwrap_err()
                .contains("empty text")
        );
    }
}
