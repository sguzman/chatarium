//! Durable context-admission decisions for controller-visible worker results.
//!
//! Worker results and controller-model context use are separate. This audit
//! references existing terminal result provenance and never duplicates result
//! content or mutates lifecycle/transcript state.

use crate::controller_result_inbox::replay_controller_worker_results;
use crate::{EventEnvelope, EventStore};
use chatarium_core::control::ControlId;
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-controller-worker-result-context-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerWorkerResultContextDecision {
    Admit,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerWorkerResultContextRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub controller_conversation_id: LocalConversationId,
    pub result_sequence: u64,
    pub decision: ControllerWorkerResultContextDecision,
    pub first_decision_sequence: u64,
    pub last_decision_sequence: u64,
}

impl ControllerWorkerResultContextRecord {
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(
            self.decision,
            ControllerWorkerResultContextDecision::Admit
        )
    }
}

pub fn record_controller_worker_result_context_decision(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    controller_conversation_id: LocalConversationId,
    result_sequence: u64,
    decision: ControllerWorkerResultContextDecision,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "controller_worker_result_context_decision",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "controller_conversation_id": controller_conversation_id.to_string(),
        "result_sequence": result_sequence,
        "decision": decision_name(decision),
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(context_scope(controller_conversation_id, route_id)),
        EventKind::ControllerWorkerResultContextDecisionRecorded,
        encoded,
    )
}

pub fn replay_controller_worker_result_context_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControllerWorkerResultContextRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, ControllerWorkerResultContextRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ControllerWorkerResultContextDecisionRecorded {
            continue;
        }

        let value = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&value, "control_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let controller_conversation_id =
            LocalConversationId::from_str(required_string(&value, "controller_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "controller result context decision at sequence {} has invalid controller conversation id: {error}",
                        event.sequence
                    )
                })?;
        let result_sequence = required_u64(&value, "result_sequence")?;
        let decision = parse_decision(required_string(&value, "decision")?)?;
        validate_scope(event, controller_conversation_id, route_id)?;

        let prior = &events[..index];
        let result = replay_controller_worker_results(prior)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "controller result context decision at sequence {} references route {} before a terminal controller-visible result",
                    event.sequence,
                    route_id.get()
                )
            })?;

        if result.control_id != control_id {
            return Err(format!(
                "controller result context decision at sequence {} control identity disagrees with route {} result",
                event.sequence,
                route_id.get()
            ));
        }
        if result.result_sequence != result_sequence {
            return Err(format!(
                "controller result context decision at sequence {} references result #{}, route {} terminal result is #{}",
                event.sequence,
                result_sequence,
                route_id.get(),
                result.result_sequence
            ));
        }
        if result.controller_conversation_id != Some(controller_conversation_id) {
            return Err(format!(
                "controller result context decision at sequence {} targets conversation {}, but route {} result belongs to {:?}",
                event.sequence,
                controller_conversation_id,
                route_id.get(),
                result.controller_conversation_id
            ));
        }
        if result_sequence >= event.sequence {
            return Err(format!(
                "controller result context decision at sequence {} does not follow terminal result sequence {}",
                event.sequence, result_sequence
            ));
        }

        match by_route.get_mut(&route_id) {
            Some(record) => {
                if record.control_id != control_id
                    || record.controller_conversation_id != controller_conversation_id
                    || record.result_sequence != result_sequence
                {
                    return Err(format!(
                        "controller result context decision at sequence {} conflicts with earlier route {} result provenance",
                        event.sequence,
                        route_id.get()
                    ));
                }
                record.decision = decision;
                record.last_decision_sequence = event.sequence;
            }
            None => {
                by_route.insert(
                    route_id,
                    ControllerWorkerResultContextRecord {
                        control_id,
                        route_id,
                        controller_conversation_id,
                        result_sequence,
                        decision,
                        first_decision_sequence: event.sequence,
                        last_decision_sequence: event.sequence,
                    },
                );
            }
        }
    }

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.first_decision_sequence);
    Ok(records)
}

pub fn replay_admitted_controller_worker_result_context(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<ControllerWorkerResultContextRecord>, String> {
    Ok(replay_controller_worker_result_context_audit(events)?
        .into_iter()
        .filter(|record| {
            record.controller_conversation_id == controller_conversation_id
                && record.is_admitted()
        })
        .collect())
}

#[must_use]
pub fn context_scope(
    controller_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> String {
    format!(
        "controller-worker-result-context:{controller_conversation_id}:{}",
        route_id.get()
    )
}

const fn decision_name(decision: ControllerWorkerResultContextDecision) -> &'static str {
    match decision {
        ControllerWorkerResultContextDecision::Admit => "admit",
        ControllerWorkerResultContextDecision::Exclude => "exclude",
    }
}

fn parse_decision(value: &str) -> Result<ControllerWorkerResultContextDecision, String> {
    match value {
        "admit" => Ok(ControllerWorkerResultContextDecision::Admit),
        "exclude" => Ok(ControllerWorkerResultContextDecision::Exclude),
        _ => Err(format!(
            "unsupported controller worker result context decision '{value}'"
        )),
    }
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed controller worker result context decision at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "controller worker result context decision at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "controller worker result context decision at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str)
        != Some("controller_worker_result_context_decision")
    {
        return Err(format!(
            "controller worker result context decision at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    controller_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<(), String> {
    let expected = context_scope(controller_conversation_id, route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "controller worker result context decision at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed controller worker result context decision is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed controller worker result context decision is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
