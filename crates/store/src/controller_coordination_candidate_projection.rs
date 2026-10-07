//! Read-only projection of machine-produced coordination suggestion candidates.
//!
//! Candidate output is deliberately non-authoritative. Parsing this projection
//! appends no journal fact and creates no WorkerControl, route, lifecycle
//! transition, continuation authority, or durable coordination suggestion.
//! Explicit user acceptance must pass through the existing durable suggestion
//! layer.

use crate::EventEnvelope;
use crate::controller_coordination_audit::{
    ControllerCoordinationOutcome, ControllerCoordinationOutputContract,
    controller_coordination_output_text, replay_controller_coordination_audit,
};
use crate::controller_result_inbox::replay_controller_worker_results_for_conversation;
use chatarium_core::coordination_suggestion::CoordinationSuggestionAction;
use chatarium_core::orchestration::{WorkerGoalId, WorkerId};
use chatarium_core::routing::RouteId;
use chatarium_core::{LocalConversationId, LocalTurnId};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CANDIDATES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCoordinationSuggestionCandidate {
    pub candidate_index: usize,
    pub basis_result_route_id: RouteId,
    pub action: CoordinationSuggestionAction,
    pub worker_conversation_id: LocalConversationId,
    pub worker_id: WorkerId,
    pub goal_id: WorkerGoalId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCoordinationCandidateProjection {
    pub controller_conversation_id: LocalConversationId,
    pub coordination_turn_id: LocalTurnId,
    pub coordination_result_sequence: u64,
    pub summary: String,
    pub candidates: Vec<ControllerCoordinationSuggestionCandidate>,
}

/// Parse and validate one completed strict-contract coordination result.
///
/// Worker and goal identity are never trusted from model output. A candidate
/// supplies only a frozen worker-result route id and an action name; worker,
/// goal, and worker-conversation provenance are re-derived from the controller
/// result inbox as it existed before the coordination turn started.
pub fn project_controller_coordination_suggestion_candidates(
    events: &[EventEnvelope],
    coordination_turn_id: LocalTurnId,
) -> Result<ControllerCoordinationCandidateProjection, String> {
    let coordination = replay_controller_coordination_audit(events)?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            format!(
                "coordination candidate projection references unknown turn {}",
                coordination_turn_id
            )
        })?;

    if coordination.output_contract != ControllerCoordinationOutputContract::SuggestionCandidatesV1
    {
        return Err(format!(
            "coordination turn {} uses legacy freeform output and has no suggestion-candidate contract",
            coordination_turn_id
        ));
    }
    if coordination.outcome != Some(ControllerCoordinationOutcome::Completed) {
        return Err(format!(
            "coordination turn {} is not completed and cannot produce suggestion candidates",
            coordination_turn_id
        ));
    }
    let coordination_result_sequence = coordination.result_sequence.ok_or_else(|| {
        format!(
            "coordination turn {} is completed without a durable result sequence",
            coordination_turn_id
        )
    })?;

    let output =
        controller_coordination_output_text(events, coordination_turn_id)?.ok_or_else(|| {
            format!(
                "coordination turn {} has no durable output text",
                coordination_turn_id
            )
        })?;
    let parsed = parse_candidate_document(output.as_str())?;

    let start_index = events
        .iter()
        .position(|event| event.sequence == coordination.started_sequence)
        .ok_or_else(|| {
            format!(
                "coordination turn {} start event #{} disappeared",
                coordination_turn_id, coordination.started_sequence
            )
        })?;
    let prior_to_start = &events[..start_index];

    let basis_by_route = replay_controller_worker_results_for_conversation(
        prior_to_start,
        coordination.controller_conversation_id,
    )?
    .into_iter()
    .map(|item| (item.route_id, item))
    .collect::<BTreeMap<_, _>>();

    let frozen_routes = coordination
        .admitted_result_routes
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::<(RouteId, &'static str)>::new();
    let mut candidates = Vec::with_capacity(parsed.candidates.len());

    for (candidate_index, raw) in parsed.candidates.into_iter().enumerate() {
        if !frozen_routes.contains(&raw.basis_result_route_id) {
            return Err(format!(
                "coordination turn {} candidate {} references basis route {} that was not frozen into the turn",
                coordination_turn_id,
                candidate_index + 1,
                raw.basis_result_route_id.get()
            ));
        }
        let basis = basis_by_route.get(&raw.basis_result_route_id).ok_or_else(|| {
            format!(
                "coordination turn {} candidate {} basis route {} has no controller-visible worker result before coordination start",
                coordination_turn_id,
                candidate_index + 1,
                raw.basis_result_route_id.get()
            )
        })?;

        let key = (raw.basis_result_route_id, raw.action.stable_name());
        if !seen.insert(key) {
            return Err(format!(
                "coordination turn {} repeats candidate action {} for basis route {}",
                coordination_turn_id,
                raw.action.stable_name(),
                raw.basis_result_route_id.get()
            ));
        }

        candidates.push(ControllerCoordinationSuggestionCandidate {
            candidate_index,
            basis_result_route_id: raw.basis_result_route_id,
            action: raw.action,
            worker_conversation_id: basis.worker_conversation_id,
            worker_id: basis.worker_id,
            goal_id: basis.goal_id,
        });
    }

    Ok(ControllerCoordinationCandidateProjection {
        controller_conversation_id: coordination.controller_conversation_id,
        coordination_turn_id,
        coordination_result_sequence,
        summary: parsed.summary,
        candidates,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCandidateDocument {
    summary: String,
    candidates: Vec<ParsedCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedCandidate {
    basis_result_route_id: RouteId,
    action: CoordinationSuggestionAction,
}

fn parse_candidate_document(text: &str) -> Result<ParsedCandidateDocument, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        format!("coordination suggestion candidate output is not valid JSON: {error}")
    })?;
    let object = value.as_object().ok_or_else(|| {
        "coordination suggestion candidate output must be exactly one JSON object".to_owned()
    })?;
    require_exact_keys(
        object,
        &["summary", "suggestion_candidates"],
        "top-level output",
    )?;

    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .ok_or_else(|| "coordination suggestion candidate summary must be a string".to_owned())?
        .to_owned();
    if summary.trim().is_empty() {
        return Err("coordination suggestion candidate summary must not be empty".to_owned());
    }

    let array = object
        .get("suggestion_candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| "coordination suggestion_candidates must be an array".to_owned())?;
    if array.len() > MAX_CANDIDATES {
        return Err(format!(
            "coordination suggestion_candidates contains {} items; maximum is {}",
            array.len(),
            MAX_CANDIDATES
        ));
    }

    let mut candidates = Vec::with_capacity(array.len());
    for (index, value) in array.iter().enumerate() {
        let candidate = value.as_object().ok_or_else(|| {
            format!(
                "coordination suggestion candidate {} must be an object",
                index + 1
            )
        })?;
        require_exact_keys(
            candidate,
            &["basis_result_route_id", "action"],
            format!("candidate {}", index + 1).as_str(),
        )?;

        let route_id = candidate
            .get("basis_result_route_id")
            .and_then(Value::as_u64)
            .map(RouteId::new)
            .ok_or_else(|| {
                format!(
                    "coordination suggestion candidate {} basis_result_route_id must be an integer",
                    index + 1
                )
            })?;
        let action_name = candidate
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "coordination suggestion candidate {} action must be a string",
                    index + 1
                )
            })?;
        let action =
            CoordinationSuggestionAction::from_stable_name(action_name).ok_or_else(|| {
                format!(
                    "coordination suggestion candidate {} has unsupported action '{}'",
                    index + 1,
                    action_name
                )
            })?;

        candidates.push(ParsedCandidate {
            basis_result_route_id: route_id,
            action,
        });
    }

    Ok(ParsedCandidateDocument {
        summary,
        candidates,
    })
}

