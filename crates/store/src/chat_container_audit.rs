//! Durable logical-chat continuity and session-rollover audit.
//!
//! A chat container is the durable identity of an ongoing logical conversation.
//! Individual SessionId values are replaceable leaves in one linear lineage.
//! Saturation is therefore an expected handoff condition, not a container failure.

use crate::session_audit::replay_session_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::chat_container::{
    ChatContainerId, ContextHandoffId, SessionLifecyclePhase, SessionLifecycleTransition,
    SessionSuccessorBinding,
};
use chatarium_core::session::SessionId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CHAT_CONTAINER_AUDIT_SCHEMA: &str = "chatarium-chat-container-audit";
const CHAT_CONTAINER_AUDIT_VERSION: u64 = 1;

/// Restart-replayable state of one physical session inside a logical chat container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatContainerSessionRecord {
    pub session_id: SessionId,
    pub phase: SessionLifecyclePhase,
    pub joined_sequence: u64,
    pub phase_sequence: u64,
    pub predecessor_session_id: Option<SessionId>,
    pub successor_session_id: Option<SessionId>,
    pub successor_handoff_id: Option<ContextHandoffId>,
}

/// Restart-replayable logical chat container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatContainerAuditRecord {
    pub container_id: ChatContainerId,
    pub root_session_id: SessionId,
    pub current_session_id: SessionId,
    pub created_sequence: u64,
    pub sessions: Vec<ChatContainerSessionRecord>,
    pub last_sequence: u64,
}

/// Append creation of one logical chat container around an already-registered root session.
pub fn record_chat_container_created(
    store: &mut impl EventStore,
    container_id: ChatContainerId,
    root_session_id: SessionId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(chat_container_scope(container_id)),
        EventKind::ChatContainerCreated,
        json!({
            "schema": CHAT_CONTAINER_AUDIT_SCHEMA,
            "version": CHAT_CONTAINER_AUDIT_VERSION,
            "record": "chat_container_created",
            "container_id": container_id.get(),
            "root_session_id": root_session_id.get(),
        }),
    )
}

/// Append one explicit non-retirement lifecycle transition for the current leaf session.
pub fn record_chat_session_lifecycle_transition(
    store: &mut impl EventStore,
    container_id: ChatContainerId,
    transition: SessionLifecycleTransition,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(chat_session_lifecycle_scope(
            container_id,
            transition.session_id(),
        )),
        EventKind::ChatSessionLifecycleTransitionRecorded,
        json!({
            "schema": CHAT_CONTAINER_AUDIT_SCHEMA,
            "version": CHAT_CONTAINER_AUDIT_VERSION,
            "record": "chat_session_lifecycle_transition_recorded",
            "container_id": container_id.get(),
            "session_id": transition.session_id().get(),
            "from_phase": transition.from().stable_name(),
            "to_phase": transition.to().stable_name(),
        }),
    )
}

/// Append one saturated-leaf -> successor continuity edge.
///
/// Replay atomically retires the predecessor and joins the successor as Healthy.
pub fn record_chat_session_successor_bound(
    store: &mut impl EventStore,
    binding: SessionSuccessorBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(chat_session_successor_scope(
            binding.container_id(),
            binding.predecessor_session_id(),
            binding.successor_session_id(),
        )),
        EventKind::ChatSessionSuccessorBound,
        json!({
            "schema": CHAT_CONTAINER_AUDIT_SCHEMA,
            "version": CHAT_CONTAINER_AUDIT_VERSION,
            "record": "chat_session_successor_bound",
            "container_id": binding.container_id().get(),
            "predecessor_session_id": binding.predecessor_session_id().get(),
            "successor_session_id": binding.successor_session_id().get(),
            "context_handoff_id": binding.context_handoff_id().get(),
        }),
    )
}

/// Reconstruct all logical chat containers and session lineages.
///
/// Local SessionId registration is validated first by the existing session audit.
/// Every root/successor must have been registered before the continuity event that
/// references it.
pub fn replay_chat_container_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ChatContainerAuditRecord>, String> {
    let registered_at = replay_session_audit(events)?
        .into_iter()
        .map(|record| (record.session_id, record.registered_sequence))
        .collect::<BTreeMap<_, _>>();

    let mut containers = BTreeMap::<ChatContainerId, ReplayContainer>::new();
    let mut session_owner = BTreeMap::<SessionId, ChatContainerId>::new();
    let mut handoff_owner = BTreeMap::<ContextHandoffId, ChatContainerId>::new();

    for event in events {
        match event.kind {
            EventKind::ChatContainerCreated => replay_container_created(
                &registered_at,
                &mut containers,
                &mut session_owner,
                event,
            )?,
            EventKind::ChatSessionLifecycleTransitionRecorded => {
                replay_lifecycle_transition(&mut containers, event)?
            }
            EventKind::ChatSessionSuccessorBound => replay_successor_binding(
                &registered_at,
                &mut containers,
                &mut session_owner,
                &mut handoff_owner,
                event,
            )?,
            _ => {}
        }
    }

    let mut records = containers
        .into_values()
        .map(ReplayContainer::into_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.created_sequence);
    Ok(records)
}

