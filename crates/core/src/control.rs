//! Typed orchestration-control admission.
//!
//! Controls are semantic domain objects, not wire messages. This module does not
//! define XML, networking, session binding, persistence, or routing. Admission
//! proves only that a proposed control is valid against an observed worker
//! lifecycle snapshot.

use crate::orchestration::{
    ContinuationPermit, WorkerGoalId, WorkerId, WorkerLifecycle, WorkerPhase,
};
use std::fmt;

/// Opaque local identity for one orchestration control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControlId(u64);

impl ControlId {
    /// Construct a control identity from a caller-owned local value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ControlId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Admitted orchestration-control kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerControlKind {
    /// Start a Ready goal or explicitly resume after attention/blocker resolution.
    StartOrResume,
    /// Continue active work using one consumed bounded continuation permit.
    Continue {
        /// One-based permit ordinal from the consumed continuation lease.
        permit_ordinal: u32,
    },
    /// Ask an active/non-terminal worker to stop/cancel its current goal.
    Stop,
    /// Request status for the current goal without changing lifecycle state.
    StatusRequest,
}

/// One semantically admitted worker control command.
///
/// Construction validates an immutable WorkerLifecycle snapshot. It does not mutate
/// worker state; later observed lifecycle transitions remain separate evidence.
#[derive(Debug, PartialEq, Eq)]
pub struct WorkerControl {
    id: ControlId,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    kind: WorkerControlKind,
}

impl WorkerControl {
    /// Admit a start/resume command for Ready, NeedsInput, or Blocked.
    pub fn start_or_resume(
        id: ControlId,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        lifecycle: &WorkerLifecycle,
    ) -> Result<Self, ControlAdmissionError> {
        let kind = WorkerControlKind::StartOrResume;
        validate_control_admission(goal_id, kind, lifecycle)?;

        Ok(Self {
            id,
            worker_id,
            goal_id,
            kind,
        })
    }

    /// Admit a continue command by consuming one bounded continuation permit.
    ///
    /// The permit is move-only and cannot be reused by callers after successful or
    /// failed admission.
    pub fn continue_work(
        id: ControlId,
        worker_id: WorkerId,
        lifecycle: &WorkerLifecycle,
        permit: ContinuationPermit,
    ) -> Result<Self, ControlAdmissionError> {
        let goal_id = permit.goal_id();
        let kind = WorkerControlKind::Continue {
            permit_ordinal: permit.ordinal(),
        };
        validate_control_admission(goal_id, kind, lifecycle)?;

        Ok(Self {
            id,
            worker_id,
            goal_id,
            kind,
        })
    }

    /// Admit a stop command for an assigned non-terminal goal.
    pub fn stop(
        id: ControlId,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        lifecycle: &WorkerLifecycle,
    ) -> Result<Self, ControlAdmissionError> {
        let kind = WorkerControlKind::Stop;
        validate_control_admission(goal_id, kind, lifecycle)?;

        Ok(Self {
            id,
            worker_id,
            goal_id,
            kind,
        })
    }

    /// Admit a goal-correlated status request in any assigned phase, including terminal.
    pub fn status_request(
        id: ControlId,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        lifecycle: &WorkerLifecycle,
    ) -> Result<Self, ControlAdmissionError> {
        let kind = WorkerControlKind::StatusRequest;
        validate_control_admission(goal_id, kind, lifecycle)?;
        Ok(Self {
            id,
            worker_id,
            goal_id,
            kind,
        })
    }

    /// Control identity.
    #[must_use]
    pub const fn id(&self) -> ControlId {
        self.id
    }

    /// Target worker identity.
    #[must_use]
    pub const fn worker_id(&self) -> WorkerId {
        self.worker_id
    }

    /// Target goal identity.
    #[must_use]
    pub const fn goal_id(&self) -> WorkerGoalId {
        self.goal_id
    }

    /// Admitted semantic command kind.
    #[must_use]
    pub const fn kind(&self) -> WorkerControlKind {
        self.kind
    }
}

