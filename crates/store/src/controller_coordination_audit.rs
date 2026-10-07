//! Durable non-authored controller coordination turns.
//!
//! A coordination turn lets an explicitly designated local controller reason
//! over the worker results that were already admitted to its model context when
//! the turn was started. It never fabricates UserMessageCommitted, issues a
//! worker control, mutates WorkerLifecycle, or consumes continuation authority.

use crate::authored::local_turn_scope;
use crate::continuation_execution_audit::replay_worker_continuation_execution_audit;
use crate::controller_result_context_audit::replay_admitted_controller_worker_result_context;
use crate::local_conversation_chat_container_audit::replay_local_conversation_topologies;
use crate::supervision_audit::replay_supervision_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::routing::RouteId;
use chatarium_core::session::SessionId;
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SCHEMA: &str = "chatarium-controller-coordination-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerCoordinationTransportState {
    pub dispatch_sequence: Option<u64>,
    pub acceptance_sequence: Option<u64>,
    pub stream_started_sequence: Option<u64>,
    pub latest_output_sequence: Option<u64>,
    pub completion_sequence: Option<u64>,
    pub failure_sequence: Option<u64>,
    pub interruption_sequence: Option<u64>,
}

impl ControllerCoordinationTransportState {
    #[must_use]
    pub const fn was_dispatched(self) -> bool {
        self.dispatch_sequence.is_some()
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.completion_sequence.is_some()
            || self.failure_sequence.is_some()
            || self.interruption_sequence.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerCoordinationOutcome {
    Completed,
    FailedObserved,
    Interrupted,
}

impl ControllerCoordinationOutcome {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::FailedObserved => "failed_observed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCoordinationRecord {
    pub controller_conversation_id: LocalConversationId,
    pub controller_session_id: SessionId,
    pub coordination_turn_id: LocalTurnId,
    pub admitted_result_routes: Vec<RouteId>,
    pub started_sequence: u64,
    pub outcome: Option<ControllerCoordinationOutcome>,
    pub terminal_sequence: Option<u64>,
    pub result_sequence: Option<u64>,
}

pub fn record_controller_coordination_started(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    controller_session_id: SessionId,
    coordination_turn_id: LocalTurnId,
    admitted_result_routes: &[RouteId],
) -> std::io::Result<u64> {
    append_typed(
        store,
        coordination_scope(controller_conversation_id, coordination_turn_id),
        EventKind::ControllerCoordinationTurnStarted,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "controller_coordination_started",
            "controller_conversation_id": controller_conversation_id.to_string(),
            "controller_session_id": controller_session_id.get(),
            "coordination_turn_id": coordination_turn_id.to_string(),
            "admitted_result_route_ids": admitted_result_routes
                .iter()
                .map(|route_id| route_id.get())
                .collect::<Vec<_>>(),
        }),
    )
}

pub fn record_controller_coordination_result(
    store: &mut impl EventStore,
    record: &ControllerCoordinationRecord,
    outcome: ControllerCoordinationOutcome,
    terminal_sequence: u64,
) -> std::io::Result<u64> {
    append_typed(
        store,
        coordination_scope(
            record.controller_conversation_id,
            record.coordination_turn_id,
        ),
        EventKind::ControllerCoordinationTurnResultRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "controller_coordination_result",
            "controller_conversation_id": record.controller_conversation_id.to_string(),
            "controller_session_id": record.controller_session_id.get(),
            "coordination_turn_id": record.coordination_turn_id.to_string(),
            "started_sequence": record.started_sequence,
            "outcome": outcome.stable_name(),
            "terminal_sequence": terminal_sequence,
        }),
    )
}

