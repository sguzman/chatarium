//! Typed durable audit records for worker goal lifecycles.
//!
//! Worker state remains a core-domain concern. This module records typed lifecycle
//! facts in the authoritative append-only journal and reconstructs them by calling
//! the existing core WorkerLifecycle transition API.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::orchestration::{
    TransitionOutcome, WorkerAction, WorkerGoalId, WorkerId, WorkerLifecycle, WorkerPhase,
    WorkerTransitionError,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const WORKER_AUDIT_SCHEMA: &str = "chatarium-worker-audit";
const WORKER_AUDIT_VERSION: u64 = 1;

/// Restart-replayable audit state for one worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerAuditRecord {
    /// Local worker identity.
    pub worker_id: WorkerId,
    /// Replayed lifecycle state for the current goal.
    pub lifecycle: WorkerLifecycle,
    /// Durable sequence where the current goal was assigned.
    pub assigned_sequence: u64,
    /// Last worker lifecycle event applied.
    pub last_sequence: u64,
}

/// Append a typed worker-goal assignment.
pub fn record_worker_goal_assigned(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        worker_id,
        EventKind::WorkerGoalAssigned,
        json!({
            "schema": WORKER_AUDIT_SCHEMA,
            "version": WORKER_AUDIT_VERSION,
            "record": "goal_assigned",
            "worker_id": worker_id.get(),
            "goal_id": goal_id.get(),
        }),
    )
}

/// Append one typed worker lifecycle transition.
///
/// Callers should append this only after the corresponding core WorkerLifecycle
/// transition succeeded. Replay validates the history again and fails closed if
/// the durable sequence is inconsistent.
pub fn record_worker_transition(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    action: WorkerAction,
) -> std::io::Result<u64> {
    if action == WorkerAction::AssignGoal {
        return Err(invalid_data(
            "AssignGoal must use record_worker_goal_assigned rather than a transition record",
        ));
    }

    append_typed(
        store,
        worker_id,
        EventKind::WorkerLifecycleTransitionRecorded,
        json!({
            "schema": WORKER_AUDIT_SCHEMA,
            "version": WORKER_AUDIT_VERSION,
            "record": "transition",
            "worker_id": worker_id.get(),
            "goal_id": goal_id.get(),
            "action": worker_action_name(action),
        }),
    )
}

/// Reconstruct every typed worker lifecycle from authoritative journal events.
///
/// Unrelated events are ignored. Worker histories fail closed on malformed payloads,
/// stale goals, illegal transitions, or cross-worker scope mismatches.
pub fn replay_worker_audit(events: &[EventEnvelope]) -> Result<Vec<WorkerAuditRecord>, String> {
    let mut workers = BTreeMap::<WorkerId, ReplayWorker>::new();

    for event in events {
        match event.kind {
            EventKind::WorkerGoalAssigned => replay_assignment(&mut workers, event)?,
            EventKind::WorkerLifecycleTransitionRecorded => replay_transition(&mut workers, event)?,
            _ => {}
        }
    }

    let mut records = workers
        .into_iter()
        .map(|(worker_id, state)| WorkerAuditRecord {
            worker_id,
            lifecycle: state.lifecycle,
            assigned_sequence: state.assigned_sequence,
            last_sequence: state.last_sequence,
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.worker_id);
    Ok(records)
}

struct ReplayWorker {
    lifecycle: WorkerLifecycle,
    assigned_sequence: u64,
    last_sequence: u64,
}

fn replay_assignment(
    workers: &mut BTreeMap<WorkerId, ReplayWorker>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "goal_assigned")?;
    let worker_id = worker_id(&payload)?;
    let goal_id = goal_id(&payload)?;
    validate_scope(event, worker_id)?;

    let state = workers.entry(worker_id).or_insert(ReplayWorker {
        lifecycle: WorkerLifecycle::default(),
        assigned_sequence: event.sequence,
        last_sequence: event.sequence,
    });

    state
        .lifecycle
        .assign_goal(goal_id)
        .map_err(|error| worker_error(worker_id, goal_id, event.sequence, error))?;
    state.assigned_sequence = event.sequence;
    state.last_sequence = event.sequence;
    Ok(())
}

