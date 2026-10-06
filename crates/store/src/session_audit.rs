//! Durable local session identity and binding audit.
//!
//! Session identity is local control-plane provenance. It is deliberately distinct
//! from remote ChatGPT identity, routing state, and worker lifecycle state.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::orchestration::WorkerId;
use chatarium_core::routing::RouteEndpointId;
use chatarium_core::session::{
    SessionEndpointBinding, SessionId, WorkerSessionBinding, WorkerSessionSuccessorBinding,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SESSION_AUDIT_SCHEMA: &str = "chatarium-session-audit";
const SESSION_AUDIT_VERSION: u64 = 1;

/// Restart-replayable local session record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAuditRecord {
    /// Local session identity.
    pub session_id: SessionId,
    /// Durable registration sequence.
    pub registered_sequence: u64,
    /// Optional routing-endpoint correlation.
    pub endpoint_binding: Option<SessionEndpointBinding>,
    /// Durable sequence of endpoint binding, if present.
    pub endpoint_bound_sequence: Option<u64>,
    /// Optional worker correlation.
    pub worker_binding: Option<WorkerSessionBinding>,
    /// Durable sequence of worker binding, if present.
    pub worker_bound_sequence: Option<u64>,
    /// Last session-identity event applied.
    pub last_sequence: u64,
}

/// Append one local session registration.
pub fn record_local_session_registered(
    store: &mut impl EventStore,
    session_id: SessionId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(session_scope(session_id)),
        EventKind::LocalSessionRegistered,
        json!({
            "schema": SESSION_AUDIT_SCHEMA,
            "version": SESSION_AUDIT_VERSION,
            "record": "session_registered",
            "session_id": session_id.get(),
        }),
    )
}

/// Append one session-to-routing-endpoint correlation.
pub fn record_session_endpoint_bound(
    store: &mut impl EventStore,
    binding: SessionEndpointBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(session_endpoint_scope(
            binding.session_id(),
            binding.endpoint_id(),
        )),
        EventKind::SessionEndpointBound,
        json!({
            "schema": SESSION_AUDIT_SCHEMA,
            "version": SESSION_AUDIT_VERSION,
            "record": "session_endpoint_bound",
            "session_id": binding.session_id().get(),
            "endpoint_id": binding.endpoint_id().get(),
        }),
    )
}

/// Append one worker-to-local-session correlation.
pub fn record_worker_session_bound(
    store: &mut impl EventStore,
    binding: WorkerSessionBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(worker_session_scope(
            binding.worker_id(),
            binding.session_id(),
        )),
        EventKind::WorkerSessionBound,
        json!({
            "schema": SESSION_AUDIT_SCHEMA,
            "version": SESSION_AUDIT_VERSION,
            "record": "worker_session_bound",
            "worker_id": binding.worker_id().get(),
            "session_id": binding.session_id().get(),
        }),
    )
}

/// Append one explicit worker-session successor handoff.
pub fn record_worker_session_successor_bound(
    store: &mut impl EventStore,
    binding: WorkerSessionSuccessorBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(worker_session_successor_scope(
            binding.worker_id(),
            binding.predecessor_session_id(),
            binding.successor_session_id(),
        )),
        EventKind::WorkerSessionSuccessorBound,
        json!({
            "schema": SESSION_AUDIT_SCHEMA,
            "version": SESSION_AUDIT_VERSION,
            "record": "worker_session_successor_bound",
            "worker_id": binding.worker_id().get(),
            "predecessor_session_id": binding.predecessor_session_id().get(),
            "successor_session_id": binding.successor_session_id().get(),
        }),
    )
}

/// Reconstruct local sessions and their identity bindings from durable journal events.
pub fn replay_session_audit(events: &[EventEnvelope]) -> Result<Vec<SessionAuditRecord>, String> {
    let mut sessions = BTreeMap::<SessionId, ReplaySession>::new();
    let mut endpoint_owner = BTreeMap::<RouteEndpointId, SessionId>::new();
    let mut worker_owner = BTreeMap::<WorkerId, SessionId>::new();

    for event in events {
        match event.kind {
            EventKind::LocalSessionRegistered => replay_registration(&mut sessions, event)?,
            EventKind::SessionEndpointBound => {
                replay_endpoint_binding(&mut sessions, &mut endpoint_owner, event)?
            }
            EventKind::WorkerSessionBound => {
                replay_worker_binding(&mut sessions, &mut worker_owner, event)?
            }
            EventKind::WorkerSessionSuccessorBound => {
                replay_worker_successor_binding(&mut sessions, &mut worker_owner, event)?
            }
            _ => {}
        }
    }

    let mut records = sessions
        .into_values()
        .map(ReplaySession::into_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.registered_sequence);
    Ok(records)
}

