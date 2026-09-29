//! Pure domain state for future Chatarium master/worker orchestration.
//!
//! This module deliberately contains no network, persistence, UI, MCP, XML, or
//! ChatGPT-specific transport logic. It defines the lifecycle and bounded
//! continuation invariants that those later layers must respect.

use std::fmt;

/// Opaque local identity for one orchestration worker.
///
/// This is not a ChatGPT conversation/session identifier and carries no transport semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerId(u64);

impl WorkerId {
    /// Construct a worker identity from a caller-owned local value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value for persistence/diagnostics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Opaque correlation identity for one worker goal lifecycle.
///
/// Callers allocate identities; the core only requires that a replacement goal
/// receive a distinct identity so stale control messages cannot affect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkerGoalId(u64);

impl WorkerGoalId {
    /// Construct an opaque goal identity from a caller-owned local sequence/value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque numeric value for persistence/diagnostic adapters.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WorkerGoalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Machine-readable worker lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPhase {
    /// No goal is currently assigned.
    Unassigned,
    /// A goal exists but work has not started yet.
    Ready,
    /// The worker is actively progressing the current goal.
    Working,
    /// The worker requires external/user input before work can resume.
    NeedsInput,
    /// The worker cannot progress until an external blocker changes.
    Blocked,
    /// The current goal completed successfully.
    Completed,
    /// The current goal terminated in failure.
    Failed,
    /// The current goal was explicitly stopped/cancelled.
    Stopped,
}

impl WorkerPhase {
    /// Whether this phase permanently terminates the current goal lifecycle.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Stopped)
    }

    /// Whether progress requires attention rather than another automatic continue.
    #[must_use]
    pub const fn requires_attention(self) -> bool {
        matches!(self, Self::NeedsInput | Self::Blocked)
    }

    /// Whether an explicit bounded continuation authority may be consumed now.
    #[must_use]
    pub const fn allows_continuation(self) -> bool {
        matches!(self, Self::Working)
    }
}

/// Worker lifecycle transition name used in typed transition errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerAction {
    /// Assign a goal.
    AssignGoal,
    /// Start or explicitly resume work.
    StartOrResume,
    /// Report progress without changing phase.
    ReportProgress,
    /// Pause for external/user input.
    RequestInput,
    /// Pause for an external blocker.
    MarkBlocked,
    /// Complete successfully.
    Complete,
    /// Fail.
    Fail,
    /// Stop/cancel.
    Stop,
}

/// Result of an accepted worker-state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionOutcome {
    /// State changed.
    Changed,
    /// The exact same terminal fact was observed again and was accepted idempotently.
    Unchanged,
}

/// Explicit worker lifecycle transition failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerTransitionError {
    /// The action requires an assigned goal, but none exists.
    NoGoal,
    /// A stale control referenced a different goal than the current lifecycle.
    GoalMismatch {
        /// Current goal.
        expected: WorkerGoalId,
        /// Goal referenced by the stale control.
        received: WorkerGoalId,
    },
    /// A live goal cannot be silently replaced.
    ActiveGoalCannotBeReplaced {
        /// Current goal.
        current: WorkerGoalId,
        /// Current non-terminal phase.
        phase: WorkerPhase,
    },
    /// The action is not valid from the current phase.
    InvalidTransition {
        /// Current phase.
        phase: WorkerPhase,
        /// Attempted action.
        action: WorkerAction,
    },
}

impl fmt::Display for WorkerTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGoal => write!(formatter, "worker has no assigned goal"),
            Self::GoalMismatch { expected, received } => write!(
                formatter,
                "stale goal control: current goal is {expected}, received {received}"
            ),
            Self::ActiveGoalCannotBeReplaced { current, phase } => write!(
                formatter,
                "cannot replace active goal {current} while worker is {phase:?}"
            ),
            Self::InvalidTransition { phase, action } => {
                write!(
                    formatter,
                    "cannot apply {action:?} while worker is {phase:?}"
                )
            }
        }
    }
}

impl std::error::Error for WorkerTransitionError {}

/// Current state for one worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerLifecycle {
    goal_id: Option<WorkerGoalId>,
    phase: WorkerPhase,
}

impl Default for WorkerLifecycle {
    fn default() -> Self {
        Self {
            goal_id: None,
            phase: WorkerPhase::Unassigned,
        }
    }
}

impl WorkerLifecycle {
    /// Current goal identity, if any.
    #[must_use]
    pub const fn goal_id(self) -> Option<WorkerGoalId> {
        self.goal_id
    }

