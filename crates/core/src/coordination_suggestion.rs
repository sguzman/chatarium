//! Non-authoritative controller coordination suggestions.
//!
//! Suggestions are typed proposals only. They are deliberately distinct from
//! admitted WorkerControl authority, routing policy, lifecycle state, and
//! continuation permits.

use crate::orchestration::{WorkerGoalId, WorkerId};
use std::fmt;

/// Opaque local identity for one coordination suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CoordinationSuggestionId(u64);

impl CoordinationSuggestionId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CoordinationSuggestionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A proposed next worker action without any control authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinationSuggestionAction {
    StartOrResume,
    Continue,
    Stop,
    StatusRequest,
}

impl CoordinationSuggestionAction {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::StartOrResume => "start_or_resume",
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::StatusRequest => "status_request",
        }
    }
}

/// Typed, powerless proposal produced from controller coordination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoordinationSuggestion {
    id: CoordinationSuggestionId,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    action: CoordinationSuggestionAction,
}

impl CoordinationSuggestion {
    #[must_use]
    pub const fn new(
        id: CoordinationSuggestionId,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        action: CoordinationSuggestionAction,
    ) -> Self {
        Self {
            id,
            worker_id,
            goal_id,
            action,
        }
    }

    #[must_use]
    pub const fn id(self) -> CoordinationSuggestionId {
        self.id
    }

    #[must_use]
    pub const fn worker_id(self) -> WorkerId {
        self.worker_id
    }

    #[must_use]
    pub const fn goal_id(self) -> WorkerGoalId {
        self.goal_id
    }

    #[must_use]
    pub const fn action(self) -> CoordinationSuggestionAction {
        self.action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_is_typed_but_carries_no_control_identity() {
        let suggestion = CoordinationSuggestion::new(
            CoordinationSuggestionId::new(7),
            WorkerId::new(11),
            WorkerGoalId::new(13),
            CoordinationSuggestionAction::StatusRequest,
        );

        assert_eq!(suggestion.id(), CoordinationSuggestionId::new(7));
        assert_eq!(suggestion.worker_id(), WorkerId::new(11));
        assert_eq!(suggestion.goal_id(), WorkerGoalId::new(13));
        assert_eq!(
            suggestion.action(),
            CoordinationSuggestionAction::StatusRequest
        );
    }

    #[test]
    fn action_names_are_stable() {
        assert_eq!(
            CoordinationSuggestionAction::StartOrResume.stable_name(),
            "start_or_resume"
        );
        assert_eq!(
            CoordinationSuggestionAction::Continue.stable_name(),
            "continue"
        );
        assert_eq!(CoordinationSuggestionAction::Stop.stable_name(), "stop");
        assert_eq!(
            CoordinationSuggestionAction::StatusRequest.stable_name(),
            "status_request"
        );
    }
}
