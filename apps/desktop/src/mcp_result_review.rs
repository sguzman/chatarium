//! Read-only presentation helpers for observed local tool results.
//!
//! Neither an observed result nor a visible preview grants inference context.
//! Only the separate durable tool-result-context decision can admit it.

use chatarium_core::LocalConversationId;
use chatarium_core::tool::ToolProviderId;
use chatarium_store::tool_outcome_audit::ToolCallOutcomeRecord;
use chatarium_store::tool_result_context_audit::{
    MAX_CONTEXT_TOOL_RESULT_BYTES, ToolResultContextDecision, ToolResultContextRecord,
};

pub const RESULT_PREVIEW_BYTES: usize = 4 * 1024;
pub const RECENT_RESULT_CANDIDATES: usize = 16;

/// Borrowed historical evidence, never an admission or tool permission.
pub struct FocusedResult<'a> {
    pub outcome: &'a ToolCallOutcomeRecord,
    pub stage: ResultReviewStage,
}

/// Build a bounded, read-only review queue for one provider and conversation.
/// Every candidate's historical ownership and existing decision identity must
/// replay cleanly. An error blocks the entire queue; partial results are never
/// returned as if they were authorized.
pub fn recent_result_queue<'a>(
    outcomes: &'a [ToolCallOutcomeRecord],
    decisions: &[ToolResultContextRecord],
    provider_id: ToolProviderId,
    conversation_id: LocalConversationId,
    mut owning_conversation: impl FnMut(
        &ToolCallOutcomeRecord,
    ) -> Result<Option<LocalConversationId>, String>,
) -> Result<Vec<FocusedResult<'a>>, String> {
    let mut candidate_outcomes = outcomes
        .iter()
        .filter(|outcome| outcome.provider_id == provider_id)
        .collect::<Vec<_>>();
    candidate_outcomes.sort_unstable_by_key(|outcome| std::cmp::Reverse(outcome.observed_sequence));
    let mut queue = Vec::new();
    for outcome in candidate_outcomes
        .into_iter()
        .take(RECENT_RESULT_CANDIDATES)
    {
        if owning_conversation(outcome)? != Some(conversation_id) {
            continue;
        }
        let mut matching_decisions = decisions
            .iter()
            .filter(|record| record.call_id == outcome.call_id);
        let record = matching_decisions.next();
        if matching_decisions.next().is_some() {
            return Err("duplicate context decisions for a tool result".to_owned());
        }
        if let Some(record) = record {
            if record.conversation_id != conversation_id
                || record.outcome_sequence != outcome.observed_sequence
                || record.route_id != outcome.route_id
                || record.provider_id != outcome.provider_id
                || record.source_session_id != outcome.source_session_id
                || record.outcome_kind != outcome.kind
            {
                return Err(
                    "result context does not match the historically owned outcome".to_owned(),
                );
            }
        }
        queue.push(FocusedResult {
            outcome,
            stage: classify(record.map(|record| record.decision), outcome.text.len()),
        });
    }
    // Prioritize undecided results, but preserve newest-first chronology
    // within a stage. Ordering is for presentation, never authorization.
    queue.sort_by(|left, right| {
        right
            .stage
            .focus_priority()
            .cmp(&left.stage.focus_priority())
            .then_with(|| {
                right.outcome
                    .observed_sequence
                    .cmp(&left.outcome.observed_sequence)
            })
            .then_with(|| right.outcome.call_id.get().cmp(&left.outcome.call_id.get()))
    });
    Ok(queue)
}

/// Backward-compatible single-result focus, now derived from the same
/// independently verified multi-result queue.
pub fn focus_recent_result<'a>(
    outcomes: &'a [ToolCallOutcomeRecord],
    decisions: &[ToolResultContextRecord],
    provider_id: ToolProviderId,
    conversation_id: LocalConversationId,
    owning_conversation: impl FnMut(
        &ToolCallOutcomeRecord,
    ) -> Result<Option<LocalConversationId>, String>,
) -> Result<Option<FocusedResult<'a>>, String> {
    Ok(recent_result_queue(
        outcomes,
        decisions,
        provider_id,
        conversation_id,
        owning_conversation,
    )?
    .into_iter()
    .next())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultReviewStage {
    ExcludedByDefault,
    ExplicitlyExcluded,
    Admitted,
    TooLargeToAdmit,
}