fn replay_transition(
    workers: &mut BTreeMap<WorkerId, ReplayWorker>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "transition")?;
    let worker_id = worker_id(&payload)?;
    let goal_id = goal_id(&payload)?;
    validate_scope(event, worker_id)?;
    let action = parse_worker_action(required_string(&payload, "action")?)?;

    let state = workers.get_mut(&worker_id).ok_or_else(|| {
        format!(
            "worker transition for worker {} goal {} at sequence {} appeared before assignment",
            worker_id.get(),
            goal_id.get(),
            event.sequence
        )
    })?;

    match action {
        WorkerAction::AssignGoal => {
            return Err(format!(
                "worker transition for worker {} at sequence {} cannot encode AssignGoal",
                worker_id.get(),
                event.sequence
            ));
        }
        WorkerAction::StartOrResume => state
            .lifecycle
            .start_or_resume(goal_id)
            .map_err(|error| worker_error(worker_id, goal_id, event.sequence, error))?,
        WorkerAction::ReportProgress => state
            .lifecycle
            .report_progress(goal_id)
            .map_err(|error| worker_error(worker_id, goal_id, event.sequence, error))?,
        WorkerAction::RequestInput => state
            .lifecycle
            .request_input(goal_id)
            .map_err(|error| worker_error(worker_id, goal_id, event.sequence, error))?,
        WorkerAction::MarkBlocked => state
            .lifecycle
            .mark_blocked(goal_id)
            .map_err(|error| worker_error(worker_id, goal_id, event.sequence, error))?,
        WorkerAction::Complete => {
            apply_terminal(
                worker_id,
                goal_id,
                event.sequence,
                state.lifecycle.complete(goal_id),
            )?;
        }
        WorkerAction::Fail => {
            apply_terminal(
                worker_id,
                goal_id,
                event.sequence,
                state.lifecycle.fail(goal_id),
            )?;
        }
        WorkerAction::Stop => {
            apply_terminal(
                worker_id,
                goal_id,
                event.sequence,
                state.lifecycle.stop(goal_id),
            )?;
        }
    }

    state.last_sequence = event.sequence;
    Ok(())
}

fn apply_terminal(
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    sequence: u64,
    result: Result<TransitionOutcome, WorkerTransitionError>,
) -> Result<(), String> {
    result
        .map(|_| ())
        .map_err(|error| worker_error(worker_id, goal_id, sequence, error))
}

fn append_typed(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(worker_scope(worker_id)), kind, encoded)
}

