//! Durable, non-authoritative suggestions derived from completed controller coordination.
//!
//! A suggestion is only machine-readable proposal data. Recording one does not
//! admit a WorkerControl, consume continuation authority, mutate lifecycle,
//! create a route, approve policy, dispatch anything, or enter inference context.

use crate::controller_coordination_audit::{
    ControllerCoordinationOutcome, controller_coordination_output_text,
    replay_controller_coordination_audit,
};
use crate::controller_result_inbox::replay_controller_worker_results_for_conversation;
use crate::{EventEnvelope, EventStore};
use chatarium_core::coordination_suggestion::{
    CoordinationSuggestion, CoordinationSuggestionAction, CoordinationSuggestionId,
};
use chatarium_core::orchestration::{WorkerGoalId, WorkerId};
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SCHEMA: &str = "chatarium-controller-coordination-suggestion-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCoordinationSuggestionRecord {
    pub suggestion: CoordinationSuggestion,
    pub controller_conversation_id: LocalConversationId,
    pub coordination_turn_id: LocalTurnId,
    pub coordination_result_sequence: u64,
    pub basis_result_route_id: RouteId,
    pub worker_conversation_id: LocalConversationId,
    pub recorded_sequence: u64,
}

/// Record one powerless typed suggestion against an already-completed coordination result.
pub fn record_controller_coordination_suggestion(
    store: &mut impl EventStore,
    record: &ControllerCoordinationSuggestionRecord,
) -> std::io::Result<u64> {
    append_typed(
        store,
        suggestion_scope(record.controller_conversation_id, record.suggestion.id()),
        EventKind::ControllerCoordinationSuggestionRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "controller_coordination_suggestion",
            "suggestion_id": record.suggestion.id().get(),
            "controller_conversation_id": record.controller_conversation_id.to_string(),
            "coordination_turn_id": record.coordination_turn_id.to_string(),
            "coordination_result_sequence": record.coordination_result_sequence,
            "basis_result_route_id": record.basis_result_route_id.get(),
            "worker_conversation_id": record.worker_conversation_id.to_string(),
            "worker_id": record.suggestion.worker_id().get(),
            "goal_id": record.suggestion.goal_id().get(),
            "action": record.suggestion.action().stable_name(),
        }),
    )
}

/// Replay all durable coordination suggestions.
///
/// Replay validates each suggestion against the journal prefix immediately before
/// its record. Later worker lifecycle or supervision changes cannot rewrite the
/// historical coordination/result provenance.
pub fn replay_controller_coordination_suggestion_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControllerCoordinationSuggestionRecord>, String> {
    let mut by_id =
        BTreeMap::<CoordinationSuggestionId, ControllerCoordinationSuggestionRecord>::new();
    let mut exact_keys =
        BTreeSet::<(LocalTurnId, RouteId, WorkerId, WorkerGoalId, &'static str)>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ControllerCoordinationSuggestionRecorded {
            continue;
        }

        let value = typed_payload(event)?;
        let suggestion_id = CoordinationSuggestionId::new(required_u64(&value, "suggestion_id")?);
        let controller_conversation_id =
            parse_conversation_id(&value, "controller_conversation_id", event.sequence)?;
        let coordination_turn_id = parse_turn_id(&value, event.sequence)?;
        let coordination_result_sequence = required_u64(&value, "coordination_result_sequence")?;
        let basis_result_route_id = RouteId::new(required_u64(&value, "basis_result_route_id")?);
        let worker_conversation_id =
            parse_conversation_id(&value, "worker_conversation_id", event.sequence)?;
        let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
        let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
        let action = parse_action(required_string(&value, "action")?)?;
        validate_scope(event, controller_conversation_id, suggestion_id)?;

        if by_id.contains_key(&suggestion_id) {
            return Err(format!(
                "duplicate coordination suggestion {} at sequence {}",
                suggestion_id.get(),
                event.sequence
            ));
        }

        let prior = &events[..index];
        let coordination = replay_controller_coordination_audit(prior)?
            .into_iter()
            .find(|record| record.coordination_turn_id == coordination_turn_id)
            .ok_or_else(|| {
                format!(
                    "coordination suggestion {} at sequence {} references unknown coordination turn {}",
                    suggestion_id.get(),
                    event.sequence,
                    coordination_turn_id
                )
            })?;
        if coordination.controller_conversation_id != controller_conversation_id {
            return Err(format!(
                "coordination suggestion {} controller conversation disagrees with source coordination turn",
                suggestion_id.get()
            ));
        }
        if coordination.outcome != Some(ControllerCoordinationOutcome::Completed) {
            return Err(format!(
                "coordination suggestion {} requires a completed coordination result",
                suggestion_id.get()
            ));
        }
        if coordination.result_sequence != Some(coordination_result_sequence) {
            return Err(format!(
                "coordination suggestion {} result sequence disagrees with source coordination result",
                suggestion_id.get()
            ));
        }
        if !coordination
            .admitted_result_routes
            .contains(&basis_result_route_id)
        {
            return Err(format!(
                "coordination suggestion {} basis route {} was not frozen into coordination turn {}",
                suggestion_id.get(),
                basis_result_route_id.get(),
                coordination_turn_id
            ));
        }
        let output =
            controller_coordination_output_text(prior, coordination_turn_id)?.ok_or_else(|| {
                format!(
                    "coordination suggestion {} source coordination has no durable output text",
                    suggestion_id.get()
                )
            })?;
        if output.trim().is_empty() {
            return Err(format!(
                "coordination suggestion {} source coordination output is empty",
                suggestion_id.get()
            ));
        }

        let basis = replay_controller_worker_results_for_conversation(
            prior,
            controller_conversation_id,
        )?
        .into_iter()
        .find(|item| item.route_id == basis_result_route_id)
        .ok_or_else(|| {
            format!(
                "coordination suggestion {} basis route {} has no controller-visible worker result",
                suggestion_id.get(),
                basis_result_route_id.get()
            )
        })?;
        if basis.worker_conversation_id != worker_conversation_id
            || basis.worker_id != worker_id
            || basis.goal_id != goal_id
        {
            return Err(format!(
                "coordination suggestion {} target provenance disagrees with basis worker result route {}",
                suggestion_id.get(),
                basis_result_route_id.get()
            ));
        }

        let exact_key = (
            coordination_turn_id,
            basis_result_route_id,
            worker_id,
            goal_id,
            action.stable_name(),
        );
        if !exact_keys.insert(exact_key) {
            return Err(format!(
                "duplicate exact coordination suggestion for turn {}, basis route {}, action {}",
                coordination_turn_id,
                basis_result_route_id.get(),
                action.stable_name()
            ));
        }

        by_id.insert(
            suggestion_id,
            ControllerCoordinationSuggestionRecord {
                suggestion: CoordinationSuggestion::new(suggestion_id, worker_id, goal_id, action),
                controller_conversation_id,
                coordination_turn_id,
                coordination_result_sequence,
                basis_result_route_id,
                worker_conversation_id,
                recorded_sequence: event.sequence,
            },
        );
    }

    let mut records = by_id.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.recorded_sequence);
    Ok(records)
}

