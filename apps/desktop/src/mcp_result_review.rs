//! Read-only presentation helpers for observed local tool results.
//!
//! Neither an observed result nor a visible preview grants inference context.
//! Only the separate durable tool-result-context decision can admit it.

use chatarium_store::tool_result_context_audit::{
    MAX_CONTEXT_TOOL_RESULT_BYTES, ToolResultContextDecision,
};

pub const RESULT_PREVIEW_BYTES: usize = 4 * 1024;

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
    fn preview_is_bounded_unicode_safe_and_never_modifies_source() {
        let text = "é".repeat(RESULT_PREVIEW_BYTES);
        let (prefix, truncated) = preview(&text);
        assert!(truncated);
        assert!(prefix.len() <= RESULT_PREVIEW_BYTES);
        assert!(text.starts_with(prefix));
        assert_eq!(text.len(), RESULT_PREVIEW_BYTES * 2);
        assert_eq!(preview("small"), ("small", false));
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