    /// Current machine-readable worker phase.
    #[must_use]
    pub const fn phase(self) -> WorkerPhase {
        self.phase
    }

    /// Assign a new goal.
    ///
    /// A replacement goal is accepted only when there is no current goal or the
    /// previous goal is terminal. It always starts a distinct lifecycle in Ready.
    pub fn assign_goal(&mut self, goal_id: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        if let Some(current) = self.goal_id
            && !self.phase.is_terminal()
        {
            return Err(WorkerTransitionError::ActiveGoalCannotBeReplaced {
                current,
                phase: self.phase,
            });
        }

        self.goal_id = Some(goal_id);
        self.phase = WorkerPhase::Ready;
        Ok(())
    }

    /// Start a ready goal or explicitly resume after input/blocker resolution.
    pub fn start_or_resume(&mut self, goal_id: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        self.require_goal(goal_id)?;
        match self.phase {
            WorkerPhase::Ready | WorkerPhase::NeedsInput | WorkerPhase::Blocked => {
                self.phase = WorkerPhase::Working;
                Ok(())
            }
            phase => Err(WorkerTransitionError::InvalidTransition {
                phase,
                action: WorkerAction::StartOrResume,
            }),
        }
    }

    /// Confirm progress for the active working goal without changing phase.
    pub fn report_progress(&self, goal_id: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        self.require_goal(goal_id)?;
        if self.phase == WorkerPhase::Working {
            Ok(())
        } else {
            Err(WorkerTransitionError::InvalidTransition {
                phase: self.phase,
                action: WorkerAction::ReportProgress,
            })
        }
    }

    /// Mark that the worker requires external/user input.
    pub fn request_input(&mut self, goal_id: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        self.require_goal(goal_id)?;
        self.transition_from_working(WorkerAction::RequestInput, WorkerPhase::NeedsInput)
    }

    /// Mark that an external blocker prevents progress.
    pub fn mark_blocked(&mut self, goal_id: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        self.require_goal(goal_id)?;
        self.transition_from_working(WorkerAction::MarkBlocked, WorkerPhase::Blocked)
    }

    /// Record successful completion.
    ///
    /// Re-observing completion for the same goal is idempotent. Other terminal
    /// outcomes cannot be silently rewritten into completion.
    pub fn complete(
        &mut self,
        goal_id: WorkerGoalId,
    ) -> Result<TransitionOutcome, WorkerTransitionError> {
        self.require_goal(goal_id)?;
        if self.phase == WorkerPhase::Completed {
            return Ok(TransitionOutcome::Unchanged);
        }
        if self.phase != WorkerPhase::Working {
            return Err(WorkerTransitionError::InvalidTransition {
                phase: self.phase,
                action: WorkerAction::Complete,
            });
        }
        self.phase = WorkerPhase::Completed;
        Ok(TransitionOutcome::Changed)
    }

    /// Record failure.
    ///
    /// Failure is allowed from any assigned non-terminal phase because failure may
    /// be discovered before, during, or while waiting on active work.
    pub fn fail(
        &mut self,
        goal_id: WorkerGoalId,
    ) -> Result<TransitionOutcome, WorkerTransitionError> {
        self.require_goal(goal_id)?;
        if self.phase == WorkerPhase::Failed {
            return Ok(TransitionOutcome::Unchanged);
        }
        if self.phase.is_terminal() {
            return Err(WorkerTransitionError::InvalidTransition {
                phase: self.phase,
                action: WorkerAction::Fail,
            });
        }
        self.phase = WorkerPhase::Failed;
        Ok(TransitionOutcome::Changed)
    }

    /// Explicitly stop/cancel an assigned non-terminal goal.
    pub fn stop(
        &mut self,
        goal_id: WorkerGoalId,
    ) -> Result<TransitionOutcome, WorkerTransitionError> {
        self.require_goal(goal_id)?;
        if self.phase == WorkerPhase::Stopped {
            return Ok(TransitionOutcome::Unchanged);
        }
        if self.phase.is_terminal() {
            return Err(WorkerTransitionError::InvalidTransition {
                phase: self.phase,
                action: WorkerAction::Stop,
            });
        }
        self.phase = WorkerPhase::Stopped;
        Ok(TransitionOutcome::Changed)
    }

    fn require_goal(&self, received: WorkerGoalId) -> Result<(), WorkerTransitionError> {
        let Some(expected) = self.goal_id else {
            return Err(WorkerTransitionError::NoGoal);
        };
        if expected != received {
            return Err(WorkerTransitionError::GoalMismatch { expected, received });
        }
        Ok(())
    }