#[must_use]
pub fn suggestion_scope(
    controller_conversation_id: LocalConversationId,
    suggestion_id: CoordinationSuggestionId,
) -> String {
    format!(
        "controller-coordination-suggestion:{controller_conversation_id}:{}",
        suggestion_id.get()
    )
}

fn parse_action(value: &str) -> Result<CoordinationSuggestionAction, String> {
    match value {
        "start_or_resume" => Ok(CoordinationSuggestionAction::StartOrResume),
        "continue" => Ok(CoordinationSuggestionAction::Continue),
        "stop" => Ok(CoordinationSuggestionAction::Stop),
        "status_request" => Ok(CoordinationSuggestionAction::StatusRequest),
        other => Err(format!(
            "unsupported coordination suggestion action '{other}'"
        )),
    }
}

fn append_typed(
    store: &mut impl EventStore,
    scope: String,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(scope), kind, encoded)
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed coordination suggestion payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "coordination suggestion event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "coordination suggestion event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("controller_coordination_suggestion") {
        return Err(format!(
            "coordination suggestion event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    controller_conversation_id: LocalConversationId,
    suggestion_id: CoordinationSuggestionId,
) -> Result<(), String> {
    let expected = suggestion_scope(controller_conversation_id, suggestion_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "coordination suggestion event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn parse_conversation_id(
    value: &Value,
    field: &str,
    sequence: u64,
) -> Result<LocalConversationId, String> {
    LocalConversationId::from_str(required_string(value, field)?).map_err(|error| {
        format!("coordination suggestion event at sequence {sequence} has invalid {field}: {error}")
    })
}

fn parse_turn_id(value: &Value, sequence: u64) -> Result<LocalTurnId, String> {
    LocalTurnId::from_str(required_string(value, "coordination_turn_id")?).map_err(|error| {
        format!(
            "coordination suggestion event at sequence {sequence} has invalid coordination_turn_id: {error}"
        )
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed coordination suggestion is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed coordination suggestion is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;

    #[test]
    fn suggestion_before_coordination_is_rejected_on_replay() {
        let mut store = MemoryEventStore::default();
        let controller = LocalConversationId::new();
        let worker = LocalConversationId::new();
        let turn = LocalTurnId::new();
        let record = ControllerCoordinationSuggestionRecord {
            suggestion: CoordinationSuggestion::new(
                CoordinationSuggestionId::new(1),
                WorkerId::new(2),
                WorkerGoalId::new(3),
                CoordinationSuggestionAction::StatusRequest,
            ),
            controller_conversation_id: controller,
            coordination_turn_id: turn,
            coordination_result_sequence: 4,
            basis_result_route_id: RouteId::new(5),
            worker_conversation_id: worker,
            recorded_sequence: 0,
        };
        record_controller_coordination_suggestion(&mut store, &record).unwrap();

        let error = replay_controller_coordination_suggestion_audit(store.events()).unwrap_err();
        assert!(error.contains("unknown coordination turn"));
    }

    #[test]
    fn malformed_action_is_rejected() {
        let mut store = MemoryEventStore::default();
        let controller = LocalConversationId::new();
        let suggestion_id = CoordinationSuggestionId::new(1);
        store
            .append_scoped(
                Some(suggestion_scope(controller, suggestion_id)),
                EventKind::ControllerCoordinationSuggestionRecorded,
                json!({
                    "schema": SCHEMA,
                    "version": VERSION,
                    "record": "controller_coordination_suggestion",
                    "suggestion_id": suggestion_id.get(),
                    "controller_conversation_id": controller.to_string(),
                    "coordination_turn_id": LocalTurnId::new().to_string(),
                    "coordination_result_sequence": 1,
                    "basis_result_route_id": 1,
                    "worker_conversation_id": LocalConversationId::new().to_string(),
                    "worker_id": 1,
                    "goal_id": 1,
                    "action": "do_magic",
                })
                .to_string(),
            )
            .unwrap();

        assert!(
            replay_controller_coordination_suggestion_audit(store.events())
                .unwrap_err()
                .contains("unsupported coordination suggestion action")
        );
    }
}
