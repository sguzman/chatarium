//! Read-only classification of durable MCP call-review stages.
//!
//! This is presentation only: execution authority remains with RouteGate,
//! explicit user decisions, immutable call provenance, and one-shot dispatch.

use chatarium_store::routing_audit::RouteUserDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallReviewStage {
    Unbound,
    AwaitingDecision,
    Denied,
    ApprovedForSeparateReview,
    DispatchedAwaitingOutcome,
    OutcomeRecorded,
}

impl CallReviewStage {
    /// A denied, completed, or already-dispatched call cannot be suggested
    /// as a fresh user execution task.
    pub const fn actionable(self) -> bool {
        matches!(
            self,
            Self::AwaitingDecision | Self::ApprovedForSeparateReview
        )
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Unbound => "Route not bound · no action",
            Self::AwaitingDecision => "Awaiting explicit Allow or Deny",
            Self::Denied => "Denied · no execution",
            Self::ApprovedForSeparateReview => "Allowed · review before one-shot Run",
            Self::DispatchedAwaitingOutcome => "Route consumed · awaiting outcome",
            Self::OutcomeRecorded => "Outcome recorded · inspect audit",
        }
    }
}

/// Inputs must come from correlated, replayed route/call/outcome audits.
/// These flags *never* form an execution permit. A completed/consumed route
/// takes precedence over a stale user approval.
pub const fn classify(
    route_bound: bool,
    latest_decision: Option<RouteUserDecision>,
    dispatch_consumed: bool,
    outcome_recorded: bool,
) -> CallReviewStage {
    if outcome_recorded {
        CallReviewStage::OutcomeRecorded
    } else if dispatch_consumed {
        CallReviewStage::DispatchedAwaitingOutcome
    } else if !route_bound {
        CallReviewStage::Unbound
    } else {
        match latest_decision {
            Some(RouteUserDecision::Allow) => CallReviewStage::ApprovedForSeparateReview,
            Some(RouteUserDecision::Deny) => CallReviewStage::Denied,
            None => CallReviewStage::AwaitingDecision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_preserves_separate_user_review() {
        let pending = classify(true, None, false, false);
        assert_eq!(pending, CallReviewStage::AwaitingDecision);
        assert!(pending.actionable());
        let approved = classify(true, Some(RouteUserDecision::Allow), false, false);
        assert_eq!(approved, CallReviewStage::ApprovedForSeparateReview);
        assert!(approved.actionable());
        let denied = classify(true, Some(RouteUserDecision::Deny), false, false);
        assert_eq!(denied, CallReviewStage::Denied);
        assert!(!denied.actionable());
        assert!(!classify(false, None, false, false).actionable());
    }

    #[test]
    fn consumed_or_observed_calls_never_reenter_actionable_queue() {
        for decision in [
            None,
            Some(RouteUserDecision::Allow),
            Some(RouteUserDecision::Deny),
        ] {
            assert_eq!(
                classify(true, decision, true, false),
                CallReviewStage::DispatchedAwaitingOutcome
            );
            assert!(!classify(true, decision, true, false).actionable());
            assert_eq!(
                classify(true, decision, true, true),
                CallReviewStage::OutcomeRecorded
            );
            assert!(!classify(true, decision, true, true).actionable());
        }
    }

    #[test]
    fn presentation_labels_do_not_imply_auto_execution() {
        for stage in [
            CallReviewStage::Unbound,
            CallReviewStage::AwaitingDecision,
            CallReviewStage::Denied,
            CallReviewStage::ApprovedForSeparateReview,
            CallReviewStage::DispatchedAwaitingOutcome,
            CallReviewStage::OutcomeRecorded,
        ] {
            assert!(!stage.label().is_empty());
            assert!(!stage.label().contains("auto"));
        }
    }
}
