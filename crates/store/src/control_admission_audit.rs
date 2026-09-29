//! Historical validation of admitted worker controls.
//!
//! Shape-valid control records are replayed against the durable worker lifecycle
//! that existed immediately before admission. This prevents restart replay from
//! treating a stale or phase-invalid control as semantically admitted.

use crate::control_audit::{ControlAuditRecord, replay_control_audit};
use crate::worker_audit::replay_worker_audit;
use crate::EventEnvelope;
use chatarium_core::control::validate_control_admission;
use chatarium_core::orchestration::{WorkerId, WorkerPhase};

/// One durable control admission validated against historical worker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedControlAdmission {
    /// Shape-valid durable control record.
    pub control: ControlAuditRecord,
    /// Worker phase that justified admission.
    pub admitted_phase: WorkerPhase,
}

/// Reconstruct every admitted control and validate it against worker state at
/// the exact historical admission boundary.
pub fn replay_validated_control_admissions(
    events: &[EventEnvelope],
) -> Result<Vec<ValidatedControlAdmission>, String> {
    replay_worker_audit(events)?;

    replay_control_audit(events)?
        .into_iter()
        .map(|control| {
            let admitted_phase =
                validate_control_freshness_before(events, &control, control.admitted_sequence)?;
            Ok(ValidatedControlAdmission {
                control,
                admitted_phase,
            })
        })
        .collect()
}

/// Validate one control against the target worker lifecycle immediately before
/// the supplied exclusive sequence.
///
/// This is used both for admission-time proof and for later freshness checks.
/// It does not mutate worker state or grant dispatch/continuation authority.
pub fn validate_control_freshness_before(
    events: &[EventEnvelope],
    control: &ControlAuditRecord,
    exclusive_sequence: u64,
) -> Result<WorkerPhase, String> {
    let prefix_end = events
        .iter()
        .position(|event| event.sequence >= exclusive_sequence)
        .unwrap_or(events.len());
    let workers = replay_worker_audit(&events[..prefix_end])?;
    let worker = workers
        .into_iter()
        .find(|record| record.worker_id == control.worker_id)
        .ok_or_else(|| {
            format!(
                "control {} targets worker {} before any durable worker lifecycle exists at sequence {}",
                control.control_id.get(),
                control.worker_id.get(),
                exclusive_sequence
            )
        })?;

    validate_control_admission(control.goal_id, control.kind, &worker.lifecycle).map_err(|error| {
        format!(
            "control {} for worker {} goal {} is not admissible immediately before sequence {}: {error}",
            control.control_id.get(),
            control.worker_id.get(),
            control.goal_id.get(),
            exclusive_sequence
        )
    })
}

/// Resolve one worker's lifecycle phase immediately before an event boundary.
pub fn worker_phase_before(
    events: &[EventEnvelope],
    worker_id: WorkerId,
    exclusive_sequence: u64,
) -> Result<Option<WorkerPhase>, String> {
    let prefix_end = events
        .iter()
        .position(|event| event.sequence >= exclusive_sequence)
        .unwrap_or(events.len());
    let workers = replay_worker_audit(&events[..prefix_end])?;
    Ok(workers
        .into_iter()
        .find(|record| record.worker_id == worker_id)
        .map(|record| record.lifecycle.phase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_audit::record_worker_control_admitted;
    use crate::worker_audit::{record_worker_goal_assigned, record_worker_transition};
    use crate::{EventStore, JsonlEventStore, MemoryEventStore};
    use chatarium_core::control::{ControlId, WorkerControl, WorkerControlKind};
    use chatarium_core::orchestration::{
        ContinuationLease, WorkerAction, WorkerGoalId, WorkerLifecycle,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const G2: WorkerGoalId = WorkerGoalId::new(200);

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-control-admission-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    fn ready(goal_id: WorkerGoalId) -> WorkerLifecycle {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(goal_id).unwrap();
        lifecycle
    }

    fn working(goal_id: WorkerGoalId) -> WorkerLifecycle {
        let mut lifecycle = ready(goal_id);
        lifecycle.start_or_resume(goal_id).unwrap();
        lifecycle
    }

    fn record_ready(store: &mut impl EventStore, goal_id: WorkerGoalId) {
        record_worker_goal_assigned(store, W1, goal_id).unwrap();
    }

    fn record_working(store: &mut impl EventStore, goal_id: WorkerGoalId) {
        record_ready(store, goal_id);
        record_worker_transition(store, W1, goal_id, WorkerAction::StartOrResume).unwrap();
    }

    #[test]
    fn valid_start_resume_admission_replays_against_ready_state() {
        let mut store = MemoryEventStore::default();
        record_ready(&mut store, G1);
        let lifecycle = ready(G1);
        let control =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let record = replay_validated_control_admissions(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.admitted_phase, WorkerPhase::Ready);
        assert_eq!(record.control.kind, WorkerControlKind::StartOrResume);
    }

    #[test]
    fn valid_continue_admission_replays_against_working_state() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let lifecycle = working(G1);
        let mut lease = ContinuationLease::new(G1, 1);
        let permit = lease.authorize(&lifecycle).unwrap();
        let control =
            WorkerControl::continue_work(ControlId::new(1), W1, &lifecycle, permit).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let record = replay_validated_control_admissions(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.admitted_phase, WorkerPhase::Working);
    }

    #[test]
    fn valid_stop_admission_replays_against_non_terminal_state() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let lifecycle = working(G1);
        let control = WorkerControl::stop(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let record = replay_validated_control_admissions(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.admitted_phase, WorkerPhase::Working);
    }

    #[test]
    fn terminal_status_request_remains_valid() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();

        let mut lifecycle = working(G1);
        lifecycle.complete(G1).unwrap();
        let control =
            WorkerControl::status_request(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let record = replay_validated_control_admissions(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.admitted_phase, WorkerPhase::Completed);
    }

    #[test]
    fn admission_before_worker_goal_is_rejected() {
        let mut store = MemoryEventStore::default();
        let lifecycle = ready(G1);
        let control =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let error = replay_validated_control_admissions(store.events()).unwrap_err();
        assert!(error.contains("before any durable worker lifecycle"));
    }

    #[test]
    fn stale_goal_at_admission_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_ready(&mut store, G2);

        let lifecycle = ready(G1);
        let control = WorkerControl::stop(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let error = replay_validated_control_admissions(store.events()).unwrap_err();
        assert!(error.contains("stale worker control"));
    }

    #[test]
    fn invalid_phase_at_admission_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);

        let lifecycle = ready(G1);
        let control =
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let error = replay_validated_control_admissions(store.events()).unwrap_err();
        assert!(error.contains("not admissible"));
        assert!(error.contains("Working"));
    }

    #[test]
    fn validated_admission_survives_real_journal_reopen() {
        let path = temp_path("reopen");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, G1);
            let lifecycle = working(G1);
            let control = WorkerControl::stop(ControlId::new(1), W1, G1, &lifecycle).unwrap();
            record_worker_control_admitted(&mut store, &control).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_validated_control_admissions(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].admitted_phase, WorkerPhase::Working);
        let _ = fs::remove_file(path);
    }
}
