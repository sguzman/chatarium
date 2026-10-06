//! Durable bridge from local Chatarium conversations to orchestration workers.
//!
//! The worker lifecycle already lives in the authoritative journal. This module
//! adds the missing local-first identity edge: which LocalConversationId owns
//! which WorkerId. It does not create goals, mutate lifecycle, create sessions,
//! or grant routing/continuation authority.

use crate::{EventEnvelope, EventStore};
use chatarium_core::orchestration::WorkerId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const LOCAL_CONVERSATION_WORKER_SCHEMA: &str = "chatarium-local-conversation-worker-audit";
const LOCAL_CONVERSATION_WORKER_VERSION: u64 = 1;

/// Restart-replayable one-to-one local conversation -> worker binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalConversationWorkerBindingRecord {
    /// Local conversation owning the worker identity.
    pub conversation_id: LocalConversationId,
    /// Existing orchestration worker identity.
    pub worker_id: WorkerId,
    /// Durable journal sequence where the binding was established.
    pub bound_sequence: u64,
}

/// Append one durable local conversation -> worker identity binding.
pub fn record_local_conversation_worker_bound(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(local_conversation_worker_scope(conversation_id, worker_id)),
        EventKind::LocalConversationWorkerBound,
        json!({
            "schema": LOCAL_CONVERSATION_WORKER_SCHEMA,
            "version": LOCAL_CONVERSATION_WORKER_VERSION,
            "record": "local_conversation_worker_bound",
            "conversation_id": conversation_id.to_string(),
            "worker_id": worker_id.get(),
        }),
    )
}

/// Replay all durable local conversation -> worker bindings.
///
/// Both identity domains are one-to-one. A conversation cannot silently change
/// workers and one worker cannot belong to multiple local conversations.
pub fn replay_local_conversation_worker_bindings(
    events: &[EventEnvelope],
) -> Result<Vec<LocalConversationWorkerBindingRecord>, String> {
    let mut by_conversation =
        BTreeMap::<LocalConversationId, LocalConversationWorkerBindingRecord>::new();
    let mut worker_owners = BTreeMap::<WorkerId, LocalConversationId>::new();

    for event in events {
        if event.kind != EventKind::LocalConversationWorkerBound {
            continue;
        }

        let payload = typed_payload(event)?;
        let conversation_id = LocalConversationId::from_str(required_string(
            &payload,
            "conversation_id",
        )?)
        .map_err(|error| {
            format!(
                "local conversation worker binding at sequence {} has invalid conversation id: {error}",
                event.sequence
            )
        })?;
        let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
        validate_scope(event, conversation_id, worker_id)?;

        if let Some(existing) = by_conversation.get(&conversation_id) {
            return Err(format!(
                "local conversation {conversation_id} is already bound to worker {}; cannot also bind worker {} at sequence {}",
                existing.worker_id.get(),
                worker_id.get(),
                event.sequence
            ));
        }
        if let Some(existing_conversation) = worker_owners.get(&worker_id) {
            return Err(format!(
                "worker {} is already bound to local conversation {}; cannot also bind conversation {} at sequence {}",
                worker_id.get(),
                existing_conversation,
                conversation_id,
                event.sequence
            ));
        }

        by_conversation.insert(
            conversation_id,
            LocalConversationWorkerBindingRecord {
                conversation_id,
                worker_id,
                bound_sequence: event.sequence,
            },
        );
        worker_owners.insert(worker_id, conversation_id);
    }

    let mut records = by_conversation.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.bound_sequence);
    Ok(records)
}

/// Stable durable scope for one conversation -> worker identity edge.
#[must_use]
pub fn local_conversation_worker_scope(
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> String {
    format!(
        "local-conversation-worker:{conversation_id}:{}",
        worker_id.get()
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
            "malformed local conversation worker payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str)
        != Some(LOCAL_CONVERSATION_WORKER_SCHEMA)
    {
        return Err(format!(
            "local conversation worker event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64)
        != Some(LOCAL_CONVERSATION_WORKER_VERSION)
    {
        return Err(format!(
            "local conversation worker event at sequence {} has unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str)
        != Some("local_conversation_worker_bound")
    {
        return Err(format!(
            "local conversation worker event at sequence {} has unexpected record",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<(), String> {
    let expected = local_conversation_worker_scope(conversation_id, worker_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local conversation worker event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!("typed local conversation worker payload is missing integer field '{field}'")
        })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("typed local conversation worker payload is missing string field '{field}'")
        })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::{JsonlEventStore, MemoryEventStore};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(1);
    const W2: WorkerId = WorkerId::new(2);

    fn temp_path(extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-local-worker-binding-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn binding_survives_reopen() {
        let path = temp_path("jsonl");
        let conversation = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_local_conversation_worker_bound(&mut store, conversation, W1).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert_eq!(
            replay_local_conversation_worker_bindings(reopened.events()).unwrap(),
            vec![LocalConversationWorkerBindingRecord {
                conversation_id: conversation,
                worker_id: W1,
                bound_sequence: 1,
            }]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversation_cannot_change_worker_identity() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_conversation_worker_bound(&mut store, conversation, W1).unwrap();
        record_local_conversation_worker_bound(&mut store, conversation, W2).unwrap();

        let error = replay_local_conversation_worker_bindings(store.events()).unwrap_err();
        assert!(error.contains("already bound to worker"));
    }

    #[test]
    fn worker_cannot_belong_to_two_local_conversations() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_conversation_worker_bound(&mut store, first, W1).unwrap();
        record_local_conversation_worker_bound(&mut store, second, W1).unwrap();

        let error = replay_local_conversation_worker_bindings(store.events()).unwrap_err();
        assert!(error.contains("already bound to local conversation"));
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(local_conversation_worker_scope(second, W1)),
                EventKind::LocalConversationWorkerBound,
                json!({
                    "schema": LOCAL_CONVERSATION_WORKER_SCHEMA,
                    "version": LOCAL_CONVERSATION_WORKER_VERSION,
                    "record": "local_conversation_worker_bound",
                    "conversation_id": first.to_string(),
                    "worker_id": W1.get(),
                })
                .to_string(),
            )
            .unwrap();

        assert!(
            replay_local_conversation_worker_bindings(store.events())
                .unwrap_err()
                .contains("scope")
        );
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(
            replay_local_conversation_worker_bindings(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn generic_sqlite_projection_carries_binding_without_schema_change() {
        let path = temp_path("sqlite");
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_conversation_worker_bound(&mut store, conversation, W1).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        assert_eq!(
            projection
                .events_of_kind(EventKind::LocalConversationWorkerBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_order_follows_durable_sequence() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_conversation_worker_bound(&mut store, first, W2).unwrap();
        record_local_conversation_worker_bound(&mut store, second, W1).unwrap();

        let records = replay_local_conversation_worker_bindings(store.events()).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.conversation_id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([first, second])
        );
        assert_eq!(records[0].conversation_id, first);
        assert_eq!(records[1].conversation_id, second);
    }
}