/// High-level control type used in typed admission failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlType {
    /// Start/resume.
    StartOrResume,
    /// Continue.
    Continue,
    /// Stop.
    Stop,
    /// Status request.
    StatusRequest,
}

/// Why a proposed orchestration control is not admissible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAdmissionError {
    /// The worker snapshot has no current goal.
    NoGoal,
    /// Proposed control is correlated to a stale/different goal.
    GoalMismatch {
        /// Current worker goal.
        expected: WorkerGoalId,
        /// Goal referenced by the proposed control/permit.
        received: WorkerGoalId,
    },
    /// A replayed/supplied continuation kind carried an impossible zero ordinal.
    InvalidContinuationPermitOrdinal,
    /// The control is incompatible with the current worker phase.
    InvalidPhase {
        /// Proposed control class.
        control: ControlType,
        /// Current worker phase.
        phase: WorkerPhase,
    },
}

impl fmt::Display for ControlAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGoal => write!(formatter, "worker has no assigned goal"),
            Self::GoalMismatch { expected, received } => write!(
                formatter,
                "stale worker control: current goal is {expected}, received {received}"
            ),
            Self::InvalidContinuationPermitOrdinal => {
                write!(formatter, "continue control permit ordinal must be greater than zero")
            }
            Self::InvalidPhase { control, phase } => {
                write!(
                    formatter,
                    "{control:?} is not admissible while worker is {phase:?}"
                )
            }
        }
    }
}

impl std::error::Error for ControlAdmissionError {}

/// Validate a control kind against one immutable worker lifecycle snapshot.
///
/// This is shared by live control construction and durable replay so admission
/// semantics cannot silently diverge. For Continue, this validates the persisted
/// permit ordinal and worker phase, but it does not prove durable lease issuance.
pub fn validate_control_admission(
    goal_id: WorkerGoalId,
    kind: WorkerControlKind,
    lifecycle: &WorkerLifecycle,
) -> Result<WorkerPhase, ControlAdmissionError> {
    let phase = matching_phase(lifecycle, goal_id)?;
    let control = match kind {
        WorkerControlKind::StartOrResume => {
            if matches!(
                phase,
                WorkerPhase::Ready | WorkerPhase::NeedsInput | WorkerPhase::Blocked
            ) {
                return Ok(phase);
            }
            ControlType::StartOrResume
        }
        WorkerControlKind::Continue { permit_ordinal } => {
            if permit_ordinal == 0 {
                return Err(ControlAdmissionError::InvalidContinuationPermitOrdinal);
            }
            if phase == WorkerPhase::Working {
                return Ok(phase);
            }
            ControlType::Continue
        }
        WorkerControlKind::Stop => {
            if !phase.is_terminal() {
                return Ok(phase);
            }
            ControlType::Stop
        }
        WorkerControlKind::StatusRequest => return Ok(phase),
    };

    Err(ControlAdmissionError::InvalidPhase { control, phase })
}