    fn transition_from_working(
        &mut self,
        action: WorkerAction,
        target: WorkerPhase,
    ) -> Result<(), WorkerTransitionError> {
        if self.phase != WorkerPhase::Working {
            return Err(WorkerTransitionError::InvalidTransition {
                phase: self.phase,
                action,
            });
        }
        self.phase = target;
        Ok(())
    }
}

/// Bounded explicit authority for a controller to issue continuation prompts.
///
/// A lease is correlated to exactly one goal. Each successful authorization burns
/// one unit. Creating another lease is therefore an explicit new controller/user
/// decision rather than an implicit self-sustaining loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationLease {
    goal_id: WorkerGoalId,
    remaining: u32,
    issued: u32,
}

impl ContinuationLease {
    /// Create explicit bounded continuation authority for one goal.
    #[must_use]
    pub const fn new(goal_id: WorkerGoalId, allowance: u32) -> Self {
        Self {
            goal_id,
            remaining: allowance,
            issued: 0,
        }
    }

    /// Goal identity to which this lease is bound.
    #[must_use]
    pub const fn goal_id(self) -> WorkerGoalId {
        self.goal_id
    }

    /// Remaining continuation authorizations.
    #[must_use]
    pub const fn remaining(self) -> u32 {
        self.remaining
    }

    /// Number of permits already issued from this lease.
    #[must_use]
    pub const fn issued(self) -> u32 {
        self.issued
    }

    /// Issue one continuation permit if worker state and bounded authority allow it.
    pub fn authorize(
        &mut self,
        worker: &WorkerLifecycle,
    ) -> Result<ContinuationPermit, ContinuationDenied> {
        let Some(current_goal) = worker.goal_id else {
            return Err(ContinuationDenied::NoGoal);
        };
        if current_goal != self.goal_id {
            return Err(ContinuationDenied::GoalMismatch {
                current: current_goal,
                lease: self.goal_id,
            });
        }

        match worker.phase {
            WorkerPhase::Working => {}
            phase if phase.requires_attention() => {
                return Err(ContinuationDenied::AttentionRequired { phase });
            }
            phase if phase.is_terminal() => {
                return Err(ContinuationDenied::Terminal { phase });
            }
            phase => return Err(ContinuationDenied::NotWorking { phase }),
        }

        if self.remaining == 0 {
            return Err(ContinuationDenied::AllowanceExhausted);
        }

        self.remaining -= 1;
        self.issued += 1;
        Ok(ContinuationPermit {
            goal_id: self.goal_id,
            ordinal: self.issued,
        })
    }
}

/// One explicit continuation authorization correlated to a goal lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationPermit {
    goal_id: WorkerGoalId,
    ordinal: u32,
}

impl ContinuationPermit {
    /// Goal identity authorized by this permit.
    #[must_use]
    pub const fn goal_id(self) -> WorkerGoalId {
        self.goal_id
    }

    /// One-based permit ordinal within its lease.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

/// Why a controller may not issue another automatic continuation now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationDenied {
    /// No goal exists.
    NoGoal,
    /// The lease belongs to an older/different goal.
    GoalMismatch {
        /// Current worker goal.
        current: WorkerGoalId,
        /// Goal bound to the lease.
        lease: WorkerGoalId,
    },
    /// A goal exists but is not actively working.
    NotWorking {
        /// Current phase.
        phase: WorkerPhase,
    },
    /// Human/external attention is required before work resumes.
    AttentionRequired {
        /// Current attention state.
        phase: WorkerPhase,
    },
    /// The goal is terminal.
    Terminal {
        /// Terminal phase.
        phase: WorkerPhase,
    },
    /// Explicit bounded continuation authority has been consumed.
    AllowanceExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    const G1: WorkerGoalId = WorkerGoalId::new(1);
    const G2: WorkerGoalId = WorkerGoalId::new(2);

    fn working(goal: WorkerGoalId) -> WorkerLifecycle {
        let mut worker = WorkerLifecycle::default();
        worker.assign_goal(goal).unwrap();
        worker.start_or_resume(goal).unwrap();
        worker
    }

    #[test]
    fn initial_state_is_unassigned() {
        let worker = WorkerLifecycle::default();
        assert_eq!(worker.goal_id(), None);
        assert_eq!(worker.phase(), WorkerPhase::Unassigned);
        assert!(!worker.phase().is_terminal());
        assert!(!worker.phase().allows_continuation());
    }