pub fn replay_controller_coordination_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControllerCoordinationRecord>, String> {
    let mut by_turn = BTreeMap::<LocalTurnId, ControllerCoordinationRecord>::new();

    for (index, event) in events.iter().enumerate() {
        match event.kind {
            EventKind::ControllerCoordinationTurnStarted => {
                let value = typed_payload(event, "controller_coordination_started")?;
                let controller_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let controller_session_id =
                    SessionId::new(required_u64(&value, "controller_session_id")?);
                let coordination_turn_id = parse_turn_id(&value, event.sequence)?;
                let admitted_result_routes = parse_route_ids(&value)?;
                validate_scope(event, controller_conversation_id, coordination_turn_id)?;

                if admitted_result_routes.is_empty() {
                    return Err(format!(
                        "controller coordination start at sequence {} has no admitted worker results",
                        event.sequence
                    ));
                }
                if by_turn.contains_key(&coordination_turn_id) {
                    return Err(format!(
                        "controller coordination turn {} is started more than once",
                        coordination_turn_id
                    ));
                }

                let prior = &events[..index];
                let turn_scope = local_turn_scope(coordination_turn_id);
                if prior
                    .iter()
                    .any(|prior_event| prior_event.scope.as_deref() == Some(turn_scope.as_str()))
                {
                    return Err(format!(
                        "controller coordination turn {} reuses an existing local turn scope",
                        coordination_turn_id
                    ));
                }
                if replay_worker_continuation_execution_audit(prior)?
                    .into_iter()
                    .any(|execution| execution.execution_turn_id == coordination_turn_id)
                {
                    return Err(format!(
                        "controller coordination turn {} reuses a worker continuation turn identity",
                        coordination_turn_id
                    ));
                }

                let topology = replay_local_conversation_topologies(prior)?
                    .into_iter()
                    .find(|record| record.conversation_id == controller_conversation_id)
                    .ok_or_else(|| {
                        format!(
                            "controller coordination start at sequence {} references local conversation {} without orchestration topology",
                            event.sequence, controller_conversation_id
                        )
                    })?;
                if topology.current_session_id != controller_session_id {
                    return Err(format!(
                        "controller coordination start at sequence {} names session {}, but controller conversation {} current session is {}",
                        event.sequence,
                        controller_session_id.get(),
                        controller_conversation_id,
                        topology.current_session_id.get()
                    ));
                }

                let supervision = replay_supervision_audit(prior)?;
                let designation = supervision
                    .controllers
                    .iter()
                    .find(|record| {
                        record.designation.session_id() == controller_session_id
                    })
                    .ok_or_else(|| {
                        format!(
                            "controller coordination start at sequence {} references session {} before controller designation",
                            event.sequence,
                            controller_session_id.get()
                        )
                    })?;
                if designation.designated_sequence >= event.sequence {
                    return Err(format!(
                        "controller coordination start at sequence {} precedes controller designation at sequence {}",
                        event.sequence, designation.designated_sequence
                    ));
                }

                let mut expected_routes = replay_admitted_controller_worker_result_context(
                    prior,
                    controller_conversation_id,
                )?
                .into_iter()
                .map(|record| record.route_id)
                .collect::<Vec<_>>();
                expected_routes.sort_by_key(|route_id| route_id.get());

                let mut recorded_routes = admitted_result_routes.clone();
                recorded_routes.sort_by_key(|route_id| route_id.get());
                if recorded_routes.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err(format!(
                        "controller coordination start at sequence {} contains duplicate admitted result routes",
                        event.sequence
                    ));
                }
                if recorded_routes != expected_routes {
                    return Err(format!(
                        "controller coordination start at sequence {} admitted-result snapshot disagrees with durable context decisions",
                        event.sequence
                    ));
                }

                by_turn.insert(
                    coordination_turn_id,
                    ControllerCoordinationRecord {
                        controller_conversation_id,
                        controller_session_id,
                        coordination_turn_id,
                        admitted_result_routes: recorded_routes,
                        started_sequence: event.sequence,
                        outcome: None,
                        terminal_sequence: None,
                        result_sequence: None,
                    },
                );
            }
            EventKind::ControllerCoordinationTurnResultRecorded => {
                let value = typed_payload(event, "controller_coordination_result")?;
                let controller_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let controller_session_id =
                    SessionId::new(required_u64(&value, "controller_session_id")?);
                let coordination_turn_id = parse_turn_id(&value, event.sequence)?;
                let started_sequence = required_u64(&value, "started_sequence")?;
                let outcome = parse_outcome(required_string(&value, "outcome")?)?;
                let terminal_sequence = required_u64(&value, "terminal_sequence")?;
                validate_scope(event, controller_conversation_id, coordination_turn_id)?;

                let record = by_turn.get_mut(&coordination_turn_id).ok_or_else(|| {
                    format!(
                        "controller coordination result at sequence {} references turn {} before start",
                        event.sequence, coordination_turn_id
                    )
                })?;
                if record.result_sequence.is_some() {
                    return Err(format!(
                        "controller coordination turn {} already has a terminal result",
                        coordination_turn_id
                    ));
                }
                if record.controller_conversation_id != controller_conversation_id
                    || record.controller_session_id != controller_session_id
                    || record.started_sequence != started_sequence
                {
                    return Err(format!(
                        "controller coordination result at sequence {} disagrees with start provenance",
                        event.sequence
                    ));
                }
                if terminal_sequence >= event.sequence {
                    return Err(format!(
                        "controller coordination result at sequence {} does not follow terminal transport event {}",
                        event.sequence, terminal_sequence
                    ));
                }

                let transport = controller_coordination_transport_state(
                    &events[..index],
                    coordination_turn_id,
                )?;
                let (expected_outcome, expected_terminal) =
                    coordination_terminal_outcome(transport).ok_or_else(|| {
                        format!(
                            "controller coordination result at sequence {} has no prior terminal transport evidence",
                            event.sequence
                        )
                    })?;
                if outcome != expected_outcome || terminal_sequence != expected_terminal {
                    return Err(format!(
                        "controller coordination result at sequence {} disagrees with terminal transport evidence",
                        event.sequence
                    ));
                }

                record.outcome = Some(outcome);
                record.terminal_sequence = Some(terminal_sequence);
                record.result_sequence = Some(event.sequence);
            }
            _ => {}
        }
    }

    let mut records = by_turn.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.started_sequence);
    for record in &records {
        controller_coordination_transport_state(events, record.coordination_turn_id)?;
    }
    Ok(records)
}

