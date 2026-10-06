//! Durable local-conversation -> logical-chat-container topology audit.
//!
//! This layer correlates today's user-facing LocalConversationId with the
//! orchestration continuity plane without conflating their identity domains.
//! ChatContainerId -> current SessionId remains owned by chat_container_audit.

use crate::chat_container_audit::{ChatContainerAuditRecord, replay_chat_container_audit};
use crate::{EventEnvelope, EventStore};
use chatarium_core::chat_container::{ChatContainerId, SessionLifecyclePhase};
use chatarium_core::session::SessionId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-conversation-chat-container-audit";
const VERSION: u64 = 1;

/// One durable identity edge from a user-facing local conversation to a logical
/// orchestration chat container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalConversationChatContainerBindingRecord {
    pub conversation_id: LocalConversationId,
    pub container_id: ChatContainerId,
    pub bound_sequence: u64,
}

/// Restart-replayable local-first orchestration topology for one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalConversationTopologyRecord {
    pub conversation_id: LocalConversationId,
    pub container_id: ChatContainerId,
    pub root_session_id: SessionId,
    pub current_session_id: SessionId,
    pub current_session_phase: SessionLifecyclePhase,
    pub session_count: usize,
    pub bound_sequence: u64,
    pub container_last_sequence: u64,
}

/// Append one local conversation -> logical chat-container correlation.
///
/// The referenced container must already exist for replay to accept this edge.
/// Creating this edge does not create routing authority, worker identity, or
/// cross-conversation context sharing.
pub fn record_local_conversation_chat_container_bound(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    container_id: ChatContainerId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(scope(conversation_id, container_id)),
        EventKind::LocalConversationChatContainerBound,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_conversation_chat_container_bound",
            "conversation_id": conversation_id.to_string(),
            "container_id": container_id.get(),
        }),
    )
}

/// Reconstruct one-to-one conversation -> container identity edges.
///
/// A binding is legal only after the referenced container was durably created.
/// One local conversation cannot own two containers and one container cannot be
/// claimed by two local conversations through this bridge.
pub fn replay_local_conversation_chat_container_bindings(
    events: &[EventEnvelope],
) -> Result<Vec<LocalConversationChatContainerBindingRecord>, String> {
    let containers = replay_chat_container_audit(events)?
        .into_iter()
        .map(|record| (record.container_id, record.created_sequence))
        .collect::<BTreeMap<_, _>>();

    let mut by_conversation =
        BTreeMap::<LocalConversationId, LocalConversationChatContainerBindingRecord>::new();
    let mut container_owner = BTreeMap::<ChatContainerId, LocalConversationId>::new();

    for event in events {
        if event.kind != EventKind::LocalConversationChatContainerBound {
            continue;
        }

        let payload = typed_payload(event)?;
        let conversation_id = LocalConversationId::from_str(required_string(
            &payload,
            "conversation_id",
        )?)
        .map_err(|error| {
            format!(
                "invalid local conversation id in chat-container binding at sequence {}: {error}",
                event.sequence
            )
        })?;
        let container_id = ChatContainerId::new(required_u64(&payload, "container_id")?);
        validate_scope(event, conversation_id, container_id)?;

        let created_sequence = containers.get(&container_id).ok_or_else(|| {
            format!(
                "local conversation chat-container binding at sequence {} references unknown container {}",
                event.sequence,
                container_id.get()
            )
        })?;
        if *created_sequence >= event.sequence {
            return Err(format!(
                "local conversation chat-container binding at sequence {} references container {} created at non-prior sequence {}",
                event.sequence,
                container_id.get(),
                created_sequence
            ));
        }

        if let Some(existing) = by_conversation.get(&conversation_id) {
            return Err(format!(
                "local conversation {conversation_id} is already bound to chat container {}; cannot also bind container {} at sequence {}",
                existing.container_id.get(),
                container_id.get(),
                event.sequence
            ));
        }
        if let Some(existing_conversation) = container_owner.get(&container_id) {
            return Err(format!(
                "chat container {} is already bound to local conversation {}; cannot also bind conversation {conversation_id} at sequence {}",
                container_id.get(),
                existing_conversation,
                event.sequence
            ));
        }

        let record = LocalConversationChatContainerBindingRecord {
            conversation_id,
            container_id,
            bound_sequence: event.sequence,
        };
        by_conversation.insert(conversation_id, record);
        container_owner.insert(container_id, conversation_id);
    }

    let mut records = by_conversation.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.bound_sequence);
    Ok(records)
}

/// Join local conversation ownership with the existing chat-container lineage.
///
/// This is a projection only. Current SessionId comes from the authoritative
/// ChatContainer audit, so future rollover automatically changes this projection
/// without rewriting the LocalConversationId -> ChatContainerId edge.
pub fn replay_local_conversation_topologies(
    events: &[EventEnvelope],
) -> Result<Vec<LocalConversationTopologyRecord>, String> {
    let containers = replay_chat_container_audit(events)?
        .into_iter()
        .map(|record| (record.container_id, record))
        .collect::<BTreeMap<_, _>>();

    let mut topologies = Vec::new();
    for binding in replay_local_conversation_chat_container_bindings(events)? {
        let container = containers.get(&binding.container_id).ok_or_else(|| {
            format!(
                "bound chat container {} disappeared from replay",
                binding.container_id.get()
            )
        })?;
        topologies.push(join_topology(binding, container)?);
    }
    topologies.sort_by_key(|record| record.bound_sequence);
    Ok(topologies)
}