    #[test]
    fn assign_then_start_enters_working() {
        let mut worker = WorkerLifecycle::default();
        worker.assign_goal(G1).unwrap();
        assert_eq!(worker.goal_id(), Some(G1));
        assert_eq!(worker.phase(), WorkerPhase::Ready);
        worker.start_or_resume(G1).unwrap();
        assert_eq!(worker.phase(), WorkerPhase::Working);
    }

    #[test]
    fn working_progress_and_explicit_continue_are_allowed() {
        let worker = working(G1);
        worker.report_progress(G1).unwrap();
        let mut lease = ContinuationLease::new(G1, 2);
        let permit = lease.authorize(&worker).unwrap();
        assert_eq!(permit.goal_id(), G1);
        assert_eq!(permit.ordinal(), 1);
        assert_eq!(lease.remaining(), 1);
        assert_eq!(lease.issued(), 1);
    }

    #[test]
    fn needs_input_halts_continuation_without_consuming_allowance() {
        let mut worker = working(G1);
        worker.request_input(G1).unwrap();
        let mut lease = ContinuationLease::new(G1, 2);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::AttentionRequired {
                phase: WorkerPhase::NeedsInput,
            })
        );
        assert_eq!(lease.remaining(), 2);
    }

    #[test]
    fn blocked_halts_continuation_without_consuming_allowance() {
        let mut worker = working(G1);
        worker.mark_blocked(G1).unwrap();
        let mut lease = ContinuationLease::new(G1, 2);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::AttentionRequired {
                phase: WorkerPhase::Blocked,
            })
        );
        assert_eq!(lease.remaining(), 2);
    }

    #[test]
    fn explicit_resume_is_required_after_attention_state() {
        for attention in [WorkerPhase::NeedsInput, WorkerPhase::Blocked] {
            let mut worker = working(G1);
            match attention {
                WorkerPhase::NeedsInput => worker.request_input(G1).unwrap(),
                WorkerPhase::Blocked => worker.mark_blocked(G1).unwrap(),
                _ => unreachable!(),
            }
            assert!(worker.phase().requires_attention());
            worker.start_or_resume(G1).unwrap();
            assert_eq!(worker.phase(), WorkerPhase::Working);
        }
    }

    #[test]
    fn completed_is_terminal_and_cannot_continue() {
        let mut worker = working(G1);
        assert_eq!(worker.complete(G1), Ok(TransitionOutcome::Changed));
        assert!(worker.phase().is_terminal());
        let mut lease = ContinuationLease::new(G1, 3);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::Terminal {
                phase: WorkerPhase::Completed,
            })
        );
        assert_eq!(lease.remaining(), 3);
    }

    #[test]
    fn failed_is_terminal_and_cannot_continue() {
        let mut worker = working(G1);
        assert_eq!(worker.fail(G1), Ok(TransitionOutcome::Changed));
        assert!(worker.phase().is_terminal());
        let mut lease = ContinuationLease::new(G1, 1);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::Terminal {
                phase: WorkerPhase::Failed,
            })
        );
    }

    #[test]
    fn stopped_is_terminal_and_cannot_continue() {
        let mut worker = working(G1);
        assert_eq!(worker.stop(G1), Ok(TransitionOutcome::Changed));
        assert!(worker.phase().is_terminal());
        let mut lease = ContinuationLease::new(G1, 1);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::Terminal {
                phase: WorkerPhase::Stopped,
            })
        );
    }

    #[test]
    fn continue_without_goal_is_rejected() {
        let worker = WorkerLifecycle::default();
        let mut lease = ContinuationLease::new(G1, 1);
        assert_eq!(lease.authorize(&worker), Err(ContinuationDenied::NoGoal));
        assert_eq!(lease.remaining(), 1);
    }

    #[test]
    fn ready_goal_is_not_implicitly_continueable() {
        let mut worker = WorkerLifecycle::default();
        worker.assign_goal(G1).unwrap();
        let mut lease = ContinuationLease::new(G1, 1);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::NotWorking {
                phase: WorkerPhase::Ready,
            })
        );
    }

    #[test]
    fn stale_goal_control_cannot_affect_replacement_goal() {
        let mut worker = working(G1);
        worker.complete(G1).unwrap();
        worker.assign_goal(G2).unwrap();
        worker.start_or_resume(G2).unwrap();

        assert_eq!(
            worker.report_progress(G1),
            Err(WorkerTransitionError::GoalMismatch {
                expected: G2,
                received: G1,
            })
        );

        let mut stale_lease = ContinuationLease::new(G1, 2);
        assert_eq!(
            stale_lease.authorize(&worker),
            Err(ContinuationDenied::GoalMismatch {
                current: G2,
                lease: G1,
            })
        );
        assert_eq!(stale_lease.remaining(), 2);
    }

    #[test]
    fn live_goal_cannot_be_silently_replaced() {
        let mut worker = working(G1);
        assert_eq!(
            worker.assign_goal(G2),
            Err(WorkerTransitionError::ActiveGoalCannotBeReplaced {
                current: G1,
                phase: WorkerPhase::Working,
            })
        );
        assert_eq!(worker.goal_id(), Some(G1));
        assert_eq!(worker.phase(), WorkerPhase::Working);
    }

    #[test]
    fn replacement_goal_after_terminal_state_starts_new_ready_lifecycle() {
        let mut worker = working(G1);
        worker.complete(G1).unwrap();
        worker.assign_goal(G2).unwrap();
        assert_eq!(worker.goal_id(), Some(G2));
        assert_eq!(worker.phase(), WorkerPhase::Ready);
    }

    #[test]
    fn duplicate_matching_terminal_observation_is_idempotent() {
        let mut completed = working(G1);
        completed.complete(G1).unwrap();
        assert_eq!(completed.complete(G1), Ok(TransitionOutcome::Unchanged));

        let mut failed = working(G1);
        failed.fail(G1).unwrap();
        assert_eq!(failed.fail(G1), Ok(TransitionOutcome::Unchanged));

        let mut stopped = working(G1);
        stopped.stop(G1).unwrap();
        assert_eq!(stopped.stop(G1), Ok(TransitionOutcome::Unchanged));
    }

    #[test]
    fn conflicting_terminal_rewrite_is_rejected() {
        let mut worker = working(G1);
        worker.complete(G1).unwrap();
        assert_eq!(
            worker.fail(G1),
            Err(WorkerTransitionError::InvalidTransition {
                phase: WorkerPhase::Completed,
                action: WorkerAction::Fail,
            })
        );
        assert_eq!(worker.phase(), WorkerPhase::Completed);
    }

    #[test]
    fn stop_from_active_attention_states_is_allowed() {
        for phase in [
            WorkerPhase::Ready,
            WorkerPhase::Working,
            WorkerPhase::NeedsInput,
            WorkerPhase::Blocked,
        ] {
            let mut worker = WorkerLifecycle::default();
            worker.assign_goal(G1).unwrap();
            match phase {
                WorkerPhase::Ready => {}
                WorkerPhase::Working => worker.start_or_resume(G1).unwrap(),
                WorkerPhase::NeedsInput => {
                    worker.start_or_resume(G1).unwrap();
                    worker.request_input(G1).unwrap();
                }
                WorkerPhase::Blocked => {
                    worker.start_or_resume(G1).unwrap();
                    worker.mark_blocked(G1).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(worker.phase(), phase);
            assert_eq!(worker.stop(G1), Ok(TransitionOutcome::Changed));
            assert_eq!(worker.phase(), WorkerPhase::Stopped);
        }
    }

    #[test]
    fn completion_before_work_begins_is_rejected() {
        let mut worker = WorkerLifecycle::default();
        worker.assign_goal(G1).unwrap();
        assert_eq!(
            worker.complete(G1),
            Err(WorkerTransitionError::InvalidTransition {
                phase: WorkerPhase::Ready,
                action: WorkerAction::Complete,
            })
        );
    }

    #[test]
    fn bounded_continuation_lease_exhausts_instead_of_looping_forever() {
        let worker = working(G1);
        let mut lease = ContinuationLease::new(G1, 3);
        let permits: Vec<_> = (0..3).map(|_| lease.authorize(&worker).unwrap()).collect();
        assert_eq!(
            permits
                .iter()
                .map(|permit| permit.ordinal())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(lease.remaining(), 0);
        assert_eq!(lease.issued(), 3);
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::AllowanceExhausted)
        );
    }

    #[test]
    fn completion_halts_immediately_even_with_unused_allowance() {
        let mut worker = working(G1);
        let mut lease = ContinuationLease::new(G1, 5);
        lease.authorize(&worker).unwrap();
        assert_eq!(lease.remaining(), 4);
        worker.complete(G1).unwrap();
        assert_eq!(
            lease.authorize(&worker),
            Err(ContinuationDenied::Terminal {
                phase: WorkerPhase::Completed,
            })
        );
        assert_eq!(lease.remaining(), 4);
    }
}