/// Stable durable scope for one local worker.
#[must_use]
pub fn worker_scope(worker_id: WorkerId) -> String {
    format!("worker:{}", worker_id.get())
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed worker payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(WORKER_AUDIT_SCHEMA) {
        return Err(format!(
            "worker event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "worker event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != WORKER_AUDIT_VERSION {
        return Err(format!(
            "unsupported worker audit payload version {version} at sequence {}",
            event.sequence
        ));
    }

    let record = required_string(&value, "record")?;
    if record != expected_record {
        return Err(format!(
            "worker event at sequence {} has record '{record}', expected '{expected_record}'",
            event.sequence
        ));
    }

    Ok(value)
}

fn worker_id(value: &Value) -> Result<WorkerId, String> {
    Ok(WorkerId::new(required_u64(value, "worker_id")?))
}

fn goal_id(value: &Value) -> Result<WorkerGoalId, String> {
    Ok(WorkerGoalId::new(required_u64(value, "goal_id")?))
}

fn validate_scope(event: &EventEnvelope, worker_id: WorkerId) -> Result<(), String> {
    let expected = worker_scope(worker_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed worker payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed worker payload is missing string field '{field}'"))
}

const fn worker_action_name(action: WorkerAction) -> &'static str {
    match action {
        WorkerAction::AssignGoal => "assign_goal",
        WorkerAction::StartOrResume => "start_or_resume",
        WorkerAction::ReportProgress => "report_progress",
        WorkerAction::RequestInput => "request_input",
        WorkerAction::MarkBlocked => "mark_blocked",
        WorkerAction::Complete => "complete",
        WorkerAction::Fail => "fail",
        WorkerAction::Stop => "stop",
    }
}

fn parse_worker_action(value: &str) -> Result<WorkerAction, String> {
    match value {
        "assign_goal" => Ok(WorkerAction::AssignGoal),
        "start_or_resume" => Ok(WorkerAction::StartOrResume),
        "report_progress" => Ok(WorkerAction::ReportProgress),
        "request_input" => Ok(WorkerAction::RequestInput),
        "mark_blocked" => Ok(WorkerAction::MarkBlocked),
        "complete" => Ok(WorkerAction::Complete),
        "fail" => Ok(WorkerAction::Fail),
        "stop" => Ok(WorkerAction::Stop),
        other => Err(format!("unknown worker lifecycle action '{other}'")),
    }
}

fn worker_error(
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    sequence: u64,
    error: WorkerTransitionError,
) -> String {
    format!(
        "invalid worker history for worker {} goal {} at sequence {}: {error}",
        worker_id.get(),
        goal_id.get(),
        sequence
    )
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JsonlEventStore, MemoryEventStore, projection::SqliteProjection};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const W2: WorkerId = WorkerId::new(20);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const G2: WorkerGoalId = WorkerGoalId::new(200);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-worker-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn reopen_records(path: &PathBuf) -> Vec<WorkerAuditRecord> {
        let reopened = JsonlEventStore::open(path).expect("reopen");
        replay_worker_audit(reopened.events()).expect("replay")
    }

    fn append_raw_transition(
        store: &mut impl EventStore,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        action: &str,
    ) {
        store
            .append_scoped(
                Some(worker_scope(worker_id)),
                EventKind::WorkerLifecycleTransitionRecorded,
                json!({
                    "schema": WORKER_AUDIT_SCHEMA,
                    "version": WORKER_AUDIT_VERSION,
                    "record": "transition",
                    "worker_id": worker_id.get(),
                    "goal_id": goal_id.get(),
                    "action": action,
                })
                .to_string(),
            )
            .unwrap();
    }

    fn record_working(store: &mut impl EventStore, worker_id: WorkerId, goal_id: WorkerGoalId) {
        record_worker_goal_assigned(store, worker_id, goal_id).unwrap();
        record_worker_transition(store, worker_id, goal_id, WorkerAction::StartOrResume).unwrap();
    }

    #[test]
    fn ready_assignment_survives_reopen() {
        let path = temp_path("ready", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.worker_id, W1);
        assert_eq!(record.lifecycle.goal_id(), Some(G1));
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Ready);
        assert_eq!(record.assigned_sequence, 1);
        assert_eq!(record.last_sequence, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn working_survives_reopen() {
        let path = temp_path("working", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, W1, G1);
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Working);
        assert_eq!(record.last_sequence, 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn attention_states_survive_reopen() {
        for (label, action, expected) in [
            (
                "needs-input",
                WorkerAction::RequestInput,
                WorkerPhase::NeedsInput,
            ),
            ("blocked", WorkerAction::MarkBlocked, WorkerPhase::Blocked),
        ] {
            let path = temp_path(label, "jsonl");
            {
                let mut store = JsonlEventStore::open(&path).unwrap();
                record_working(&mut store, W1, G1);
                record_worker_transition(&mut store, W1, G1, action).unwrap();
            }
            let record = reopen_records(&path).remove(0);
            assert_eq!(record.lifecycle.phase(), expected);
            assert!(record.lifecycle.phase().requires_attention());
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn terminal_states_survive_reopen_and_remain_terminal() {
        for (label, action, expected) in [
            ("completed", WorkerAction::Complete, WorkerPhase::Completed),
            ("failed", WorkerAction::Fail, WorkerPhase::Failed),
            ("stopped", WorkerAction::Stop, WorkerPhase::Stopped),
        ] {
            let path = temp_path(label, "jsonl");
            {
                let mut store = JsonlEventStore::open(&path).unwrap();
                record_working(&mut store, W1, G1);
                record_worker_transition(&mut store, W1, G1, action).unwrap();
            }

            let record = reopen_records(&path).remove(0);
            assert_eq!(record.lifecycle.phase(), expected);
            assert!(record.lifecycle.phase().is_terminal());
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn replacement_goal_after_terminal_survives_reopen() {
        let path = temp_path("replacement", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, W1, G1);
            record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
            record_worker_goal_assigned(&mut store, W1, G2).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.lifecycle.goal_id(), Some(G2));
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Ready);
        assert_eq!(record.assigned_sequence, 4);
        assert_eq!(record.last_sequence, 4);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_old_goal_transition_after_replacement_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        record_worker_goal_assigned(&mut store, W1, G2).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::StartOrResume).unwrap();

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("stale goal control"));
    }

    #[test]
    fn transition_before_assignment_is_rejected() {
        let mut store = MemoryEventStore::default();
        append_raw_transition(&mut store, W1, G1, "start_or_resume");

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("before assignment"));
    }

    #[test]
    fn invalid_transition_is_rejected_by_core_replay() {
        let mut store = MemoryEventStore::default();
        record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("cannot apply Complete"));
    }

    #[test]
    fn active_goal_replacement_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_goal_assigned(&mut store, W1, G2).unwrap();

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("cannot replace active goal"));
    }

    #[test]
    fn two_workers_replay_independently_with_interleaved_events() {
        let mut store = MemoryEventStore::default();
        record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        record_worker_goal_assigned(&mut store, W2, G2).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::StartOrResume).unwrap();
        record_worker_transition(&mut store, W2, G2, WorkerAction::StartOrResume).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::RequestInput).unwrap();
        record_worker_transition(&mut store, W2, G2, WorkerAction::Complete).unwrap();

        let records = replay_worker_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].worker_id, W1);
        assert_eq!(records[0].lifecycle.phase(), WorkerPhase::NeedsInput);
        assert_eq!(records[1].worker_id, W2);
        assert_eq!(records[1].lifecycle.phase(), WorkerPhase::Completed);
    }

    #[test]
    fn torn_tail_cannot_fabricate_worker_transition() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"worker_lifecycle_transition_recorded""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Ready);
        assert_eq!(record.last_sequence, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_worker_payload_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(worker_scope(W1)),
                EventKind::WorkerGoalAssigned,
                json!({
                    "schema": WORKER_AUDIT_SCHEMA,
                    "version": WORKER_AUDIT_VERSION,
                    "record": "goal_assigned",
                    "worker_id": W1.get(),
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_worker_audit(store.events()).is_err());
    }

    #[test]
    fn worker_scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(worker_scope(W2)),
                EventKind::WorkerGoalAssigned,
                json!({
                    "schema": WORKER_AUDIT_SCHEMA,
                    "version": WORKER_AUDIT_VERSION,
                    "record": "goal_assigned",
                    "worker_id": W1.get(),
                    "goal_id": G1.get(),
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn duplicate_matching_terminal_observation_replays_idempotently() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();

        let record = replay_worker_audit(store.events()).unwrap().remove(0);
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Completed);
        assert_eq!(record.last_sequence, 4);
    }

    #[test]
    fn contradictory_terminal_rewrite_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::Fail).unwrap();

        let error = replay_worker_audit(store.events()).unwrap_err();
        assert!(error.contains("cannot apply Fail"));
    }

    #[test]
    fn progress_requires_working_phase_and_does_not_change_phase() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::ReportProgress).unwrap();

        let record = replay_worker_audit(store.events()).unwrap().remove(0);
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Working);
        assert_eq!(record.last_sequence, 3);
    }

    #[test]
    fn continuation_authority_is_not_recreated_by_worker_replay() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);

        let record = replay_worker_audit(store.events()).unwrap().remove(0);
        assert_eq!(record.lifecycle.phase(), WorkerPhase::Working);

        // Replay exposes lifecycle only. A caller must make a fresh explicit
        // decision to construct any ContinuationLease after restart.
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();

        assert!(replay_worker_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn generic_sqlite_projection_carries_worker_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::StartOrResume).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        let assignments = projection
            .events_of_kind(EventKind::WorkerGoalAssigned)
            .unwrap();
        let transitions = projection
            .events_of_kind(EventKind::WorkerLifecycleTransitionRecorded)
            .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(transitions.len(), 1);

        drop(projection);
        let _ = fs::remove_file(path);
    }
}
