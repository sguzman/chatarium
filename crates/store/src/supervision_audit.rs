//! Durable controller/worker session supervision audit.
//!
//! Supervision is coordination provenance only. It does not grant route-policy
//! authority, continuation authority, dispatch ability, or lifecycle mutation.

use crate::session_audit::{SessionAuditRecord, replay_session_audit};
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::session::SessionId;
use chatarium_core::supervision::{
    ControllerDesignation, ControllerWorkerBinding, SupervisionError,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SUPERVISION_AUDIT_SCHEMA: &str = "chatarium-supervision-audit";
const SUPERVISION_AUDIT_VERSION: u64 = 1;

/// Durable controller designation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerAuditRecord {
    /// Designated controller session.
    pub designation: ControllerDesignation,
    /// Durable designation sequence.
    pub designated_sequence: u64,
}

/// Durable controller-to-worker supervision relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerWorkerAuditRecord {
    /// Directed controller-to-worker session binding.
    pub binding: ControllerWorkerBinding,
    /// Durable binding sequence.
    pub bound_sequence: u64,
}

/// Replayed supervision state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisionAudit {
    /// Explicit controller designations.
    pub controllers: Vec<ControllerAuditRecord>,
    /// Directed controller-to-worker relationships.
    pub bindings: Vec<ControllerWorkerAuditRecord>,
}

/// Append one explicit controller-session designation.
pub fn record_controller_session_designated(
    store: &mut impl EventStore,
    designation: ControllerDesignation,
) -> std::io::Result<u64> {
    let session_id = designation.session_id();
    append_typed(
        store,
        controller_scope(session_id),
        EventKind::ControllerSessionDesignated,
        json!({
            "schema": SUPERVISION_AUDIT_SCHEMA,
            "version": SUPERVISION_AUDIT_VERSION,
            "record": "controller_designated",
            "session_id": session_id.get(),
        }),
    )
}

/// Append one controller-to-worker session relationship.
pub fn record_controller_worker_bound(
    store: &mut impl EventStore,
    binding: ControllerWorkerBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        controller_worker_scope(
            binding.controller_session_id(),
            binding.worker_session_id(),
        ),
        EventKind::ControllerWorkerBound,
        json!({
            "schema": SUPERVISION_AUDIT_SCHEMA,
            "version": SUPERVISION_AUDIT_VERSION,
            "record": "controller_worker_bound",
            "controller_session_id": binding.controller_session_id().get(),
            "worker_session_id": binding.worker_session_id().get(),
        }),
    )
}