impl ResultReviewStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ExcludedByDefault => "EXCLUDED FROM CONTEXT · DEFAULT",
            Self::ExplicitlyExcluded => "EXCLUDED FROM CONTEXT · EXPLICIT",
            Self::Admitted => "ADMITTED TO CONTEXT · EXPLICIT",
            Self::TooLargeToAdmit => "EXCLUDED FROM CONTEXT · OVERSIZED",
        }
    }

    pub const fn may_admit(self) -> bool {
        matches!(self, Self::ExcludedByDefault | Self::ExplicitlyExcluded)
    }

    pub const fn may_exclude(self) -> bool {
        matches!(self, Self::Admitted)
    }

    /// A bounded preview must never be mistaken for full-result review.
    /// Larger admissible results remain available through the exact audit.
    pub const fn may_admit_from_preview(self, truncated: bool) -> bool {
        self.may_admit() && !truncated
    }

    /// Prefer previously undecided outcomes without changing journal state.
    pub const fn focus_priority(self) -> u8 {
        match self {
            Self::ExcludedByDefault => 3,
            Self::Admitted => 2,
            Self::ExplicitlyExcluded => 1,
            Self::TooLargeToAdmit => 0,
        }
    }
}

pub const fn classify(
    decision: Option<ToolResultContextDecision>,
    outcome_bytes: usize,
) -> ResultReviewStage {
    if outcome_bytes > MAX_CONTEXT_TOOL_RESULT_BYTES {
        ResultReviewStage::TooLargeToAdmit
    } else {
        match decision {
            Some(ToolResultContextDecision::Admit) => ResultReviewStage::Admitted,
            Some(ToolResultContextDecision::Exclude) => ResultReviewStage::ExplicitlyExcluded,
            None => ResultReviewStage::ExcludedByDefault,
        }
    }
}