struct ReplaySession {
    session_id: SessionId,
    registered_sequence: u64,
    endpoint_binding: Option<SessionEndpointBinding>,
    endpoint_bound_sequence: Option<u64>,
    worker_binding: Option<WorkerSessionBinding>,
    worker_bound_sequence: Option<u64>,
    last_sequence: u64,
}

impl ReplaySession {
    fn into_record(self) -> SessionAuditRecord {
        SessionAuditRecord {
            session_id: self.session_id,
            registered_sequence: self.registered_sequence,
            endpoint_binding: self.endpoint_binding,
            endpoint_bound_sequence: self.endpoint_bound_sequence,
            worker_binding: self.worker_binding,
            worker_bound_sequence: self.worker_bound_sequence,
            last_sequence: self.last_sequence,
        }
    }
}

fn replay_registration(
    sessions: &mut BTreeMap<SessionId, ReplaySession>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "session_registered")?;
    let session_id = SessionId::new(required_u64(&payload, "session_id")?);
    validate_scope(event, &session_scope(session_id))?;

    if sessions.contains_key(&session_id) {
        return Err(format!(
            "duplicate local session registration {} at sequence {}",
            session_id.get(),
            event.sequence
        ));
    }

    sessions.insert(
        session_id,
        ReplaySession {
            session_id,
            registered_sequence: event.sequence,
            endpoint_binding: None,
            endpoint_bound_sequence: None,
            worker_binding: None,
            worker_bound_sequence: None,
            last_sequence: event.sequence,
        },
    );
    Ok(())
}

fn replay_endpoint_binding(
    sessions: &mut BTreeMap<SessionId, ReplaySession>,
    endpoint_owner: &mut BTreeMap<RouteEndpointId, SessionId>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "session_endpoint_bound")?;
    let session_id = SessionId::new(required_u64(&payload, "session_id")?);
    let endpoint_id = RouteEndpointId::new(required_u64(&payload, "endpoint_id")?);
    validate_scope(event, &session_endpoint_scope(session_id, endpoint_id))?;

    let session = sessions.get_mut(&session_id).ok_or_else(|| {
        format!(
            "session endpoint binding at sequence {} references unregistered session {}",
            event.sequence,
            session_id.get()
        )
    })?;

    if let Some(existing) = session.endpoint_binding {
        return Err(format!(
            "session {} is already bound to endpoint {}; cannot also bind endpoint {} at sequence {}",
            session_id.get(),
            existing.endpoint_id().get(),
            endpoint_id.get(),
            event.sequence
        ));
    }
    if let Some(existing_session) = endpoint_owner.get(&endpoint_id) {
        return Err(format!(
            "endpoint {} is already bound to session {}; cannot also bind session {} at sequence {}",
            endpoint_id.get(),
            existing_session.get(),
            session_id.get(),
            event.sequence
        ));
    }

    let binding = SessionEndpointBinding::new(session_id, endpoint_id);
    session.endpoint_binding = Some(binding);
    session.endpoint_bound_sequence = Some(event.sequence);
    session.last_sequence = event.sequence;
    endpoint_owner.insert(endpoint_id, session_id);
    Ok(())
}

fn replay_worker_binding(
    sessions: &mut BTreeMap<SessionId, ReplaySession>,
    worker_owner: &mut BTreeMap<WorkerId, SessionId>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "worker_session_bound")?;
    let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
    let session_id = SessionId::new(required_u64(&payload, "session_id")?);
    validate_scope(event, &worker_session_scope(worker_id, session_id))?;

    let session = sessions.get_mut(&session_id).ok_or_else(|| {
        format!(
            "worker session binding at sequence {} references unregistered session {}",
            event.sequence,
            session_id.get()
        )
    })?;

    if let Some(existing) = session.worker_binding {
        return Err(format!(
            "session {} is already bound to worker {}; cannot also bind worker {} at sequence {}",
            session_id.get(),
            existing.worker_id().get(),
            worker_id.get(),
            event.sequence
        ));
    }
    if let Some(existing_session) = worker_owner.get(&worker_id) {
        return Err(format!(
            "worker {} is already bound to session {}; cannot also bind session {} at sequence {}",
            worker_id.get(),
            existing_session.get(),
            session_id.get(),
            event.sequence
        ));
    }

    let binding = WorkerSessionBinding::new(worker_id, session_id);
    session.worker_binding = Some(binding);
    session.worker_bound_sequence = Some(event.sequence);
    session.last_sequence = event.sequence;
    worker_owner.insert(worker_id, session_id);
    Ok(())
}