fn join_topology(
    binding: LocalConversationChatContainerBindingRecord,
    container: &ChatContainerAuditRecord,
) -> Result<LocalConversationTopologyRecord, String> {
    let current = container
        .sessions
        .iter()
        .find(|session| session.session_id == container.current_session_id)
        .ok_or_else(|| {
            format!(
                "chat container {} current session {} is absent from its replayed lineage",
                container.container_id.get(),
                container.current_session_id.get()
            )
        })?;

    Ok(LocalConversationTopologyRecord {
        conversation_id: binding.conversation_id,
        container_id: binding.container_id,
        root_session_id: container.root_session_id,
        current_session_id: container.current_session_id,
        current_session_phase: current.phase,
        session_count: container.sessions.len(),
        bound_sequence: binding.bound_sequence,
        container_last_sequence: container.last_sequence,
    })
}

/// Stable scope for one conversation -> chat-container identity edge.
#[must_use]
pub fn scope(conversation_id: LocalConversationId, container_id: ChatContainerId) -> String {
    format!(
        "local-conversation-chat-container:{conversation_id}:{}",
        container_id.get()
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
            "malformed local conversation chat-container payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local conversation chat-container event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local conversation chat-container event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str)
        != Some("local_conversation_chat_container_bound")
    {
        return Err(format!(
            "local conversation chat-container event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    conversation_id: LocalConversationId,
    container_id: ChatContainerId,
) -> Result<(), String> {
    let expected = scope(conversation_id, container_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local conversation chat-container event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed local conversation topology payload is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed local conversation topology payload is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_container_audit::{
        record_chat_container_created, record_chat_session_lifecycle_transition,
        record_chat_session_successor_bound,
    };
    use crate::session_audit::record_local_session_registered;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::chat_container::{
        ContextHandoffId, SessionLifecycleTransition, SessionSuccessorBinding,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const C1: ChatContainerId = ChatContainerId::new(10);
    const C2: ChatContainerId = ChatContainerId::new(20);
    const S1: SessionId = SessionId::new(1);
    const S2: SessionId = SessionId::new(2);

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-local-topology-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    fn create_container(
        store: &mut impl EventStore,
        container: ChatContainerId,
        session: SessionId,
    ) {
        record_local_session_registered(store, session).unwrap();
        record_chat_container_created(store, container, session).unwrap();
    }

    #[test]
    fn binding_and_topology_survive_reopen() {
        let path = temp_path("reopen");
        let conversation = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            create_container(&mut store, C1, S1);
            record_local_conversation_chat_container_bound(&mut store, conversation, C1).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let bindings =
            replay_local_conversation_chat_container_bindings(reopened.events()).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].conversation_id, conversation);
        assert_eq!(bindings[0].container_id, C1);

        let topologies = replay_local_conversation_topologies(reopened.events()).unwrap();
        assert_eq!(topologies.len(), 1);
        assert_eq!(topologies[0].current_session_id, S1);
        assert_eq!(
            topologies[0].current_session_phase,
            SessionLifecyclePhase::Healthy
        );
        assert_eq!(topologies[0].session_count, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn one_to_one_binding_is_enforced() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        create_container(&mut store, C1, S1);
        create_container(&mut store, C2, S2);
        record_local_conversation_chat_container_bound(&mut store, first, C1).unwrap();
        record_local_conversation_chat_container_bound(&mut store, first, C2).unwrap();
        assert!(
            replay_local_conversation_chat_container_bindings(store.events())
                .unwrap_err()
                .contains("already bound to chat container")
        );

        let mut store = MemoryEventStore::default();
        create_container(&mut store, C1, S1);
        record_local_conversation_chat_container_bound(&mut store, first, C1).unwrap();
        record_local_conversation_chat_container_bound(&mut store, second, C1).unwrap();
        assert!(
            replay_local_conversation_chat_container_bindings(store.events())
                .unwrap_err()
                .contains("already bound to local conversation")
        );
    }

    #[test]
    fn binding_requires_prior_container() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_conversation_chat_container_bound(&mut store, conversation, C1).unwrap();
        assert!(
            replay_local_conversation_chat_container_bindings(store.events())
                .unwrap_err()
                .contains("unknown container")
        );
    }

    #[test]
    fn topology_tracks_current_session_after_rollover() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        create_container(&mut store, C1, S1);
        record_local_conversation_chat_container_bound(&mut store, conversation, C1).unwrap();

        record_chat_session_lifecycle_transition(
            &mut store,
            C1,
            SessionLifecycleTransition::new(
                S1,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .unwrap(),
        )
        .unwrap();
        record_local_session_registered(&mut store, S2).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, ContextHandoffId::new(100)).unwrap(),
        )
        .unwrap();

        let topology = replay_local_conversation_topologies(store.events())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(topology.root_session_id, S1);
        assert_eq!(topology.current_session_id, S2);
        assert_eq!(
            topology.current_session_phase,
            SessionLifecyclePhase::Healthy
        );
        assert_eq!(topology.session_count, 2);
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let conversation = LocalConversationId::new();
        let other = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        create_container(&mut store, C1, S1);

        let payload = serde_json::to_string(&json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_conversation_chat_container_bound",
            "conversation_id": conversation.to_string(),
            "container_id": C1.get(),
        }))
        .unwrap();
        store
            .append_scoped(
                Some(scope(other, C1)),
                EventKind::LocalConversationChatContainerBound,
                payload,
            )
            .unwrap();

        assert!(
            replay_local_conversation_chat_container_bindings(store.events())
                .unwrap_err()
                .contains("scope")
        );
    }
}
