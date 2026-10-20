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

/// An absent admission category has no payload to validate or admit.
/// Categories that do exist still execute the original strict replay.
pub(crate) fn replay_when<T>(
    admitted: bool,
    replay: impl FnOnce() -> Result<Vec<T>, String>,
) -> Result<Vec<T>, String> {
    if admitted {
        replay()
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_admission_skips_costly_replay_without_losing_present_errors() {
        let skipped: Result<Vec<u64>, String> = replay_when(false, || {
            panic!("no expensive replay should run for an absent category")
        });
        assert_eq!(skipped.unwrap(), Vec::<u64>::new());
        let actual: Result<Vec<u64>, String> = replay_when(true, || Ok(vec![4, 9]));
        assert_eq!(actual.unwrap(), vec![4, 9]);
        let denied: Result<Vec<u64>, String> = replay_when(true, || Err("invalid provenance".into()));
        assert_eq!(denied.unwrap_err(), "invalid provenance");
    }

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
