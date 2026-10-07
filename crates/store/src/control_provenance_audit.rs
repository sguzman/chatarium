//! Durable issuer provenance for admitted worker controls.
//!
//! Issuer provenance identifies whether a control came directly from the user or
//! from a designated controller session. It grants no additional authority.

use crate::control_audit::replay_control_audit;
use crate::session_audit::{replay_session_audit, worker_session_before};
use crate::supervision_audit::replay_supervision_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::control::ControlId;
use chatarium_core::control_provenance::{ControlIssuer, ControlProvenance};
use chatarium_core::session::SessionId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CONTROL_PROVENANCE_SCHEMA: &str = "chatarium-control-provenance-audit";
const CONTROL_PROVENANCE_VERSION: u64 = 1;

/// Restart-replayable issuer provenance for one admitted control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlProvenanceAuditRecord {
    /// Typed control issuer provenance.
    pub provenance: ControlProvenance,
    /// Durable sequence where issuer provenance was recorded.
    pub bound_sequence: u64,
}

/// Append one explicit control issuer binding.
pub fn record_worker_control_issuer_bound(
    store: &mut impl EventStore,
    provenance: ControlProvenance,
) -> std::io::Result<u64> {
    let mut payload = json!({
        "schema": CONTROL_PROVENANCE_SCHEMA,
        "version": CONTROL_PROVENANCE_VERSION,
        "record": "control_issuer_bound",
        "control_id": provenance.control_id().get(),
    });

    match provenance.issuer() {
        ControlIssuer::User => {
            payload
                .as_object_mut()
                .expect("control provenance payload is an object")
                .insert("issuer".to_owned(), json!("user"));
        }
        ControlIssuer::ControllerSession(session_id) => {
            let object = payload
                .as_object_mut()
                .expect("control provenance payload is an object");
            object.insert("issuer".to_owned(), json!("controller_session"));
            object.insert("controller_session_id".to_owned(), json!(session_id.get()));
        }
    }

    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(control_issuer_scope(provenance.control_id())),
        EventKind::WorkerControlIssuerBound,
        encoded,
    )
}

