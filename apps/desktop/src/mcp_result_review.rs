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

/// Resolve historical ownership separately for each recent observation.
/// A replay failure blocks the entire focus rather than masking an invalid
/// journal as an empty or authorized queue. Older results stay in the audit.
pub fn focus_recent_result<'a>(
    outcomes: &'a [ToolCallOutcomeRecord],
    decisions: &[ToolResultContextRecord],
    provider_id: ToolProviderId,
    conversation_id: LocalConversationId,
    mut owning_conversation: impl FnMut(
        &ToolCallOutcomeRecord,
    ) -> Result<Option<LocalConversationId>, String>,
) -> Result<Option<FocusedResult<'a>>, String> {
    let mut candidate_outcomes = outcomes
        .iter()
        .filter(|outcome| outcome.provider_id == provider_id)
        .collect::<Vec<_>>();
    candidate_outcomes.sort_unstable_by_key(|outcome| std::cmp::Reverse(outcome.observed_sequence));
    let mut focus: Option<FocusedResult<'a>> = None;
    for outcome in candidate_outcomes
        .into_iter()
        .take(RECENT_RESULT_CANDIDATES)
    {
        if owning_conversation(outcome)? != Some(conversation_id) {
            continue;
        }
        let record = decisions
            .iter()
            .find(|record| record.call_id == outcome.call_id);
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
        let stage = classify(record.map(|record| record.decision), outcome.text.len());
        if focus
            .as_ref()
            .is_none_or(|current| stage.focus_priority() > current.stage.focus_priority())
        {
            focus = Some(FocusedResult { outcome, stage });
        }
    }
    Ok(focus)
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