fn matching_phase(
    lifecycle: &WorkerLifecycle,
    received: WorkerGoalId,
) -> Result<WorkerPhase, ControlAdmissionError> {
    let Some(expected) = lifecycle.goal_id() else {
        return Err(ControlAdmissionError::NoGoal);
    };
    if expected != received {
        return Err(ControlAdmissionError::GoalMismatch { expected, received });
    }
    Ok(lifecycle.phase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{ContinuationLease, TransitionOutcome};

    const W1: WorkerId = WorkerId::new(10);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const G2: WorkerGoalId = WorkerGoalId::new(200);

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

    fn needs_input() -> WorkerLifecycle {
        let mut lifecycle = working();
        lifecycle.request_input(G1).unwrap();
        lifecycle
    }

    fn blocked() -> WorkerLifecycle {
        let mut lifecycle = working();
        lifecycle.mark_blocked(G1).unwrap();
        lifecycle
    }

    fn terminal(phase: WorkerPhase) -> WorkerLifecycle {
        let mut lifecycle = working();
        match phase {
            WorkerPhase::Completed => {
                assert_eq!(lifecycle.complete(G1), Ok(TransitionOutcome::Changed));
            }
            WorkerPhase::Failed => {
                assert_eq!(lifecycle.fail(G1), Ok(TransitionOutcome::Changed));
            }
            WorkerPhase::Stopped => {
                assert_eq!(lifecycle.stop(G1), Ok(TransitionOutcome::Changed));
            }
            _ => panic!("test requires terminal phase"),
        }
        lifecycle
    }

    #[test]
    fn start_resume_is_admitted_from_ready_attention_and_blocked() {
        for lifecycle in [ready(), needs_input(), blocked()] {
            let before = lifecycle;
            let command =
                WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle).unwrap();
            assert_eq!(command.kind(), WorkerControlKind::StartOrResume);
            assert_eq!(command.worker_id(), W1);
            assert_eq!(command.goal_id(), G1);
            assert_eq!(lifecycle, before);
        }
    }

    #[test]
    fn start_resume_is_rejected_while_working() {
        let lifecycle = working();
        assert_eq!(
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle),
            Err(ControlAdmissionError::InvalidPhase {
                control: ControlType::StartOrResume,
                phase: WorkerPhase::Working,
            })
        );
    }

    #[test]
    fn start_resume_is_rejected_from_every_terminal_phase() {
        for phase in [
            WorkerPhase::Completed,
            WorkerPhase::Failed,
            WorkerPhase::Stopped,
        ] {
            let lifecycle = terminal(phase);
            assert_eq!(
                WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle),
                Err(ControlAdmissionError::InvalidPhase {
                    control: ControlType::StartOrResume,
                    phase,
                })
            );
        }
    }

    #[test]
    fn continue_is_admitted_while_working_with_matching_move_only_permit() {
        let lifecycle = working();
        let mut lease = ContinuationLease::new(G1, 2);
        let permit = lease.authorize(&lifecycle).unwrap();

        let command =
            WorkerControl::continue_work(ControlId::new(7), W1, &lifecycle, permit).unwrap();

        assert_eq!(command.id(), ControlId::new(7));
        assert_eq!(command.worker_id(), W1);
        assert_eq!(command.goal_id(), G1);
        assert_eq!(
            command.kind(),
            WorkerControlKind::Continue { permit_ordinal: 1 }
        );
        assert_eq!(lease.remaining(), 1);
    }

    #[test]
    fn stale_continue_permit_is_rejected() {
        let old_lifecycle = working();
        let mut lease = ContinuationLease::new(G1, 1);
        let permit = lease.authorize(&old_lifecycle).unwrap();

        let mut current = old_lifecycle;
        current.complete(G1).unwrap();
        current.assign_goal(G2).unwrap();
        current.start_or_resume(G2).unwrap();

        assert_eq!(
            WorkerControl::continue_work(ControlId::new(1), W1, &current, permit),
            Err(ControlAdmissionError::GoalMismatch {
                expected: G2,
                received: G1,
            })
        );
    }

    #[test]
    fn continue_is_rejected_from_non_working_phases() {
        let cases = [
            ready(),
            needs_input(),
            blocked(),
            terminal(WorkerPhase::Completed),
            terminal(WorkerPhase::Failed),
            terminal(WorkerPhase::Stopped),
        ];

        for lifecycle in cases {
            let mut working_snapshot = working();
            let mut lease = ContinuationLease::new(G1, 1);
            let permit = lease.authorize(&working_snapshot).unwrap();
            let expected_phase = lifecycle.phase();

            assert_eq!(
                WorkerControl::continue_work(ControlId::new(1), W1, &lifecycle, permit),
                Err(ControlAdmissionError::InvalidPhase {
                    control: ControlType::Continue,
                    phase: expected_phase,
                })
            );

            // Keep the helper snapshot obviously separate from the observed state
            // against which admission was checked.
            working_snapshot.report_progress(G1).unwrap();
        }
    }

    #[test]
    fn stop_is_admitted_for_every_assigned_non_terminal_phase() {
        for lifecycle in [ready(), working(), needs_input(), blocked()] {
            let before = lifecycle;
            let command = WorkerControl::stop(ControlId::new(2), W1, G1, &lifecycle).unwrap();
            assert_eq!(command.kind(), WorkerControlKind::Stop);
            assert_eq!(lifecycle, before);
        }
    }

    #[test]
    fn stop_is_rejected_for_terminal_phases() {
        for phase in [
            WorkerPhase::Completed,
            WorkerPhase::Failed,
            WorkerPhase::Stopped,
        ] {
            let lifecycle = terminal(phase);
            assert_eq!(
                WorkerControl::stop(ControlId::new(2), W1, G1, &lifecycle),
                Err(ControlAdmissionError::InvalidPhase {
                    control: ControlType::Stop,
                    phase,
                })
            );
        }
    }

    #[test]
    fn status_request_is_admitted_for_any_assigned_phase() {
        let cases = [
            ready(),
            working(),
            needs_input(),
            blocked(),
            terminal(WorkerPhase::Completed),
            terminal(WorkerPhase::Failed),
            terminal(WorkerPhase::Stopped),
        ];

        for lifecycle in cases {
            let before = lifecycle;
            let command =
                WorkerControl::status_request(ControlId::new(3), W1, G1, &lifecycle).unwrap();
            assert_eq!(command.kind(), WorkerControlKind::StatusRequest);
            assert_eq!(lifecycle, before);
        }
    }

    #[test]
    fn stale_goal_is_rejected_for_goal_correlated_controls() {
        let lifecycle = working();

        assert_eq!(
            WorkerControl::start_or_resume(ControlId::new(1), W1, G2, &lifecycle),
            Err(ControlAdmissionError::GoalMismatch {
                expected: G1,
                received: G2,
            })
        );
        assert_eq!(
            WorkerControl::stop(ControlId::new(2), W1, G2, &lifecycle),
            Err(ControlAdmissionError::GoalMismatch {
                expected: G1,
                received: G2,
            })
        );
        assert_eq!(
            WorkerControl::status_request(ControlId::new(3), W1, G2, &lifecycle),
            Err(ControlAdmissionError::GoalMismatch {
                expected: G1,
                received: G2,
            })
        );
    }

    #[test]
    fn unassigned_worker_rejects_goal_correlated_controls() {
        let lifecycle = WorkerLifecycle::default();
        assert_eq!(
            WorkerControl::start_or_resume(ControlId::new(1), W1, G1, &lifecycle),
            Err(ControlAdmissionError::NoGoal)
        );
        assert_eq!(
            WorkerControl::stop(ControlId::new(2), W1, G1, &lifecycle),
            Err(ControlAdmissionError::NoGoal)
        );
        assert_eq!(
            WorkerControl::status_request(ControlId::new(3), W1, G1, &lifecycle),
            Err(ControlAdmissionError::NoGoal)
        );
    }

    #[test]
    fn replay_validation_rejects_zero_continue_ordinal() {
        let lifecycle = working();
        assert_eq!(
            validate_control_admission(
                G1,
                WorkerControlKind::Continue { permit_ordinal: 0 },
                &lifecycle,
            ),
            Err(ControlAdmissionError::InvalidContinuationPermitOrdinal)
        );
    }

    #[test]
    fn control_admission_never_mutates_lifecycle() {
        let lifecycle = working();
        let before = lifecycle;

        let _ = WorkerControl::stop(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        let _ = WorkerControl::status_request(ControlId::new(2), W1, G1, &lifecycle).unwrap();

        let mut lease = ContinuationLease::new(G1, 1);
        let permit = lease.authorize(&lifecycle).unwrap();
        let _ = WorkerControl::continue_work(ControlId::new(3), W1, &lifecycle, permit).unwrap();

        assert_eq!(lifecycle, before);
    }

    #[test]
    fn control_preserves_all_local_correlation_identity() {
        let lifecycle = ready();
        let command =
            WorkerControl::start_or_resume(ControlId::new(55), W1, G1, &lifecycle).unwrap();

        assert_eq!(command.id(), ControlId::new(55));
        assert_eq!(command.worker_id(), W1);
        assert_eq!(command.goal_id(), G1);
        assert_eq!(command.kind(), WorkerControlKind::StartOrResume);
    }
}
