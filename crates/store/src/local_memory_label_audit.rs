//! Durable user-authored organization labels for immutable local memory artifacts.
//!
//! Labels are metadata beside artifact text. They do not alter memory text,
//! context admission, supersession, routing, lifecycle, or inference authority.

use crate::local_memory_audit::replay_local_memory_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::local_memory::{LocalMemoryId, LocalMemoryLabel};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SCHEMA: &str = "chatarium-local-memory-label-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemoryLabelRecord {
    pub memory_id: LocalMemoryId,
    pub label: LocalMemoryLabel,
    pub active: bool,
    pub first_added_sequence: u64,
    pub last_changed_sequence: u64,
}

pub fn record_local_memory_label_added(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    label: &LocalMemoryLabel,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(memory_label_scope(memory_id)),
        EventKind::LocalMemoryLabelAdded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_label_added",
            "memory_id": memory_id.get(),
            "label": label.as_str(),
        }),
    )
}

pub fn record_local_memory_label_removed(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    label: &LocalMemoryLabel,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(memory_label_scope(memory_id)),
        EventKind::LocalMemoryLabelRemoved,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_label_removed",
            "memory_id": memory_id.get(),
            "label": label.as_str(),
        }),
    )
}

/// Replay current label state while preserving first-add and latest-change provenance.
pub fn replay_local_memory_label_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalMemoryLabelRecord>, String> {
    let mut records = BTreeMap::<(LocalMemoryId, LocalMemoryLabel), LocalMemoryLabelRecord>::new();

    for (index, event) in events.iter().enumerate() {
        let active = match event.kind {
            EventKind::LocalMemoryLabelAdded => true,
            EventKind::LocalMemoryLabelRemoved => false,
            _ => continue,
        };

        let expected_record = if active {
            "local_memory_label_added"
        } else {
            "local_memory_label_removed"
        };
        let value = typed_payload(event, expected_record)?;
        let memory_id = LocalMemoryId::new(required_u64(&value, "memory_id")?);
        let label = LocalMemoryLabel::new(required_string(&value, "label")?.to_owned())
            .map_err(|error| {
                format!(
                    "local memory label event at sequence {} has invalid label: {error:?}",
                    event.sequence
                )
            })?;
        validate_scope(event, memory_id)?;

        let artifact_exists = replay_local_memory_audit(&events[..index])?
            .into_iter()
            .any(|artifact| artifact.memory_id == memory_id);
        if !artifact_exists {
            return Err(format!(
                "local memory label event at sequence {} references memory {} before artifact creation",
                event.sequence,
                memory_id.get()
            ));
        }

        let key = (memory_id, label.clone());
        match records.get_mut(&key) {
            None if active => {
                records.insert(
                    key,
                    LocalMemoryLabelRecord {
                        memory_id,
                        label,
                        active: true,
                        first_added_sequence: event.sequence,
                        last_changed_sequence: event.sequence,
                    },
                );
            }
            None => {
                return Err(format!(
                    "local memory label removal at sequence {} removes inactive label '{}' from memory {}",
                    event.sequence,
                    label,
                    memory_id.get()
                ));
            }
            Some(record) if record.active == active => {
                let action = if active { "add" } else { "remove" };
                return Err(format!(
                    "local memory label event at sequence {} repeats {action} for label '{}' on memory {}",
                    event.sequence,
                    label,
                    memory_id.get()
                ));
            }
            Some(record) => {
                record.active = active;
                record.last_changed_sequence = event.sequence;
            }
        }
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        left.first_added_sequence
            .cmp(&right.first_added_sequence)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
            .then_with(|| left.label.cmp(&right.label))
    });
    Ok(values)
}

pub fn replay_active_local_memory_labels(
    events: &[EventEnvelope],
    memory_id: LocalMemoryId,
) -> Result<Vec<LocalMemoryLabel>, String> {
    let mut labels = replay_local_memory_label_audit(events)?
        .into_iter()
        .filter(|record| record.memory_id == memory_id && record.active)
        .map(|record| record.label)
        .collect::<Vec<_>>();
    labels.sort();
    Ok(labels)
}