pub fn controller_coordination_transport_state(
    events: &[EventEnvelope],
    coordination_turn_id: LocalTurnId,
) -> Result<ControllerCoordinationTransportState, String> {
    let scope = local_turn_scope(coordination_turn_id);
    let mut state = ControllerCoordinationTransportState::default();

    for event in events
        .iter()
        .filter(|event| event.scope.as_deref() == Some(scope.as_str()))
    {
        if event.kind == EventKind::UserMessageCommitted {
            return Err(format!(
                "non-authored controller coordination turn {} contains a user-message commit at sequence {}",
                coordination_turn_id, event.sequence
            ));
        }

        match event.kind {
            EventKind::DispatchAttempted => {
                if state.dispatch_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "controller coordination turn {} has multiple dispatch attempts",
                        coordination_turn_id
                    ));
                }
            }
            EventKind::RemoteAcceptanceObserved => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                if state.acceptance_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "controller coordination turn {} has multiple remote-acceptance observations",
                        coordination_turn_id
                    ));
                }
            }
            EventKind::AssistantStreamStarted => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                if state
                    .stream_started_sequence
                    .replace(event.sequence)
                    .is_some()
                {
                    return Err(format!(
                        "controller coordination turn {} has multiple assistant-stream starts",
                        coordination_turn_id
                    ));
                }
            }
            EventKind::AssistantDeltaObserved | EventKind::AssistantSnapshotObserved => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                state.latest_output_sequence = Some(event.sequence);
            }
            EventKind::AssistantCompletionObserved => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                if state.completion_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "controller coordination turn {} has multiple completion observations",
                        coordination_turn_id
                    ));
                }
                if state.failure_sequence.is_some() || state.interruption_sequence.is_some() {
                    return Err(format!(
                        "controller coordination turn {} records completion after failure/interruption",
                        coordination_turn_id
                    ));
                }
                state.latest_output_sequence = Some(event.sequence);
            }
            EventKind::RemoteFailureObserved => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                if state.failure_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "controller coordination turn {} has multiple definitive failure observations",
                        coordination_turn_id
                    ));
                }
                if state.completion_sequence.is_some() {
                    return Err(format!(
                        "controller coordination turn {} records remote failure after completion",
                        coordination_turn_id
                    ));
                }
            }
            EventKind::TransportInterrupted => {
                require_dispatch(state, coordination_turn_id, event.sequence)?;
                if state
                    .interruption_sequence
                    .replace(event.sequence)
                    .is_some()
                {
                    return Err(format!(
                        "controller coordination turn {} has multiple interruption observations",
                        coordination_turn_id
                    ));
                }
                if state.completion_sequence.is_some() {
                    return Err(format!(
                        "controller coordination turn {} records interruption after completion",
                        coordination_turn_id
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(state)
}

pub fn controller_coordination_output_text(
    events: &[EventEnvelope],
    coordination_turn_id: LocalTurnId,
) -> Result<Option<String>, String> {
    controller_coordination_transport_state(events, coordination_turn_id)?;

    let scope = local_turn_scope(coordination_turn_id);
    let mut latest = None;
    for event in events
        .iter()
        .filter(|event| event.scope.as_deref() == Some(scope.as_str()))
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved
            )
        })
    {
        let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
            format!(
                "controller coordination turn {} has malformed assistant output payload at sequence {}: {error}",
                coordination_turn_id, event.sequence
            )
        })?;
        match value.get("text") {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => latest = Some(text.clone()),
            Some(_) => {
                return Err(format!(
                    "controller coordination turn {} assistant output at sequence {} has non-string text",
                    coordination_turn_id, event.sequence
                ));
            }
        }
    }

    Ok(latest.filter(|text| !text.trim().is_empty()))
}