#[derive(Debug)]
struct ReplayContainer {
    container_id: ChatContainerId,
    root_session_id: SessionId,
    current_session_id: SessionId,
    created_sequence: u64,
    sessions: BTreeMap<SessionId, ReplaySession>,
    last_sequence: u64,
}

#[derive(Debug, Clone, Copy)]
struct ReplaySession {
    session_id: SessionId,
    phase: SessionLifecyclePhase,
    joined_sequence: u64,
    phase_sequence: u64,
    predecessor_session_id: Option<SessionId>,
    successor_session_id: Option<SessionId>,
    successor_handoff_id: Option<ContextHandoffId>,
}

impl ReplayContainer {
    fn into_record(self) -> ChatContainerAuditRecord {
        let mut sessions = self
            .sessions
            .into_values()
            .map(ReplaySession::into_record)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|record| record.joined_sequence);
        ChatContainerAuditRecord {
            container_id: self.container_id,
            root_session_id: self.root_session_id,
            current_session_id: self.current_session_id,
            created_sequence: self.created_sequence,
            sessions,
            last_sequence: self.last_sequence,
        }
    }
}

impl ReplaySession {
    const fn into_record(self) -> ChatContainerSessionRecord {
        ChatContainerSessionRecord {
            session_id: self.session_id,
            phase: self.phase,
            joined_sequence: self.joined_sequence,
            phase_sequence: self.phase_sequence,
            predecessor_session_id: self.predecessor_session_id,
            successor_session_id: self.successor_session_id,
            successor_handoff_id: self.successor_handoff_id,
        }
    }
}

fn replay_container_created(
    registered_at: &BTreeMap<SessionId, u64>,
    containers: &mut BTreeMap<ChatContainerId, ReplayContainer>,
    session_owner: &mut BTreeMap<SessionId, ChatContainerId>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "chat_container_created")?;
    let container_id = ChatContainerId::new(required_u64(&payload, "container_id")?);
    let root_session_id = SessionId::new(required_u64(&payload, "root_session_id")?);
    validate_scope(event, &chat_container_scope(container_id))?;
    require_registered_before(registered_at, root_session_id, event.sequence, "root")?;

    if containers.contains_key(&container_id) {
        return Err(format!(
            "duplicate chat container {} at sequence {}",
            container_id.get(),
            event.sequence
        ));
    }
    if let Some(existing) = session_owner.get(&root_session_id) {
        return Err(format!(
            "session {} already belongs to chat container {}; cannot become root of container {} at sequence {}",
            root_session_id.get(),
            existing.get(),
            container_id.get(),
            event.sequence
        ));
    }

    let root = ReplaySession {
        session_id: root_session_id,
        phase: SessionLifecyclePhase::Healthy,
        joined_sequence: event.sequence,
        phase_sequence: event.sequence,
        predecessor_session_id: None,
        successor_session_id: None,
        successor_handoff_id: None,
    };
    let mut sessions = BTreeMap::new();
    sessions.insert(root_session_id, root);

    containers.insert(
        container_id,
        ReplayContainer {
            container_id,
            root_session_id,
            current_session_id: root_session_id,
            created_sequence: event.sequence,
            sessions,
            last_sequence: event.sequence,
        },
    );
    session_owner.insert(root_session_id, container_id);
    Ok(())
}

