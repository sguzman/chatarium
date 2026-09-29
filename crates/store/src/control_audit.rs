//! Durable audit records for locally admitted worker-control commands.
//!
//! Admission is not dispatch, delivery, execution, or lifecycle evidence. This
//! module records only the fact that Chatarium admitted a typed control locally.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::control::{ControlId, WorkerControl, WorkerControlKind};
use chatarium_core::orchestration::{WorkerGoalId, WorkerId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CONTROL_AUDIT_SCHEMA: &str = "chatarium-control-audit";
const CONTROL_AUDIT_VERSION: u64 = 1;

/// Restart-replayable fact that one typed worker control was admitted locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlAuditRecord {
    /// Local control identity.
    pub control_id: ControlId,
    /// Target worker identity.
    pub worker_id: WorkerId,
    /// Target goal identity.
    pub goal_id: WorkerGoalId,
    /// Admitted semantic control kind.
    pub kind: WorkerControlKind,
    /// Durable sequence of the admission record.
    pub admitted_sequence: u64,
}

/// Append one locally admitted worker control.
///
/// This function takes a borrowed admitted control. It records no transport fact
/// and does not mutate worker lifecycle state.
pub fn record_worker_control_admitted(
    store: &mut impl EventStore,
    control: &WorkerControl,
) -> std::io::Result<u64> {
    let (kind, permit_ordinal) = encode_kind(control.kind());
    let mut payload = json!({
        "schema": CONTROL_AUDIT_SCHEMA,
        "version": CONTROL_AUDIT_VERSION,
        "record": "control_admitted",
        "control_id": control.id().get(),
        "worker_id": control.worker_id().get(),
        "goal_id": control.goal_id().get(),
        "kind": kind,
    });

    if let Some(ordinal) = permit_ordinal {
        payload
            .as_object_mut()
            .expect("control audit payload is an object")
            .insert("permit_ordinal".to_owned(), json!(ordinal));
    }

    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(control_scope(control.id())),
        EventKind::WorkerControlAdmitted,
        encoded,
    )
}

/// Reconstruct all admitted-control facts from authoritative journal events.
///
/// Unrelated events are ignored. Replay intentionally does not inspect or mutate
/// worker lifecycle state.
pub fn replay_control_audit(events: &[EventEnvelope]) -> Result<Vec<ControlAuditRecord>, String> {
    let mut controls = BTreeMap::<ControlId, ControlAuditRecord>::new();

    for event in events {
        if event.kind != EventKind::WorkerControlAdmitted {
            continue;
        }

        let payload = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&payload, "control_id")?);
        validate_scope(event, control_id)?;

        if controls.contains_key(&control_id) {
            return Err(format!(
                "duplicate admitted worker control {} at sequence {}",
                control_id.get(),
                event.sequence
            ));
        }

        let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
        let goal_id = WorkerGoalId::new(required_u64(&payload, "goal_id")?);
        let kind = decode_kind(&payload)?;

        controls.insert(
            control_id,
            ControlAuditRecord {
                control_id,
                worker_id,
                goal_id,
                kind,
                admitted_sequence: event.sequence,
            },
        );
    }

    let mut records = controls.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.admitted_sequence);
    Ok(records)
}