/// Reconstruct all explicit control issuer provenance.
///
/// Replay validates referenced controls and controller-session identity facts.
pub fn replay_control_provenance_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControlProvenanceAuditRecord>, String> {
    let controls = replay_control_audit(events)?;
    let sessions = replay_session_audit(events)?;
    let supervision = replay_supervision_audit(events)?;

    let controls_by_id = controls
        .into_iter()
        .map(|record| (record.control_id, record))
        .collect::<BTreeMap<_, _>>();
    let sessions_by_id = sessions
        .iter()
        .copied()
        .map(|record| (record.session_id, record))
        .collect::<BTreeMap<_, _>>();
    let controller_designated_sequence = supervision
        .controllers
        .iter()
        .map(|record| (record.designation.session_id(), record.designated_sequence))
        .collect::<BTreeMap<_, _>>();
    let supervision_by_worker_session = supervision
        .bindings
        .iter()
        .map(|record| (record.binding.worker_session_id(), *record))
        .collect::<BTreeMap<_, _>>();

    let mut provenance_by_control = BTreeMap::<ControlId, ControlProvenanceAuditRecord>::new();

    for event in events {
        if event.kind != EventKind::WorkerControlIssuerBound {
            continue;
        }

        let payload = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&payload, "control_id")?);
        validate_scope(event, control_id)?;

        if provenance_by_control.contains_key(&control_id) {
            return Err(format!(
                "duplicate issuer provenance for control {} at sequence {}",
                control_id.get(),
                event.sequence
            ));
        }

        let control = controls_by_id.get(&control_id).ok_or_else(|| {
            format!(
                "control issuer provenance at sequence {} references missing admitted control {}",
                event.sequence,
                control_id.get()
            )
        })?;
        if control.admitted_sequence >= event.sequence {
            return Err(format!(
                "control issuer provenance at sequence {} precedes admission of control {} at sequence {}",
                event.sequence,
                control_id.get(),
                control.admitted_sequence
            ));
        }

        let issuer = parse_issuer(&payload)?;

        if let ControlIssuer::ControllerSession(session_id) = issuer {
            let session = sessions_by_id.get(&session_id).ok_or_else(|| {
                format!(
                    "controller issuer session {} is not registered",
                    session_id.get()
                )
            })?;
            if session.registered_sequence >= event.sequence {
                return Err(format!(
                    "control issuer provenance at sequence {} precedes registration of controller session {} at sequence {}",
                    event.sequence,
                    session_id.get(),
                    session.registered_sequence
                ));
            }

            let designated_sequence = controller_designated_sequence
                .get(&session_id)
                .copied()
                .ok_or_else(|| {
                    format!(
                        "controller issuer session {} is not controller-designated",
                        session_id.get()
                    )
                })?;
            if designated_sequence >= event.sequence {
                return Err(format!(
                    "control issuer provenance at sequence {} precedes controller designation at sequence {}",
                    event.sequence, designated_sequence
                ));
            }

            if session.worker_binding.is_some() {
                return Err(format!(
                    "controller issuer session {} is worker-bound",
                    session_id.get()
                ));
            }

            let worker_session =
                worker_session_before(&sessions, control.worker_id, event.sequence).ok_or_else(
                    || {
                        format!(
                            "controller issuer provenance at sequence {} targets worker {} before any active worker-session binding",
                            event.sequence,
                            control.worker_id.get()
                        )
                    },
                )?;
            let worker_bound_sequence = worker_session.worker_bound_sequence.ok_or_else(|| {
                format!(
                    "target worker session {} is missing worker binding sequence",
                    worker_session.session_id.get()
                )
            })?;
            if worker_bound_sequence >= event.sequence {
                return Err(format!(
                    "control issuer provenance at sequence {} precedes target worker-session binding at sequence {}",
                    event.sequence, worker_bound_sequence
                ));
            }

            let supervision = supervision_by_worker_session
                .get(&worker_session.session_id)
                .ok_or_else(|| {
                    format!(
                        "controller issuer session {} did not supervise target worker session {} when control {} was issued",
                        session_id.get(),
                        worker_session.session_id.get(),
                        control_id.get()
                    )
                })?;
            if supervision.binding.controller_session_id() != session_id {
                return Err(format!(
                    "controller issuer session {} does not supervise target worker session {}; supervisor is {}",
                    session_id.get(),
                    worker_session.session_id.get(),
                    supervision.binding.controller_session_id().get()
                ));
            }
            if supervision.bound_sequence >= event.sequence {
                return Err(format!(
                    "control issuer provenance at sequence {} precedes controller-worker supervision at sequence {}",
                    event.sequence, supervision.bound_sequence
                ));
            }
        }

        provenance_by_control.insert(
            control_id,
            ControlProvenanceAuditRecord {
                provenance: ControlProvenance::new(control_id, issuer),
                bound_sequence: event.sequence,
            },
        );
    }

    let mut records = provenance_by_control.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.bound_sequence);
    Ok(records)
}