pub fn coordination_terminal_outcome(
    state: ControllerCoordinationTransportState,
) -> Option<(ControllerCoordinationOutcome, u64)> {
    if let Some(sequence) = state.completion_sequence {
        Some((ControllerCoordinationOutcome::Completed, sequence))
    } else if let Some(sequence) = state.failure_sequence {
        Some((ControllerCoordinationOutcome::FailedObserved, sequence))
    } else {
        state
            .interruption_sequence
            .map(|sequence| (ControllerCoordinationOutcome::Interrupted, sequence))
    }
}

fn require_dispatch(
    state: ControllerCoordinationTransportState,
    coordination_turn_id: LocalTurnId,
    sequence: u64,
) -> Result<(), String> {
    if state.dispatch_sequence.is_none() {
        return Err(format!(
            "controller coordination turn {} has remote evidence at sequence {} before dispatch",
            coordination_turn_id, sequence
        ));
    }
    Ok(())
}

#[must_use]
pub fn coordination_scope(
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
) -> String {
    format!("controller-coordination:{controller_conversation_id}:{coordination_turn_id}")
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

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed controller coordination payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "controller coordination event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "controller coordination event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "controller coordination event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn parse_conversation_id(value: &Value, sequence: u64) -> Result<LocalConversationId, String> {
    LocalConversationId::from_str(required_string(value, "controller_conversation_id")?).map_err(
        |error| {
            format!(
                "controller coordination event at sequence {sequence} has invalid controller conversation id: {error}"
            )
        },
    )
}

fn parse_turn_id(value: &Value, sequence: u64) -> Result<LocalTurnId, String> {
    LocalTurnId::from_str(required_string(value, "coordination_turn_id")?).map_err(|error| {
        format!(
            "controller coordination event at sequence {sequence} has invalid coordination turn id: {error}"
        )
    })
}

fn parse_route_ids(value: &Value) -> Result<Vec<RouteId>, String> {
    value
        .get("admitted_result_route_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "typed controller coordination payload is missing array field 'admitted_result_route_ids'"
                .to_owned()
        })?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .map(RouteId::new)
                .ok_or_else(|| {
                    "typed controller coordination admitted result route id is not an integer"
                        .to_owned()
                })
        })
        .collect()
}

fn parse_outcome(value: &str) -> Result<ControllerCoordinationOutcome, String> {
    match value {
        "completed" => Ok(ControllerCoordinationOutcome::Completed),
        "failed_observed" => Ok(ControllerCoordinationOutcome::FailedObserved),
        "interrupted" => Ok(ControllerCoordinationOutcome::Interrupted),
        _ => Err(format!(
            "unsupported controller coordination outcome '{value}'"
        )),
    }
}

fn validate_scope(
    event: &EventEnvelope,
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
) -> Result<(), String> {
    let expected = coordination_scope(controller_conversation_id, coordination_turn_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "controller coordination event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed controller coordination payload is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed controller coordination payload is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