/// Return an exact UTF-8 prefix only; never truncate the durable observation
/// or treat the remaining bytes as having been reviewed.
pub fn preview(text: &str) -> (&str, bool) {
    if text.len() <= RESULT_PREVIEW_BYTES {
        return (text, false);
    }
    let mut end = RESULT_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_exclusion_requires_explicit_admission() {
        let stage = classify(None, 7);
        assert_eq!(stage, ResultReviewStage::ExcludedByDefault);
        assert!(stage.may_admit());
        assert!(!stage.may_exclude());
        assert!(stage.label().contains("EXCLUDED"));
        assert_eq!(
            classify(Some(ToolResultContextDecision::Exclude), 7),
            ResultReviewStage::ExplicitlyExcluded
        );
        assert!(classify(Some(ToolResultContextDecision::Exclude), 7).may_admit());
    }

    #[test]
    fn admitted_outcome_can_only_be_excluded_by_separate_decision() {
        let stage = classify(Some(ToolResultContextDecision::Admit), 7);
        assert_eq!(stage, ResultReviewStage::Admitted);
        assert!(!stage.may_admit());
        assert!(stage.may_exclude());
        assert!(stage.label().contains("EXPLICIT"));
    }

    #[test]
    fn oversized_observation_never_offers_admission_even_if_stale_decision() {
        for decision in [
            None,
            Some(ToolResultContextDecision::Admit),
            Some(ToolResultContextDecision::Exclude),
        ] {
            let stage = classify(decision, MAX_CONTEXT_TOOL_RESULT_BYTES + 1);
            assert_eq!(stage, ResultReviewStage::TooLargeToAdmit);
            assert!(!stage.may_admit());
            assert!(!stage.may_exclude());
        }
        assert!(classify(None, MAX_CONTEXT_TOOL_RESULT_BYTES).may_admit());
    }

    #[test]
    fn truncated_preview_never_offers_full_result_admission() {
        assert!(ResultReviewStage::ExcludedByDefault.may_admit_from_preview(false));
        assert!(!ResultReviewStage::ExcludedByDefault.may_admit_from_preview(true));
        assert!(!ResultReviewStage::Admitted.may_admit_from_preview(false));
        assert!(!ResultReviewStage::TooLargeToAdmit.may_admit_from_preview(false));
        // A previously admitted result can still be excluded without reading it.
        assert!(ResultReviewStage::Admitted.may_exclude());
    }

    #[test]
    fn preview_is_bounded_unicode_safe_and_never_modifies_source() {
        let text = "é".repeat(RESULT_PREVIEW_BYTES);
        let (prefix, truncated) = preview(&text);
        assert!(truncated);
        assert!(prefix.len() <= RESULT_PREVIEW_BYTES);
        assert!(text.starts_with(prefix));
        assert_eq!(text.len(), RESULT_PREVIEW_BYTES * 2);
        assert_eq!(preview("small"), ("small", false));
    }

    fn sample_outcome(call_id: u64, provider: u64, sequence: u64) -> ToolCallOutcomeRecord {
        use chatarium_core::routing::RouteId;
        use chatarium_core::session::SessionId;
        use chatarium_core::tool::ToolCallId;
        use chatarium_store::tool_outcome_audit::ToolCallOutcomeKind;
        ToolCallOutcomeRecord {
            call_id: ToolCallId::new(call_id),
            route_id: RouteId::new(call_id),
            provider_id: ToolProviderId::new(provider),
            source_session_id: SessionId::new(1),
            kind: ToolCallOutcomeKind::Result,
            text: "untrusted observation".to_owned(),
            call_recorded_sequence: 1,
            route_bound_sequence: 2,
            dispatch_sequence: 3,
            observed_sequence: sequence,
        }
    }

    #[test]
    fn focus_never_exposes_another_conversation_or_provider() {
        let own = LocalConversationId::new();
        let other = LocalConversationId::new();
        let outcomes = vec![
            sample_outcome(1, 7, 8),
            sample_outcome(2, 7, 9),
            sample_outcome(3, 8, 10),
        ];
        let chosen = focus_recent_result(&outcomes, &[], ToolProviderId::new(7), own, |outcome| {
            Ok(Some(if outcome.call_id.get() == 2 {
                other
            } else {
                own
            }))
        })
        .unwrap()
        .unwrap();
        assert_eq!(chosen.outcome.call_id.get(), 1);
        assert_eq!(chosen.stage, ResultReviewStage::ExcludedByDefault);
    }

    #[test]
    fn ownership_failure_blocks_entire_focus_without_fallback() {
        let own = LocalConversationId::new();
        let outcomes = vec![sample_outcome(1, 7, 8)];
        let err = focus_recent_result(&outcomes, &[], ToolProviderId::new(7), own, |_| {
            Err("historical owner projection rejected".to_owned())
        });
        assert!(matches!(err, Err(ref msg) if msg.contains("projection rejected")));
    }

    fn sample_decision(
        outcome: &ToolCallOutcomeRecord,
        owner: LocalConversationId,
        decision: ToolResultContextDecision,
    ) -> ToolResultContextRecord {
        ToolResultContextRecord {
            call_id: outcome.call_id,
            route_id: outcome.route_id,
            provider_id: outcome.provider_id,
            source_session_id: outcome.source_session_id,
            conversation_id: owner,
            outcome_sequence: outcome.observed_sequence,
            outcome_kind: outcome.kind,
            decision,
            first_decision_sequence: outcome.observed_sequence + 1,
            last_decision_sequence: outcome.observed_sequence + 1,
        }
    }

    #[test]
    fn queue_shows_multiple_owned_results_in_review_priority_order() {
        let own = LocalConversationId::new();
        let other = LocalConversationId::new();
        let outcomes = vec![
            sample_outcome(1, 7, 10),
            sample_outcome(2, 7, 11),
            sample_outcome(3, 7, 12),
            sample_outcome(4, 7, 13),
            sample_outcome(5, 8, 14),
        ];
        let decisions = vec![
            sample_decision(&outcomes[1], own, ToolResultContextDecision::Admit),
            sample_decision(&outcomes[2], own, ToolResultContextDecision::Exclude),
        ];
        let queue = recent_result_queue(
            &outcomes,
            &decisions,
            ToolProviderId::new(7),
            own,
            |outcome| {
                Ok(Some(if outcome.call_id.get() == 4 {
                    other
                } else {
                    own
                }))
            },
        )
        .unwrap();
        assert_eq!(
            queue
                .iter()
                .map(|item| item.outcome.call_id.get())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(queue[0].stage, ResultReviewStage::ExcludedByDefault);
        assert_eq!(queue[1].stage, ResultReviewStage::Admitted);
        assert_eq!(queue[2].stage, ResultReviewStage::ExplicitlyExcluded);
        assert!(queue[0].stage.may_admit_from_preview(false));
        assert!(!queue[1].stage.may_admit());
        assert!(!queue[2].stage.may_exclude());
    }

    #[test]
    fn queue_keeps_newest_first_within_each_review_stage() {
        let own = LocalConversationId::new();
        let outcomes = vec![
            sample_outcome(1, 7, 10),
            sample_outcome(2, 7, 12),
            sample_outcome(3, 7, 11),
        ];
        let queue = recent_result_queue(
            &outcomes,
            &[],
            ToolProviderId::new(7),
            own,
            |_| Ok(Some(own)),
        )
        .unwrap();
        assert_eq!(
            queue
                .iter()
                .map(|item| item.outcome.call_id.get())
                .collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn queue_limits_provider_window_before_cross_conversation_filtering() {
        let own = LocalConversationId::new();
        let outcomes = (1..=RECENT_RESULT_CANDIDATES + 3)
            .map(|number| sample_outcome(number as u64, 7, number as u64))
            .collect::<Vec<_>>();
        let queue = recent_result_queue(
            &outcomes,
            &[],
            ToolProviderId::new(7),
            own,
            |_| Ok(Some(own)),
        )
        .unwrap();
        assert_eq!(queue.len(), RECENT_RESULT_CANDIDATES);
        assert_eq!(queue[0].outcome.observed_sequence, (RECENT_RESULT_CANDIDATES + 3) as u64);
        assert!(queue.iter().all(|entry| entry.outcome.observed_sequence >= 4));
    }

    #[test]
    fn corrupt_or_duplicate_decision_blocks_entire_queue() {
        let own = LocalConversationId::new();
        let outcome = sample_outcome(1, 7, 10);
        let good = sample_decision(&outcome, own, ToolResultContextDecision::Admit);
        let mut wrong = good;
        wrong.route_id = chatarium_core::routing::RouteId::new(999);
        let outcomes = vec![outcome];
        assert!(recent_result_queue(
            &outcomes,
            &[wrong],
            ToolProviderId::new(7),
            own,
            |_| Ok(Some(own)),
        )
        .is_err());
        assert!(recent_result_queue(
            &outcomes,
            &[good, good],
            ToolProviderId::new(7),
            own,
            |_| Ok(Some(own)),
        )
        .is_err());
    }

    #[test]
    fn queue_replay_failure_and_oversized_results_fail_closed() {
        let own = LocalConversationId::new();
        let mut oversized = sample_outcome(2, 7, 11);
        oversized.text = "x".repeat(MAX_CONTEXT_TOOL_RESULT_BYTES + 1);
        let outcomes = vec![sample_outcome(1, 7, 10), oversized];
        let queue = recent_result_queue(
            &outcomes,
            &[],
            ToolProviderId::new(7),
            own,
            |_| Ok(Some(own)),
        )
        .unwrap();
        assert_eq!(queue[1].stage, ResultReviewStage::TooLargeToAdmit);
        assert!(!queue[1].stage.may_admit());
        assert!(recent_result_queue(
            &outcomes,
            &[],
            ToolProviderId::new(7),
            own,
            |outcome| {
                if outcome.call_id.get() == 1 {
                    Err("owner replay failed".to_owned())
                } else {
                    Ok(Some(own))
                }
            },
        )
        .is_err());
    }

    #[test]
    fn undecided_result_precedes_admitted_then_explicitly_excluded() {
        assert!(
            ResultReviewStage::ExcludedByDefault.focus_priority()
                > ResultReviewStage::Admitted.focus_priority()
        );
        assert!(
            ResultReviewStage::Admitted.focus_priority()
                > ResultReviewStage::ExplicitlyExcluded.focus_priority()
        );
    }
}