/// Stable durable scope for one control issuer binding.
#[must_use]
pub fn control_issuer_scope(control_id: ControlId) -> String {
    format!("control-issuer:{}", control_id.get())
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed control provenance payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(CONTROL_PROVENANCE_SCHEMA) {
        return Err(format!(
            "control provenance event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "control provenance event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != CONTROL_PROVENANCE_VERSION {
        return Err(format!(
            "unsupported control provenance payload version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "control_issuer_bound" {
        return Err(format!(
            "control provenance event at sequence {} is not a control_issuer_bound record",
            event.sequence
        ));
    }

    Ok(value)
}

fn parse_issuer(value: &Value) -> Result<ControlIssuer, String> {
    match required_string(value, "issuer")? {
        "user" => {
            if value.get("controller_session_id").is_some() {
                return Err(
                    "user-issued control provenance must not include controller_session_id"
                        .to_owned(),
                );
            }
            Ok(ControlIssuer::User)
        }
        "controller_session" => {
            let session_id = SessionId::new(required_u64(value, "controller_session_id")?);
            Ok(ControlIssuer::ControllerSession(session_id))
        }
        other => Err(format!("unknown control issuer kind '{other}'")),
    }
}

fn validate_scope(event: &EventEnvelope, control_id: ControlId) -> Result<(), String> {
    let expected = control_issuer_scope(control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "control provenance event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed control provenance payload is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed control provenance payload is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_audit::record_worker_control_admitted;
    use crate::projection::SqliteProjection;
    use crate::session_audit::{
        record_local_session_registered, record_worker_session_bound,
        record_worker_session_successor_bound,
    };
    use crate::supervision_audit::{
        record_controller_session_designated, record_controller_worker_bound,
    };
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::control::WorkerControl;
    use chatarium_core::orchestration::{WorkerGoalId, WorkerId, WorkerLifecycle};
    use chatarium_core::session::{WorkerSessionBinding, WorkerSessionSuccessorBinding};
    use chatarium_core::supervision::{ControllerDesignation, ControllerWorkerBinding};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const C1: SessionId = SessionId::new(1);
    const S1: SessionId = SessionId::new(10);
    const S2: SessionId = SessionId::new(11);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-control-provenance-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn admitted_control(id: u64) -> WorkerControl {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(G1).unwrap();
        lifecycle.start_or_resume(G1).unwrap();
        WorkerControl::stop(ControlId::new(id), W1, G1, &lifecycle).unwrap()
    }

    fn record_control(store: &mut impl EventStore, id: u64) -> WorkerControl {
        let control = admitted_control(id);
        record_worker_control_admitted(store, &control).unwrap();
        control
    }

    fn register_controller(store: &mut impl EventStore, session_id: SessionId) {
        record_local_session_registered(store, session_id).unwrap();
        record_controller_session_designated(store, ControllerDesignation::new(session_id))
            .unwrap();
    }

    fn register_worker_session(store: &mut impl EventStore) {
        record_local_session_registered(store, S1).unwrap();
        record_worker_session_bound(store, WorkerSessionBinding::new(W1, S1)).unwrap();
    }

    fn bind_supervision(store: &mut impl EventStore, controller: SessionId) {
        record_controller_worker_bound(
            store,
            ControllerWorkerBinding::new(controller, S1).unwrap(),
        )
        .unwrap();
    }

    fn register_supervised_worker(store: &mut impl EventStore, controller: SessionId) {
        register_worker_session(store);
        bind_supervision(store, controller);
    }

    #[test]
    fn user_and_controller_provenance_survive_reopen() {
        let path = temp_path("reopen", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            let user_control = record_control(&mut store, 1);
            record_worker_control_issuer_bound(
                &mut store,
                ControlProvenance::new(user_control.id(), ControlIssuer::User),
            )
            .unwrap();

            register_controller(&mut store, C1);
            register_supervised_worker(&mut store, C1);
            let controller_control = record_control(&mut store, 2);
            record_worker_control_issuer_bound(
                &mut store,
                ControlProvenance::new(
                    controller_control.id(),
                    ControlIssuer::ControllerSession(C1),
                ),
            )
            .unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_control_provenance_audit(reopened.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].provenance.issuer(), ControlIssuer::User);
        assert_eq!(
            records[1].provenance.issuer(),
            ControlIssuer::ControllerSession(C1)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn duplicate_issuer_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = record_control(&mut store, 1);
        for _ in 0..2 {
            record_worker_control_issuer_bound(
                &mut store,
                ControlProvenance::new(control.id(), ControlIssuer::User),
            )
            .unwrap();
        }

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate issuer provenance"));
    }

    #[test]
    fn provenance_before_control_admission_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(ControlId::new(1), ControlIssuer::User),
        )
        .unwrap();
        let _ = record_control(&mut store, 1);

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("precedes admission"));
    }

    #[test]
    fn malformed_issuer_metadata_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = record_control(&mut store, 1);
        store
            .append_scoped(
                Some(control_issuer_scope(control.id())),
                EventKind::WorkerControlIssuerBound,
                json!({
                    "schema": CONTROL_PROVENANCE_SCHEMA,
                    "version": CONTROL_PROVENANCE_VERSION,
                    "record": "control_issuer_bound",
                    "control_id": control.id().get(),
                    "issuer": "user",
                    "controller_session_id": C1.get(),
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_control_provenance_audit(store.events()).is_err());
    }

    #[test]
    fn missing_controller_session_id_is_rejected() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, C1);
        let control = record_control(&mut store, 1);
        store
            .append_scoped(
                Some(control_issuer_scope(control.id())),
                EventKind::WorkerControlIssuerBound,
                json!({
                    "schema": CONTROL_PROVENANCE_SCHEMA,
                    "version": CONTROL_PROVENANCE_VERSION,
                    "record": "control_issuer_bound",
                    "control_id": control.id().get(),
                    "issuer": "controller_session",
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_control_provenance_audit(store.events()).is_err());
    }

    #[test]
    fn issuer_scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = record_control(&mut store, 1);
        store
            .append_scoped(
                Some(control_issuer_scope(ControlId::new(99))),
                EventKind::WorkerControlIssuerBound,
                json!({
                    "schema": CONTROL_PROVENANCE_SCHEMA,
                    "version": CONTROL_PROVENANCE_VERSION,
                    "record": "control_issuer_bound",
                    "control_id": control.id().get(),
                    "issuer": "user",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn undesignated_controller_issuer_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_local_session_registered(&mut store, C1).unwrap();
        let control = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::ControllerSession(C1)),
        )
        .unwrap();

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("not controller-designated"));
    }

    #[test]
    fn controller_issuer_rejects_supervision_added_after_issuance() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, C1);
        register_worker_session(&mut store);
        let control = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::ControllerSession(C1)),
        )
        .unwrap();
        bind_supervision(&mut store, C1);

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("precedes controller-worker supervision"));
    }

    #[test]
    fn controller_issuer_rejects_worker_binding_added_after_issuance() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, C1);
        record_local_session_registered(&mut store, S1).unwrap();
        let control = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::ControllerSession(C1)),
        )
        .unwrap();
        record_worker_session_bound(&mut store, WorkerSessionBinding::new(W1, S1)).unwrap();
        bind_supervision(&mut store, C1);

        let error = replay_control_provenance_audit(store.events()).unwrap_err();
        assert!(error.contains("precedes target worker-session binding"));
    }

    #[test]
    fn controller_provenance_resolves_worker_session_at_each_event_sequence() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, C1);
        register_supervised_worker(&mut store, C1);

        let first = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(first.id(), ControlIssuer::ControllerSession(C1)),
        )
        .unwrap();

        record_local_session_registered(&mut store, S2).unwrap();
        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(W1, S1, S2).unwrap(),
        )
        .unwrap();
        record_controller_worker_bound(&mut store, ControllerWorkerBinding::new(C1, S2).unwrap())
            .unwrap();

        let second = record_control(&mut store, 2);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(second.id(), ControlIssuer::ControllerSession(C1)),
        )
        .unwrap();

        let records = replay_control_provenance_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].provenance.issuer(),
            ControlIssuer::ControllerSession(C1)
        );
        assert_eq!(
            records[1].provenance.issuer(),
            ControlIssuer::ControllerSession(C1)
        );
    }

    #[test]
    fn direct_user_issuer_does_not_require_supervision() {
        let mut store = MemoryEventStore::default();
        let control = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::User),
        )
        .unwrap();

        let records = replay_control_provenance_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].provenance.issuer(), ControlIssuer::User);
    }

    #[test]
    fn torn_tail_cannot_fabricate_issuer_provenance() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            let _ = record_control(&mut store, 1);
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"worker_control_issuer_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(
            replay_control_provenance_audit(reopened.events())
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_provenance_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        let control = record_control(&mut store, 1);
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::User),
        )
        .unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        assert_eq!(
            projection
                .events_of_kind(EventKind::WorkerControlIssuerBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }
}