#[must_use]
pub fn memory_label_scope(memory_id: LocalMemoryId) -> String {
    format!("local-memory-label:{}", memory_id.get())
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

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed local memory label event at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local memory label event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local memory label event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "local memory label event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, memory_id: LocalMemoryId) -> Result<(), String> {
    let expected = memory_label_scope(memory_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local memory label event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed local memory label event is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed local memory label event is missing string field '{field}'"))
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
    fn labels_add_remove_and_readd_without_touching_artifact_text() {
        let source = LocalConversationId::new();
        let memory_id = LocalMemoryId::new(1);
        let label = LocalMemoryLabel::new("project:chatarium").unwrap();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, memory_id, source, "exact memory").unwrap();

        record_local_memory_label_added(&mut store, memory_id, &label).unwrap();
        let first = replay_local_memory_label_audit(store.events()).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].active);
        assert_eq!(first[0].label, label);

        record_local_memory_label_removed(&mut store, memory_id, &label).unwrap();
        assert!(
            replay_active_local_memory_labels(store.events(), memory_id)
                .unwrap()
                .is_empty()
        );

        record_local_memory_label_added(&mut store, memory_id, &label).unwrap();
        let active = replay_active_local_memory_labels(store.events(), memory_id).unwrap();
        assert_eq!(active, vec![label]);

        let artifacts = replay_local_memory_audit(store.events()).unwrap();
        assert_eq!(artifacts[0].text, "exact memory");
    }

    #[test]
    fn label_requires_prior_memory_and_valid_exact_text() {
        let memory_id = LocalMemoryId::new(1);
        let label = LocalMemoryLabel::new("topic").unwrap();
        let mut store = MemoryEventStore::default();
        record_local_memory_label_added(&mut store, memory_id, &label).unwrap();
        assert!(
            replay_local_memory_label_audit(store.events())
                .unwrap_err()
                .contains("before artifact creation")
        );

        let source = LocalConversationId::new();
        let mut malformed = MemoryEventStore::default();
        record_local_memory_artifact(&mut malformed, memory_id, source, "memory").unwrap();
        malformed
            .append_scoped(
                Some(memory_label_scope(memory_id)),
                EventKind::LocalMemoryLabelAdded,
                json!({
                    "schema": SCHEMA,
                    "version": VERSION,
                    "record": "local_memory_label_added",
                    "memory_id": memory_id.get(),
                    "label": " bad ",
                })
                .to_string(),
            )
            .unwrap();
        assert!(
            replay_local_memory_label_audit(malformed.events())
                .unwrap_err()
                .contains("invalid label")
        );
    }

    #[test]
    fn duplicate_add_and_remove_of_inactive_label_fail_closed() {
        let source = LocalConversationId::new();
        let memory_id = LocalMemoryId::new(1);
        let label = LocalMemoryLabel::new("topic").unwrap();

        let mut duplicate = MemoryEventStore::default();
        record_local_memory_artifact(&mut duplicate, memory_id, source, "memory").unwrap();
        record_local_memory_label_added(&mut duplicate, memory_id, &label).unwrap();
        record_local_memory_label_added(&mut duplicate, memory_id, &label).unwrap();
        assert!(
            replay_local_memory_label_audit(duplicate.events())
                .unwrap_err()
                .contains("repeats add")
        );

        let mut removal = MemoryEventStore::default();
        record_local_memory_artifact(&mut removal, memory_id, source, "memory").unwrap();
        record_local_memory_label_removed(&mut removal, memory_id, &label).unwrap();
        assert!(
            replay_local_memory_label_audit(removal.events())
                .unwrap_err()
                .contains("removes inactive")
        );
    }

    #[test]
    fn active_labels_are_sorted() {
        let source = LocalConversationId::new();
        let memory_id = LocalMemoryId::new(1);
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, memory_id, source, "memory").unwrap();
        for value in ["zeta", "alpha", "middle"] {
            let label = LocalMemoryLabel::new(value).unwrap();
            record_local_memory_label_added(&mut store, memory_id, &label).unwrap();
        }

        assert_eq!(
            replay_active_local_memory_labels(store.events(), memory_id)
                .unwrap()
                .into_iter()
                .map(|label| label.to_string())
                .collect::<Vec<_>>(),
            vec!["alpha", "middle", "zeta"]
        );
    }
}
