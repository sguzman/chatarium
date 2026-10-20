//! Lightweight Send-time gate for expensive journal admission replays.
//! A full provenance replay is necessary only when an admission decision
//! of the corresponding kind exists in the durable journal.

use chatarium_core::EventKind;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Presence {
    pub routed: bool,
    pub tools: bool,
    pub local_memory: bool,
    pub controller_results: bool,
    pub coordination_results: bool,
}

impl Presence {
    pub fn from_kinds(kinds: impl IntoIterator<Item = EventKind>) -> Self {
        let mut result = Self::default();
        for kind in kinds {
            match kind {
                EventKind::LocalRouteContextDecisionRecorded => result.routed = true,
                EventKind::ToolResultContextDecisionRecorded => result.tools = true,
                EventKind::LocalMemoryContextDecisionRecorded => result.local_memory = true,
                EventKind::ControllerWorkerResultContextDecisionRecorded => {
                    result.controller_results = true;
                }
                EventKind::ControllerCoordinationResultContextDecisionRecorded => {
                    result.coordination_results = true;
                }
                _ => {}
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_conversation_journal_needs_no_heavy_admission_replay() {
        let events = std::iter::repeat_n(EventKind::AssistantDeltaObserved, 200_000)
            .chain([
                EventKind::UserMessageCommitted,
                EventKind::AssistantCompletionObserved,
                EventKind::DraftChanged,
            ]);
        assert_eq!(Presence::from_kinds(events), Presence::default());
    }

    #[test]
    fn any_admission_decision_requires_its_provenance_replay() {
        let events = [
            EventKind::LocalRouteContextDecisionRecorded,
            EventKind::ToolResultContextDecisionRecorded,
            EventKind::LocalMemoryContextDecisionRecorded,
            EventKind::ControllerWorkerResultContextDecisionRecorded,
            EventKind::ControllerCoordinationResultContextDecisionRecorded,
        ];
        assert_eq!(
            Presence::from_kinds(events),
            Presence {
                routed: true,
                tools: true,
                local_memory: true,
                controller_results: true,
                coordination_results: true,
            }
        );
    }

    #[test]
    fn unrelated_event_kinds_never_authorize_a_context_category() {
        let events = [
            EventKind::ToolCallOutcomeObserved,
            EventKind::LocalMemoryArtifactRecorded,
            EventKind::LocalRouteDelivered,
            EventKind::ControllerCoordinationTurnResultRecorded,
        ];
        assert_eq!(Presence::from_kinds(events), Presence::default());
    }
}