fn require_exact_keys(
    object: &Map<String, Value>,
    expected: &[&str],
    label: &str,
) -> Result<(), String> {
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        let actual = object.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "coordination suggestion {label} must contain exactly [{}]; observed [{}]",
            expected.join(", "),
            actual
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_candidate_document_parses_minimal_authority_free_schema() {
        let parsed = parse_candidate_document(
            r#"{"summary":"Two workers are ready.","suggestion_candidates":[{"basis_result_route_id":7,"action":"status_request"},{"basis_result_route_id":9,"action":"continue"}]}"#,
        )
        .unwrap();

        assert_eq!(parsed.summary, "Two workers are ready.");
        assert_eq!(
            parsed.candidates,
            vec![
                ParsedCandidate {
                    basis_result_route_id: RouteId::new(7),
                    action: CoordinationSuggestionAction::StatusRequest,
                },
                ParsedCandidate {
                    basis_result_route_id: RouteId::new(9),
                    action: CoordinationSuggestionAction::Continue,
                },
            ]
        );
    }

    #[test]
    fn empty_candidate_array_is_valid_but_summary_is_required() {
        let parsed = parse_candidate_document(
            r#"{"summary":"No worker action is appropriate.","suggestion_candidates":[]}"#,
        )
        .unwrap();
        assert!(parsed.candidates.is_empty());

        assert!(
            parse_candidate_document(r#"{"summary":" ","suggestion_candidates":[]}"#)
                .unwrap_err()
                .contains("must not be empty")
        );
    }

    #[test]
    fn parser_rejects_authority_smuggling_and_unknown_actions() {
        let extra_top = json!({
            "summary": "x",
            "suggestion_candidates": [],
            "control_id": 99,
        })
        .to_string();
        assert!(
            parse_candidate_document(extra_top.as_str())
                .unwrap_err()
                .contains("exactly")
        );

        let extra_candidate = json!({
            "summary": "x",
            "suggestion_candidates": [{
                "basis_result_route_id": 7,
                "action": "stop",
                "worker_id": 3,
            }],
        })
        .to_string();
        assert!(
            parse_candidate_document(extra_candidate.as_str())
                .unwrap_err()
                .contains("exactly")
        );

        let bad_action = json!({
            "summary": "x",
            "suggestion_candidates": [{
                "basis_result_route_id": 7,
                "action": "execute_everything",
            }],
        })
        .to_string();
        assert!(
            parse_candidate_document(bad_action.as_str())
                .unwrap_err()
                .contains("unsupported action")
        );
    }

    #[test]
    fn parser_rejects_wrappers_nonobjects_and_oversized_candidate_sets() {
        assert!(
            parse_candidate_document(
                "~~~json\n{\"summary\":\"x\",\"suggestion_candidates\":[]}\n~~~"
            )
            .unwrap_err()
            .contains("not valid JSON")
        );
        assert!(
            parse_candidate_document(r#"["not","an","object"]"#)
                .unwrap_err()
                .contains("exactly one JSON object")
        );

        let too_many = json!({
            "summary": "x",
            "suggestion_candidates": (0..=MAX_CANDIDATES)
                .map(|index| json!({
                    "basis_result_route_id": index as u64 + 1,
                    "action": "status_request",
                }))
                .collect::<Vec<_>>(),
        })
        .to_string();
        assert!(
            parse_candidate_document(too_many.as_str())
                .unwrap_err()
                .contains("maximum")
        );
    }
}