/// Reconstruct controller designations and worker relationships.
///
/// Replay validates supervision against the existing durable local-session
/// identity history and rejects role conflicts.
pub fn replay_supervision_audit(events: &[EventEnvelope]) -> Result<SupervisionAudit, String> {
    let sessions = replay_session_audit(events)?;
    let sessions_by_id = sessions
        .into_iter()
        .map(|record| (record.session_id, record))
        .collect::<BTreeMap<_, _>>();

    let mut designated = BTreeMap::<SessionId, u64>::new();
    let mut worker_controller = BTreeMap::<SessionId, SessionId>::new();
    let mut controllers = Vec::new();
    let mut bindings = Vec::new();

    for event in events {
        match event.kind {
            EventKind::ControllerSessionDesignated => {
                let payload = typed_payload(event, "controller_designated")?;
                let session_id = SessionId::new(required_u64(&payload, "session_id")?);
                validate_scope(event, &controller_scope(session_id))?;

                let session = registered_before(&sessions_by_id, session_id, event.sequence)?;

                if session.worker_binding.is_some() {
                    return Err(format!(
                        "session {} cannot be controller-designated because it is worker-bound",
                        session_id.get()
                    ));
                }
                if designated.contains_key(&session_id) {
                    return Err(format!(
                        "duplicate controller designation for session {} at sequence {}",
                        session_id.get(),
                        event.sequence
                    ));
                }

                designated.insert(session_id, event.sequence);
                controllers.push(ControllerAuditRecord {
                    designation: ControllerDesignation::new(session_id),
                    designated_sequence: event.sequence,
                });
            }
            EventKind::ControllerWorkerBound => {
                let payload = typed_payload(event, "controller_worker_bound")?;
                let controller_session_id =
                    SessionId::new(required_u64(&payload, "controller_session_id")?);
                let worker_session_id =
                    SessionId::new(required_u64(&payload, "worker_session_id")?);
                validate_scope(
                    event,
                    &controller_worker_scope(controller_session_id, worker_session_id),
                )?;

                let binding =
                    ControllerWorkerBinding::new(controller_session_id, worker_session_id)
                        .map_err(|error| supervision_error(event.sequence, error))?;

                let controller =
                    registered_before(&sessions_by_id, controller_session_id, event.sequence)?;
                let worker =
                    registered_before(&sessions_by_id, worker_session_id, event.sequence)?;

                if controller.worker_binding.is_some() {
                    return Err(format!(
                        "controller session {} is worker-bound and cannot supervise other sessions",
                        controller_session_id.get()
                    ));
                }

                let designated_sequence = designated
                    .get(&controller_session_id)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "controller-worker binding at sequence {} references undesignated controller session {}",
                            event.sequence,
                            controller_session_id.get()
                        )
                    })?;
                if designated_sequence >= event.sequence {
                    return Err(format!(
                        "controller-worker binding at sequence {} precedes controller designation at sequence {}",
                        event.sequence, designated_sequence
                    ));
                }

                if designated.contains_key(&worker_session_id) {
                    return Err(format!(
                        "worker session {} is controller-designated and cannot be supervision target",
                        worker_session_id.get()
                    ));
                }

                let worker_bound_sequence = worker.worker_bound_sequence.ok_or_else(|| {
                    format!(
                        "supervision target session {} is not worker-bound",
                        worker_session_id.get()
                    )
                })?;
                if worker_bound_sequence >= event.sequence {
                    return Err(format!(
                        "controller-worker binding at sequence {} precedes worker-session binding at sequence {}",
                        event.sequence, worker_bound_sequence
                    ));
                }

                if let Some(existing_controller) = worker_controller.get(&worker_session_id) {
                    return Err(format!(
                        "worker session {} is already supervised by controller {}; cannot also bind controller {} at sequence {}",
                        worker_session_id.get(),
                        existing_controller.get(),
                        controller_session_id.get(),
                        event.sequence
                    ));
                }

                worker_controller.insert(worker_session_id, controller_session_id);
                bindings.push(ControllerWorkerAuditRecord {
                    binding,
                    bound_sequence: event.sequence,
                });
            }
            _ => {}
        }
    }

    Ok(SupervisionAudit {
        controllers,
        bindings,
    })
}

/// Stable scope for a controller designation.
#[must_use]
pub fn controller_scope(session_id: SessionId) -> String {
    format!("controller:{}", session_id.get())
}

/// Stable scope for a controller-to-worker relationship.
#[must_use]
pub fn controller_worker_scope(
    controller_session_id: SessionId,
    worker_session_id: SessionId,
) -> String {
    format!(
        "controller-worker:{}:{}",
        controller_session_id.get(),
        worker_session_id.get()
    )
}

fn registered_before<'a>(
    sessions: &'a BTreeMap<SessionId, SessionAuditRecord>,
    session_id: SessionId,
    event_sequence: u64,
) -> Result<&'a SessionAuditRecord, String> {
    let session = sessions.get(&session_id).ok_or_else(|| {
        format!(
            "supervision event at sequence {} references unregistered session {}",
            event_sequence,
            session_id.get()
        )
    })?;

    if session.registered_sequence >= event_sequence {
        return Err(format!(
            "supervision event at sequence {} precedes registration of session {} at sequence {}",
            event_sequence,
            session_id.get(),
            session.registered_sequence
        ));
    }

    Ok(session)
}