fn replay_worker_successor_binding(
    sessions: &mut BTreeMap<SessionId, ReplaySession>,
    worker_owner: &mut BTreeMap<WorkerId, SessionId>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "worker_session_successor_bound")?;
    let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
    let predecessor_session_id =
        SessionId::new(required_u64(&payload, "predecessor_session_id")?);
    let successor_session_id =
        SessionId::new(required_u64(&payload, "successor_session_id")?);
    validate_scope(
        event,
        &worker_session_successor_scope(
            worker_id,
            predecessor_session_id,
            successor_session_id,
        ),
    )?;

    let binding = WorkerSessionSuccessorBinding::new(
        worker_id,
        predecessor_session_id,
        successor_session_id,
    )
    .map_err(|error| {
        format!(
            "invalid worker-session successor binding at sequence {}: {error:?}",
            event.sequence
        )
    })?;

    let active_session = worker_owner.get(&worker_id).copied().ok_or_else(|| {
        format!(
            "worker-session successor binding at sequence {} references worker {} before any worker-session binding",
            event.sequence,
            worker_id.get()
        )
    })?;
    if active_session != predecessor_session_id {
        return Err(format!(
            "worker-session successor binding at sequence {} names predecessor {}, but worker {} is active on session {}",
            event.sequence,
            predecessor_session_id.get(),
            worker_id.get(),
            active_session.get()
        ));
    }

    let predecessor = sessions.get(&predecessor_session_id).ok_or_else(|| {
        format!(
            "worker-session successor binding at sequence {} references missing predecessor session {}",
            event.sequence,
            predecessor_session_id.get()
        )
    })?;
    if predecessor.worker_binding
        != Some(WorkerSessionBinding::new(worker_id, predecessor_session_id))
    {
        return Err(format!(
            "predecessor session {} is not historically bound to worker {} at sequence {}",
            predecessor_session_id.get(),
            worker_id.get(),
            event.sequence
        ));
    }

    let successor = sessions.get_mut(&successor_session_id).ok_or_else(|| {
        format!(
            "worker-session successor binding at sequence {} references unregistered successor session {}",
            event.sequence,
            successor_session_id.get()
        )
    })?;
    if let Some(existing) = successor.worker_binding {
        return Err(format!(
            "successor session {} is already bound to worker {}; cannot receive worker {} at sequence {}",
            successor_session_id.get(),
            existing.worker_id().get(),
            worker_id.get(),
            event.sequence
        ));
    }

    successor.worker_binding = Some(WorkerSessionBinding::new(
        binding.worker_id(),
        binding.successor_session_id(),
    ));
    successor.worker_bound_sequence = Some(event.sequence);
    successor.last_sequence = event.sequence;
    worker_owner.insert(worker_id, successor_session_id);
    Ok(())
}

/// Resolve the worker-session binding active immediately before one durable sequence.
///
/// Historical predecessor bindings remain in session records; the binding with
/// the greatest durable bind/handoff sequence before `before_sequence` is the
/// active execution leaf for that worker at that point in history.
#[must_use]
pub fn worker_session_before(
    sessions: &[SessionAuditRecord],
    worker_id: WorkerId,
    before_sequence: u64,
) -> Option<SessionAuditRecord> {
    sessions
        .iter()
        .copied()
        .filter(|record| {
            record
                .worker_binding
                .is_some_and(|binding| binding.worker_id() == worker_id)
                && record
                    .worker_bound_sequence
                    .is_some_and(|sequence| sequence < before_sequence)
        })
        .max_by_key(|record| record.worker_bound_sequence)
}

/// Resolve the currently active session leaf for one worker identity.
#[must_use]
pub fn active_worker_session(
    sessions: &[SessionAuditRecord],
    worker_id: WorkerId,
) -> Option<SessionAuditRecord> {
    sessions
        .iter()
        .copied()
        .filter(|record| {
            record
                .worker_binding
                .is_some_and(|binding| binding.worker_id() == worker_id)
        })
        .max_by_key(|record| record.worker_bound_sequence)
}