fn replay_lifecycle_transition(
    containers: &mut BTreeMap<ChatContainerId, ReplayContainer>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "chat_session_lifecycle_transition_recorded")?;
    let container_id = ChatContainerId::new(required_u64(&payload, "container_id")?);
    let session_id = SessionId::new(required_u64(&payload, "session_id")?);
    let from = required_phase(&payload, "from_phase")?;
    let to = required_phase(&payload, "to_phase")?;
    validate_scope(
        event,
        &chat_session_lifecycle_scope(container_id, session_id),
    )?;

    SessionLifecycleTransition::new(session_id, from, to).map_err(|error| {
        format!(
            "invalid chat-session lifecycle event at sequence {}: {error}",
            event.sequence
        )
    })?;

    let container = containers.get_mut(&container_id).ok_or_else(|| {
        format!(
            "chat-session lifecycle event at sequence {} references unknown container {}",
            event.sequence,
            container_id.get()
        )
    })?;
    if container.current_session_id != session_id {
        return Err(format!(
            "chat-session lifecycle event at sequence {} targets session {}, but current leaf is {}",
            event.sequence,
            session_id.get(),
            container.current_session_id.get()
        ));
    }
    let session = container.sessions.get_mut(&session_id).ok_or_else(|| {
        format!(
            "chat-session lifecycle event at sequence {} references nonmember session {}",
            event.sequence,
            session_id.get()
        )
    })?;
    if session.phase != from {
        return Err(format!(
            "stale chat-session lifecycle transition at sequence {}: session {} is {}, event expected {}",
            event.sequence,
            session_id.get(),
            session.phase.stable_name(),
            from.stable_name()
        ));
    }

    session.phase = to;
    session.phase_sequence = event.sequence;
    container.last_sequence = event.sequence;
    Ok(())
}

fn replay_successor_binding(
    registered_at: &BTreeMap<SessionId, u64>,
    containers: &mut BTreeMap<ChatContainerId, ReplayContainer>,
    session_owner: &mut BTreeMap<SessionId, ChatContainerId>,
    handoff_owner: &mut BTreeMap<ContextHandoffId, ChatContainerId>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "chat_session_successor_bound")?;
    let container_id = ChatContainerId::new(required_u64(&payload, "container_id")?);
    let predecessor_session_id = SessionId::new(required_u64(&payload, "predecessor_session_id")?);
    let successor_session_id = SessionId::new(required_u64(&payload, "successor_session_id")?);
    let context_handoff_id = ContextHandoffId::new(required_u64(&payload, "context_handoff_id")?);
    validate_scope(
        event,
        &chat_session_successor_scope(container_id, predecessor_session_id, successor_session_id),
    )?;

    let binding = SessionSuccessorBinding::new(
        container_id,
        predecessor_session_id,
        successor_session_id,
        context_handoff_id,
    )
    .map_err(|error| {
        format!(
            "invalid chat-session successor binding at sequence {}: {error}",
            event.sequence
        )
    })?;
    require_registered_before(
        registered_at,
        binding.successor_session_id(),
        event.sequence,
        "successor",
    )?;

    if let Some(existing) = session_owner.get(&successor_session_id) {
        return Err(format!(
            "successor session {} already belongs to chat container {}; cannot join container {} at sequence {}",
            successor_session_id.get(),
            existing.get(),
            container_id.get(),
            event.sequence
        ));
    }
    if let Some(existing) = handoff_owner.get(&context_handoff_id) {
        return Err(format!(
            "context handoff {} is already used by chat container {}; cannot reuse it at sequence {}",
            context_handoff_id.get(),
            existing.get(),
            event.sequence
        ));
    }

    let container = containers.get_mut(&container_id).ok_or_else(|| {
        format!(
            "chat-session successor binding at sequence {} references unknown container {}",
            event.sequence,
            container_id.get()
        )
    })?;
    if container.current_session_id != predecessor_session_id {
        return Err(format!(
            "chat-session successor binding at sequence {} targets predecessor {}, but current leaf is {}",
            event.sequence,
            predecessor_session_id.get(),
            container.current_session_id.get()
        ));
    }

    {
        let predecessor = container
            .sessions
            .get_mut(&predecessor_session_id)
            .ok_or_else(|| {
                format!(
                    "chat-session successor binding at sequence {} references nonmember predecessor {}",
                    event.sequence,
                    predecessor_session_id.get()
                )
            })?;
        if predecessor.phase != SessionLifecyclePhase::Saturated {
            return Err(format!(
                "predecessor session {} must be saturated before rollover; observed {} at sequence {}",
                predecessor_session_id.get(),
                predecessor.phase.stable_name(),
                event.sequence
            ));
        }
        if predecessor.successor_session_id.is_some() {
            return Err(format!(
                "predecessor session {} already has a successor at sequence {}",
                predecessor_session_id.get(),
                event.sequence
            ));
        }

        predecessor.phase = SessionLifecyclePhase::Retired;
        predecessor.phase_sequence = event.sequence;
        predecessor.successor_session_id = Some(successor_session_id);
        predecessor.successor_handoff_id = Some(context_handoff_id);
    }

    container.sessions.insert(
        successor_session_id,
        ReplaySession {
            session_id: successor_session_id,
            phase: SessionLifecyclePhase::Healthy,
            joined_sequence: event.sequence,
            phase_sequence: event.sequence,
            predecessor_session_id: Some(predecessor_session_id),
            successor_session_id: None,
            successor_handoff_id: None,
        },
    );
    container.current_session_id = successor_session_id;
    container.last_sequence = event.sequence;
    session_owner.insert(successor_session_id, container_id);
    handoff_owner.insert(context_handoff_id, container_id);
    Ok(())
}