fn append_typed(
    store: &mut impl EventStore,
    scope: String,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(scope), kind, encoded)
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed supervision payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(SUPERVISION_AUDIT_SCHEMA) {
        return Err(format!(
            "supervision event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "supervision event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != SUPERVISION_AUDIT_VERSION {
        return Err(format!(
            "unsupported supervision payload version {version} at sequence {}",
            event.sequence
        ));
    }

    let record = required_string(&value, "record")?;
    if record != expected_record {
        return Err(format!(
            "supervision event at sequence {} has record '{record}', expected '{expected_record}'",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "supervision event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed supervision payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed supervision payload is missing string field '{field}'"))
}

fn supervision_error(sequence: u64, error: SupervisionError) -> String {
    format!("invalid supervision binding at sequence {sequence}: {error:?}")
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::session_audit::{
        record_local_session_registered, record_worker_session_bound,
    };
    use crate::worker_audit::replay_worker_audit;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::orchestration::WorkerId;
    use chatarium_core::session::WorkerSessionBinding;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const C1: SessionId = SessionId::new(1);
    const C2: SessionId = SessionId::new(2);
    const S1: SessionId = SessionId::new(10);
    const S2: SessionId = SessionId::new(20);
    const W1: WorkerId = WorkerId::new(100);
    const W2: WorkerId = WorkerId::new(200);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-supervision-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn register(store: &mut impl EventStore, session_id: SessionId) {
        record_local_session_registered(store, session_id).unwrap();
    }

    fn bind_worker(
        store: &mut impl EventStore,
        worker_id: WorkerId,
        session_id: SessionId,
    ) {
        record_worker_session_bound(
            store,
            WorkerSessionBinding::new(worker_id, session_id),
        )
        .unwrap();
    }

    fn designate(store: &mut impl EventStore, session_id: SessionId) {
        record_controller_session_designated(store, ControllerDesignation::new(session_id)).unwrap();
    }

    fn supervise(
        store: &mut impl EventStore,
        controller: SessionId,
        worker: SessionId,
    ) {
        let binding = ControllerWorkerBinding::new(controller, worker).unwrap();
        record_controller_worker_bound(store, binding).unwrap();
    }

    #[test]
    fn designation_and_binding_survive_reopen() {
        let path = temp_path("reopen", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, C1);
            register(&mut store, S1);
            bind_worker(&mut store, W1, S1);
            designate(&mut store, C1);
            supervise(&mut store, C1, S1);
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let audit = replay_supervision_audit(reopened.events()).unwrap();

        assert_eq!(audit.controllers.len(), 1);
        assert_eq!(
            audit.controllers[0].designation,
            ControllerDesignation::new(C1)
        );
        assert_eq!(audit.controllers[0].designated_sequence, 4);
        assert_eq!(audit.bindings.len(), 1);
        assert_eq!(
            audit.bindings[0].binding,
            ControllerWorkerBinding::new(C1, S1).unwrap()
        );
        assert_eq!(audit.bindings[0].bound_sequence, 5);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn one_controller_can_supervise_multiple_workers() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        register(&mut store, S2);
        bind_worker(&mut store, W1, S1);
        bind_worker(&mut store, W2, S2);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);
        supervise(&mut store, C1, S2);

        let audit = replay_supervision_audit(store.events()).unwrap();
        assert_eq!(audit.controllers.len(), 1);
        assert_eq!(audit.bindings.len(), 2);
    }

    #[test]
    fn one_worker_session_cannot_have_two_controllers() {
        let mut store = MemoryEventStore::default();
        for session in [C1, C2, S1] {
            register(&mut store, session);
        }
        bind_worker(&mut store, W1, S1);
        designate(&mut store, C1);
        designate(&mut store, C2);
        supervise(&mut store, C1, S1);
        supervise(&mut store, C2, S1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("already supervised"));
    }

    #[test]
    fn self_supervision_is_rejected_by_core() {
        assert_eq!(
            ControllerWorkerBinding::new(C1, C1),
            Err(SupervisionError::SelfSupervision { session_id: C1 })
        );
    }

    #[test]
    fn undesignated_controller_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        bind_worker(&mut store, W1, S1);
        supervise(&mut store, C1, S1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("undesignated controller"));
    }

    #[test]
    fn unregistered_controller_is_rejected() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(controller_scope(C1)),
                EventKind::ControllerSessionDesignated,
                json!({
                    "schema": SUPERVISION_AUDIT_SCHEMA,
                    "version": SUPERVISION_AUDIT_VERSION,
                    "record": "controller_designated",
                    "session_id": C1.get(),
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("unregistered session"));
    }

    #[test]
    fn unregistered_worker_session_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("unregistered session"));
    }

    #[test]
    fn non_worker_session_cannot_be_target() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("not worker-bound"));
    }

    #[test]
    fn worker_bound_session_cannot_be_controller() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        bind_worker(&mut store, W1, C1);
        designate(&mut store, C1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("cannot be controller-designated"));
    }

    #[test]
    fn designated_controller_cannot_later_become_worker_bound() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        designate(&mut store, C1);
        bind_worker(&mut store, W1, C1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("cannot be controller-designated"));
    }

    #[test]
    fn duplicate_designation_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        designate(&mut store, C1);
        designate(&mut store, C1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate controller designation"));
    }

    #[test]
    fn duplicate_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        bind_worker(&mut store, W1, S1);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);
        supervise(&mut store, C1, S1);

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("already supervised"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_supervision_binding() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            register(&mut store, C1);
            register(&mut store, S1);
            bind_worker(&mut store, W1, S1);
            designate(&mut store, C1);
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":5,"kind":"controller_worker_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let audit = replay_supervision_audit(reopened.events()).unwrap();
        assert_eq!(audit.controllers.len(), 1);
        assert!(audit.bindings.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_record_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(controller_scope(C1)),
                EventKind::ControllerSessionDesignated,
                json!({
                    "schema": SUPERVISION_AUDIT_SCHEMA,
                    "version": SUPERVISION_AUDIT_VERSION,
                    "record": "controller_designated",
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_supervision_audit(store.events()).is_err());
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        store
            .append_scoped(
                Some(controller_scope(C2)),
                EventKind::ControllerSessionDesignated,
                json!({
                    "schema": SUPERVISION_AUDIT_SCHEMA,
                    "version": SUPERVISION_AUDIT_VERSION,
                    "record": "controller_designated",
                    "session_id": C1.get(),
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_supervision_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        let audit = replay_supervision_audit(store.events()).unwrap();
        assert!(audit.controllers.is_empty());
        assert!(audit.bindings.is_empty());
    }

    #[test]
    fn generic_sqlite_projection_carries_supervision_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        bind_worker(&mut store, W1, S1);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        assert_eq!(
            projection
                .events_of_kind(EventKind::ControllerSessionDesignated)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::ControllerWorkerBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn supervision_replay_does_not_fabricate_worker_lifecycle_or_authority() {
        let mut store = MemoryEventStore::default();
        register(&mut store, C1);
        register(&mut store, S1);
        bind_worker(&mut store, W1, S1);
        designate(&mut store, C1);
        supervise(&mut store, C1, S1);

        let audit = replay_supervision_audit(store.events()).unwrap();
        assert_eq!(audit.bindings.len(), 1);

        let workers = replay_worker_audit(store.events()).unwrap();
        assert!(workers.is_empty());

        assert!(!store.events().iter().any(|event| {
            matches!(
                event.kind,
                EventKind::RouteProposed
                    | EventKind::RouteUserDecisionRecorded
                    | EventKind::RouteDispatched
                    | EventKind::WorkerControlAdmitted
            )
        }));

    }
}