/// Stable scope for one local session registration.
#[must_use]
pub fn session_scope(session_id: SessionId) -> String {
    format!("session:{}", session_id.get())
}

/// Stable scope for one session-to-endpoint binding.
#[must_use]
pub fn session_endpoint_scope(session_id: SessionId, endpoint_id: RouteEndpointId) -> String {
    format!(
        "session-endpoint:{}:{}",
        session_id.get(),
        endpoint_id.get()
    )
}

/// Stable scope for one worker-to-session binding.
#[must_use]
pub fn worker_session_scope(worker_id: WorkerId, session_id: SessionId) -> String {
    format!("worker-session:{}:{}", worker_id.get(), session_id.get())
}

/// Stable scope for one worker identity handoff between session leaves.
#[must_use]
pub fn worker_session_successor_scope(
    worker_id: WorkerId,
    predecessor_session_id: SessionId,
    successor_session_id: SessionId,
) -> String {
    format!(
        "worker-session-successor:{}:{}:{}",
        worker_id.get(),
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
            "malformed typed session payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(SESSION_AUDIT_SCHEMA) {
        return Err(format!(
            "session event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "session event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != SESSION_AUDIT_VERSION {
        return Err(format!(
            "unsupported session audit payload version {version} at sequence {}",
            event.sequence
        ));
    }

    let record = required_string(&value, "record")?;
    if record != expected_record {
        return Err(format!(
            "session event at sequence {} has record '{record}', expected '{expected_record}'",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "session event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed session payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed session payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::worker_audit::replay_worker_audit;
    use crate::{JsonlEventStore, MemoryEventStore};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const S1: SessionId = SessionId::new(1);
    const S2: SessionId = SessionId::new(2);
    const E1: RouteEndpointId = RouteEndpointId::new(10);
    const E2: RouteEndpointId = RouteEndpointId::new(20);
    const W1: WorkerId = WorkerId::new(100);
    const W2: WorkerId = WorkerId::new(200);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-session-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn register(store: &mut impl EventStore, session_id: SessionId) {
        record_local_session_registered(store, session_id).unwrap();
    }

    #[test]
    fn registration_and_bindings_survive_reopen() {
        let path = temp_path("reopen", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, S1);
            record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();
            record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_session_audit(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, S1);
        assert_eq!(
            records[0].endpoint_binding,
            Some(SessionEndpointBinding::new(S1, E1))
        );
        assert_eq!(
            records[0].worker_binding,
            Some(WorkerSessionBinding::new(W1, S1))
        );
        assert_eq!(records[0].registered_sequence, 1);
        assert_eq!(records[0].endpoint_bound_sequence, Some(2));
        assert_eq!(records[0].worker_bound_sequence, Some(3));
        assert_eq!(records[0].last_sequence, 3);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sessions_replay_independently_in_registration_order() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S2, E2)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();

        let records = replay_session_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].session_id, S1);
        assert_eq!(records[1].session_id, S2);
        assert_eq!(
            records[0].worker_binding,
            Some(WorkerSessionBinding::new(W1, S1))
        );
        assert_eq!(
            records[1].endpoint_binding,
            Some(SessionEndpointBinding::new(S2, E2))
        );
    }

    #[test]
    fn session_cannot_bind_to_two_endpoints() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E2)).unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to endpoint"));
    }

    #[test]
    fn endpoint_cannot_bind_to_two_sessions() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S2, E1)).unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to session"));
    }

    #[test]
    fn worker_cannot_bind_to_two_sessions() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S2)).unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to session"));
    }

    #[test]
    fn session_cannot_bind_to_two_workers() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W2, S1)).unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to worker"));
    }

    #[test]
    fn explicit_successor_moves_active_worker_without_erasing_history() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        let first_binding_sequence = store.events().last().unwrap().sequence;

        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(W1, S1, S2).unwrap(),
        )
        .unwrap();
        let handoff_sequence = store.events().last().unwrap().sequence;

        let records = replay_session_audit(store.events()).unwrap();
        let predecessor = records
            .iter()
            .find(|record| record.session_id == S1)
            .copied()
            .unwrap();
        let successor = records
            .iter()
            .find(|record| record.session_id == S2)
            .copied()
            .unwrap();

        assert_eq!(
            predecessor.worker_binding,
            Some(WorkerSessionBinding::new(W1, S1))
        );
        assert_eq!(predecessor.worker_bound_sequence, Some(first_binding_sequence));
        assert_eq!(
            successor.worker_binding,
            Some(WorkerSessionBinding::new(W1, S2))
        );
        assert_eq!(successor.worker_bound_sequence, Some(handoff_sequence));
        assert_eq!(active_worker_session(&records, W1).unwrap().session_id, S2);
        assert_eq!(
            worker_session_before(&records, W1, handoff_sequence)
                .unwrap()
                .session_id,
            S1
        );
        assert_eq!(
            worker_session_before(&records, W1, handoff_sequence + 1)
                .unwrap()
                .session_id,
            S2
        );
    }

    #[test]
    fn successor_requires_current_predecessor() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        let s3 = SessionId::new(3);
        register(&mut store, s3);
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(W1, S1, S2).unwrap(),
        )
        .unwrap();
        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(W1, S1, s3).unwrap(),
        )
        .unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("names predecessor"));
        assert!(error.contains("active on session"));
    }

    #[test]
    fn successor_cannot_target_already_worker_bound_session() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S2);
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W2, S2)).unwrap();
        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(W1, S1, S2).unwrap(),
        )
        .unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to worker"));
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        register(&mut store, S1);

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate local session registration"));
    }

    #[test]
    fn duplicate_bindings_are_rejected() {
        let mut endpoint_store = MemoryEventStore::default();
        register(&mut endpoint_store, S1);
        record_session_endpoint_bound(&mut endpoint_store, SessionEndpointBinding::new(S1, E1))
            .unwrap();
        record_session_endpoint_bound(&mut endpoint_store, SessionEndpointBinding::new(S1, E1))
            .unwrap();
        assert!(replay_session_audit(endpoint_store.events()).is_err());

        let mut worker_store = MemoryEventStore::default();
        register(&mut worker_store, S1);
        record_worker_session_bound(&mut worker_store, WorkerSessionBinding::new(W1, S1)).unwrap();
        record_worker_session_bound(&mut worker_store, WorkerSessionBinding::new(W1, S1)).unwrap();
        assert!(replay_session_audit(worker_store.events()).is_err());
    }

    #[test]
    fn bindings_before_registration_are_rejected() {
        let mut endpoint_store = MemoryEventStore::default();
        record_session_endpoint_bound(&mut endpoint_store, SessionEndpointBinding::new(S1, E1))
            .unwrap();
        let endpoint_error = replay_session_audit(endpoint_store.events()).unwrap_err();
        assert!(endpoint_error.contains("unregistered session"));

        let mut worker_store = MemoryEventStore::default();
        record_worker_session_bound(&mut worker_store, WorkerSessionBinding::new(W1, S1)).unwrap();
        let worker_error = replay_session_audit(worker_store.events()).unwrap_err();
        assert!(worker_error.contains("unregistered session"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_session_binding() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, S1);
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"session_endpoint_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let record = replay_session_audit(reopened.events()).unwrap().remove(0);
        assert_eq!(record.endpoint_binding, None);
        assert_eq!(record.worker_binding, None);
        assert_eq!(record.last_sequence, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_record_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(session_scope(S1)),
                EventKind::LocalSessionRegistered,
                json!({
                    "schema": SESSION_AUDIT_SCHEMA,
                    "version": SESSION_AUDIT_VERSION,
                    "record": "session_registered",
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_session_audit(store.events()).is_err());
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(session_scope(S2)),
                EventKind::LocalSessionRegistered,
                json!({
                    "schema": SESSION_AUDIT_SCHEMA,
                    "version": SESSION_AUDIT_VERSION,
                    "record": "session_registered",
                    "session_id": S1.get(),
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_session_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(replay_session_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn generic_sqlite_projection_carries_session_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        assert_eq!(
            projection
                .events_of_kind(EventKind::LocalSessionRegistered)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::SessionEndpointBound)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::WorkerSessionBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn session_replay_does_not_fabricate_worker_lifecycle_or_remote_state() {
        let mut store = MemoryEventStore::default();
        register(&mut store, S1);
        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();

        let sessions = replay_session_audit(store.events()).unwrap();
        assert_eq!(sessions.len(), 1);

        let workers = replay_worker_audit(store.events()).unwrap();
        assert!(workers.is_empty());

        // No route proposal exists merely because an endpoint identity is bound.
        assert!(
            !store
                .events()
                .iter()
                .any(|event| event.kind == EventKind::RouteProposed)
        );
    }
}