/// Stable journal scope for one admitted control.
#[must_use]
pub fn control_scope(control_id: ControlId) -> String {
    format!("control:{}", control_id.get())
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed control payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(CONTROL_AUDIT_SCHEMA) {
        return Err(format!(
            "control event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "control event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != CONTROL_AUDIT_VERSION {
        return Err(format!(
            "unsupported control audit payload version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "control_admitted" {
        return Err(format!(
            "control event at sequence {} is not a control_admitted record",
            event.sequence
        ));
    }

    Ok(value)
}

fn encode_kind(kind: WorkerControlKind) -> (&'static str, Option<u32>) {
    match kind {
        WorkerControlKind::StartOrResume => ("start_or_resume", None),
        WorkerControlKind::Continue { permit_ordinal } => ("continue", Some(permit_ordinal)),
        WorkerControlKind::Stop => ("stop", None),
        WorkerControlKind::StatusRequest => ("status_request", None),
    }
}

fn decode_kind(value: &Value) -> Result<WorkerControlKind, String> {
    let kind = required_string(value, "kind")?;
    let ordinal = value.get("permit_ordinal");

    match kind {
        "continue" => {
            let permit_ordinal = ordinal
                .and_then(Value::as_u64)
                .ok_or_else(|| "continue control is missing integer permit_ordinal".to_owned())?;
            let permit_ordinal = u32::try_from(permit_ordinal)
                .map_err(|_| "continue permit_ordinal exceeds u32".to_owned())?;
            if permit_ordinal == 0 {
                return Err("continue permit_ordinal must be greater than zero".to_owned());
            }
            Ok(WorkerControlKind::Continue { permit_ordinal })
        }
        "start_or_resume" | "stop" | "status_request" => {
            if ordinal.is_some() {
                return Err(format!(
                    "non-continue control kind '{kind}' must not carry permit_ordinal"
                ));
            }
            Ok(match kind {
                "start_or_resume" => WorkerControlKind::StartOrResume,
                "stop" => WorkerControlKind::Stop,
                "status_request" => WorkerControlKind::StatusRequest,
                _ => unreachable!(),
            })
        }
        other => Err(format!("unknown admitted worker control kind '{other}'")),
    }
}

fn validate_scope(event: &EventEnvelope, control_id: ControlId) -> Result<(), String> {
    let expected = control_scope(control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "control event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed control payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed control payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JsonlEventStore, MemoryEventStore, projection::SqliteProjection};
    use chatarium_core::control::WorkerControl;
    use chatarium_core::orchestration::{ContinuationLease, WorkerLifecycle, WorkerPhase};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const G1: WorkerGoalId = WorkerGoalId::new(100);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-control-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn ready() -> WorkerLifecycle {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(G1).unwrap();
        lifecycle
    }

    fn working() -> WorkerLifecycle {
        let mut lifecycle = ready();
        lifecycle.start_or_resume(G1).unwrap();
        lifecycle
    }

    fn controls() -> Vec<WorkerControl> {
        let ready_lifecycle = ready();
        let working_lifecycle = working();

        let start =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &ready_lifecycle).unwrap();

        let mut lease = ContinuationLease::new(G1, 1);
        let permit = lease.authorize(&working_lifecycle).unwrap();
        let continue_control =
            WorkerControl::continue_work(ControlId::new(2), W1, &working_lifecycle, permit).unwrap();

        let stop =
            WorkerControl::stop(ControlId::new(3), W1, G1, &working_lifecycle).unwrap();

        let status =
            WorkerControl::status_request(ControlId::new(4), W1, G1, &working_lifecycle).unwrap();

        vec![start, continue_control, stop, status]
    }

    fn reopen(path: &PathBuf) -> Vec<ControlAuditRecord> {
        let store = JsonlEventStore::open(path).unwrap();
        replay_control_audit(store.events()).unwrap()
    }

    #[test]
    fn every_control_kind_survives_reopen() {
        let path = temp_path("kinds", "jsonl");
        let commands = controls();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            for command in &commands {
                record_worker_control_admitted(&mut store, command).unwrap();
            }
        }

        let records = reopen(&path);
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].kind, WorkerControlKind::StartOrResume);
        assert_eq!(
            records[1].kind,
            WorkerControlKind::Continue { permit_ordinal: 1 }
        );
        assert_eq!(records[2].kind, WorkerControlKind::Stop);
        assert_eq!(records[3].kind, WorkerControlKind::StatusRequest);
        assert_eq!(
            records
                .iter()
                .map(|record| record.admitted_sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn admitted_control_preserves_local_identity() {
        let mut store = MemoryEventStore::default();
        let command = controls().remove(0);
        record_worker_control_admitted(&mut store, &command).unwrap();

        let record = replay_control_audit(store.events()).unwrap().remove(0);
        assert_eq!(record.control_id, command.id());
        assert_eq!(record.worker_id, W1);
        assert_eq!(record.goal_id, G1);
    }

    #[test]
    fn duplicate_control_id_is_rejected() {
        let mut store = MemoryEventStore::default();
        let lifecycle = ready();
        let first =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        let second =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();

        record_worker_control_admitted(&mut store, &first).unwrap();
        record_worker_control_admitted(&mut store, &second).unwrap();

        let error = replay_control_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate admitted worker control"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_control() {
        let path = temp_path("torn-tail", "jsonl");
        let command = controls().remove(0);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_worker_control_admitted(&mut store, &command).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"worker_control_admitted""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let records = reopen(&path);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].control_id, ControlId::new(1));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_payload_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(control_scope(ControlId::new(1))),
                EventKind::WorkerControlAdmitted,
                json!({
                    "schema": CONTROL_AUDIT_SCHEMA,
                    "version": CONTROL_AUDIT_VERSION,
                    "record": "control_admitted",
                    "control_id": 1,
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_control_audit(store.events()).is_err());
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(control_scope(ControlId::new(99))),
                EventKind::WorkerControlAdmitted,
                json!({
                    "schema": CONTROL_AUDIT_SCHEMA,
                    "version": CONTROL_AUDIT_VERSION,
                    "record": "control_admitted",
                    "control_id": 1,
                    "worker_id": W1.get(),
                    "goal_id": G1.get(),
                    "kind": "stop",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_control_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn invalid_continuation_metadata_is_rejected() {
        for payload in [
            json!({
                "schema": CONTROL_AUDIT_SCHEMA,
                "version": CONTROL_AUDIT_VERSION,
                "record": "control_admitted",
                "control_id": 1,
                "worker_id": W1.get(),
                "goal_id": G1.get(),
                "kind": "continue",
            }),
            json!({
                "schema": CONTROL_AUDIT_SCHEMA,
                "version": CONTROL_AUDIT_VERSION,
                "record": "control_admitted",
                "control_id": 1,
                "worker_id": W1.get(),
                "goal_id": G1.get(),
                "kind": "continue",
                "permit_ordinal": 0,
            }),
            json!({
                "schema": CONTROL_AUDIT_SCHEMA,
                "version": CONTROL_AUDIT_VERSION,
                "record": "control_admitted",
                "control_id": 1,
                "worker_id": W1.get(),
                "goal_id": G1.get(),
                "kind": "stop",
                "permit_ordinal": 1,
            }),
        ] {
            let mut store = MemoryEventStore::default();
            store
                .append_scoped(
                    Some(control_scope(ControlId::new(1))),
                    EventKind::WorkerControlAdmitted,
                    payload.to_string(),
                )
                .unwrap();
            assert!(replay_control_audit(store.events()).is_err());
        }
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(replay_control_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn control_replay_does_not_fabricate_worker_lifecycle_state() {
        let mut store = MemoryEventStore::default();
        let lifecycle = working();
        assert_eq!(lifecycle.phase(), WorkerPhase::Working);

        let stop = WorkerControl::stop(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &stop).unwrap();

        let records = replay_control_audit(store.events()).unwrap();
        assert_eq!(records[0].kind, WorkerControlKind::Stop);

        // There are no WorkerGoalAssigned/WorkerLifecycleTransitionRecorded events,
        // so a separate worker replay sees no worker state at all.
        let workers = crate::worker_audit::replay_worker_audit(store.events()).unwrap();
        assert!(workers.is_empty());
    }

    #[test]
    fn generic_sqlite_projection_carries_control_event_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        let command = controls().remove(0);
        record_worker_control_admitted(&mut store, &command).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        let projected = projection
            .events_of_kind(EventKind::WorkerControlAdmitted)
            .unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0], store.events()[0]);

        drop(projection);
        let _ = fs::remove_file(path);
    }
}