fn require_registered_before(
    registered_at: &BTreeMap<SessionId, u64>,
    session_id: SessionId,
    event_sequence: u64,
    role: &str,
) -> Result<(), String> {
    let registered_sequence = registered_at.get(&session_id).ok_or_else(|| {
        format!(
            "{role} session {} is not registered before chat-container event at sequence {event_sequence}",
            session_id.get()
        )
    })?;
    if *registered_sequence >= event_sequence {
        return Err(format!(
            "{role} session {} registration sequence {} must predate chat-container event sequence {}",
            session_id.get(),
            registered_sequence,
            event_sequence
        ));
    }
    Ok(())
}

/// Stable scope for one logical chat-container creation.
#[must_use]
pub fn chat_container_scope(container_id: ChatContainerId) -> String {
    format!("chat-container:{}", container_id.get())
}

/// Stable scope for one lifecycle transition.
#[must_use]
pub fn chat_session_lifecycle_scope(
    container_id: ChatContainerId,
    session_id: SessionId,
) -> String {
    format!(
        "chat-session-lifecycle:{}:{}",
        container_id.get(),
        session_id.get()
    )
}

/// Stable scope for one successor edge.
#[must_use]
pub fn chat_session_successor_scope(
    container_id: ChatContainerId,
    predecessor_session_id: SessionId,
    successor_session_id: SessionId,
) -> String {
    format!(
        "chat-session-successor:{}:{}:{}",
        container_id.get(),
        predecessor_session_id.get(),
        successor_session_id.get()
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

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed chat-container payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(CHAT_CONTAINER_AUDIT_SCHEMA) {
        return Err(format!(
            "chat-container event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "chat-container event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != CHAT_CONTAINER_AUDIT_VERSION {
        return Err(format!(
            "unsupported chat-container audit payload version {version} at sequence {}",
            event.sequence
        ));
    }
    let record = required_string(&value, "record")?;
    if record != expected_record {
        return Err(format!(
            "chat-container event at sequence {} has record '{record}', expected '{expected_record}'",
            event.sequence
        ));
    }
    Ok(value)
}

fn required_phase(value: &Value, field: &str) -> Result<SessionLifecyclePhase, String> {
    let raw = required_string(value, field)?;
    SessionLifecyclePhase::from_stable_name(raw)
        .ok_or_else(|| format!("typed chat-container payload has invalid phase '{raw}'"))
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "chat-container event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed chat-container payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed chat-container payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::session_audit::record_local_session_registered;
    use crate::{JsonlEventStore, MemoryEventStore};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const C1: ChatContainerId = ChatContainerId::new(10);
    const C2: ChatContainerId = ChatContainerId::new(20);
    const H1: ContextHandoffId = ContextHandoffId::new(100);
    const H2: ContextHandoffId = ContextHandoffId::new(200);
    const S1: SessionId = SessionId::new(1);
    const S2: SessionId = SessionId::new(2);
    const S3: SessionId = SessionId::new(3);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-chat-container-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn register(store: &mut impl EventStore, session_id: SessionId) {
        record_local_session_registered(store, session_id).unwrap();
    }

    fn transition(
        store: &mut impl EventStore,
        session_id: SessionId,
        from: SessionLifecyclePhase,
        to: SessionLifecyclePhase,
    ) {
        let transition = SessionLifecycleTransition::new(session_id, from, to).unwrap();
        record_chat_session_lifecycle_transition(store, C1, transition).unwrap();
    }

    fn saturate(store: &mut impl EventStore, session_id: SessionId) {
        transition(
            store,
            session_id,
            SessionLifecyclePhase::Healthy,
            SessionLifecyclePhase::Saturated,
        );
    }

    #[test]
    fn rollover_survives_reopen_and_keeps_logical_container_alive() {
        let path = temp_path("reopen", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, S1);
            register(&mut store, S2);
            record_chat_container_created(&mut store, C1, S1).unwrap();
            transition(
                &mut store,
                S1,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Aging,
            );
            transition(
                &mut store,
                S1,
                SessionLifecyclePhase::Aging,
                SessionLifecyclePhase::Saturated,
            );
            record_chat_session_successor_bound(
                &mut store,
                SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
            )
            .unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let record = replay_chat_container_audit(reopened.events())
            .unwrap()
            .remove(0);

        assert_eq!(record.container_id, C1);
        assert_eq!(record.root_session_id, S1);
        assert_eq!(record.current_session_id, S2);
        assert_eq!(record.sessions.len(), 2);
        assert_eq!(record.sessions[0].session_id, S1);
        assert_eq!(record.sessions[0].phase, SessionLifecyclePhase::Retired);
        assert_eq!(record.sessions[0].successor_session_id, Some(S2));
        assert_eq!(record.sessions[0].successor_handoff_id, Some(H1));
        assert_eq!(record.sessions[1].session_id, S2);
        assert_eq!(record.sessions[1].phase, SessionLifecyclePhase::Healthy);
        assert_eq!(record.sessions[1].predecessor_session_id, Some(S1));
        assert!(!record.sessions[0].phase.accepts_ordinary_turns());
        assert!(record.sessions[1].phase.accepts_ordinary_turns());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn successor_requires_saturated_current_leaf() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
        )
        .unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("must be saturated"));
    }

    #[test]
    fn rollover_is_linear_and_cannot_branch_from_retired_predecessor() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        register(&mut store, S3);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        saturate(&mut store, S1);
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
        )
        .unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S3, H2).unwrap(),
        )
        .unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("current leaf"));
    }

    #[test]
    fn successor_must_be_registered_before_rollover() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        saturate(&mut store, S1);
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
        )
        .unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("successor session 2 is not registered"));
    }

    #[test]
    fn one_session_cannot_belong_to_two_chat_containers() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        record_chat_container_created(&mut store, C2, S1).unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("already belongs to chat container"));
    }

    #[test]
    fn stale_phase_transition_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        transition(
            &mut store,
            S1,
            SessionLifecyclePhase::Healthy,
            SessionLifecyclePhase::Aging,
        );
        transition(
            &mut store,
            S1,
            SessionLifecyclePhase::Healthy,
            SessionLifecyclePhase::Saturated,
        );

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("stale chat-session lifecycle transition"));
    }

    #[test]
    fn handoff_identity_cannot_be_reused() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        register(&mut store, S3);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        saturate(&mut store, S1);
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
        )
        .unwrap();
        saturate(&mut store, S2);
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S2, S3, H1).unwrap(),
        )
        .unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("context handoff 100 is already used"));
    }

    #[test]
    fn direct_retirement_without_successor_provenance_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        saturate(&mut store, S1);
        store
            .append_scoped(
                Some(chat_session_lifecycle_scope(C1, S1)),
                EventKind::ChatSessionLifecycleTransitionRecorded,
                json!({
                    "schema": CHAT_CONTAINER_AUDIT_SCHEMA,
                    "version": CHAT_CONTAINER_AUDIT_VERSION,
                    "record": "chat_session_lifecycle_transition_recorded",
                    "container_id": C1.get(),
                    "session_id": S1.get(),
                    "from_phase": "saturated",
                    "to_phase": "retired",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_chat_container_audit(store.events()).unwrap_err();
        assert!(error.contains("invalid session lifecycle transition"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_successor() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, S1);
            register(&mut store, S2);
            record_chat_container_created(&mut store, C1, S1).unwrap();
            saturate(&mut store, S1);
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":5,"kind":"chat_session_successor_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let record = replay_chat_container_audit(reopened.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.current_session_id, S1);
        assert_eq!(record.sessions.len(), 1);
        assert_eq!(record.sessions[0].phase, SessionLifecyclePhase::Saturated);
        assert!(record.sessions[0].phase.successor_required());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_chat_container_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_chat_container_created(&mut store, C1, S1).unwrap();
        saturate(&mut store, S1);
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, H1).unwrap(),
        )
        .unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        assert_eq!(
            projection
                .events_of_kind(EventKind::ChatContainerCreated)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::ChatSessionLifecycleTransitionRecorded)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::ChatSessionSuccessorBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unrelated_events_do_not_create_chat_containers() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(
            replay_chat_container_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }
}
