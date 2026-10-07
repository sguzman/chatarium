//! Durable context-admission decisions for terminal controller coordination results.
//!
//! Coordination execution and later model-context use are separate. This audit
//! references the already-durable coordination result and never duplicates
//! output text or mutates transcript, lifecycle, control, or continuation state.

use crate::controller_coordination_audit::replay_controller_coordination_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-controller-coordination-result-context-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerCoordinationResultContextDecision {
    Admit,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerCoordinationResultContextRecord {
    pub controller_conversation_id: LocalConversationId,
    pub coordination_turn_id: LocalTurnId,
    pub result_sequence: u64,
    pub decision: ControllerCoordinationResultContextDecision,
    pub first_decision_sequence: u64,
    pub last_decision_sequence: u64,
}

impl ControllerCoordinationResultContextRecord {
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(
            self.decision,
            ControllerCoordinationResultContextDecision::Admit
        )
    }
}

pub fn record_controller_coordination_result_context_decision(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
    result_sequence: u64,
    decision: ControllerCoordinationResultContextDecision,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "controller_coordination_result_context_decision",
        "controller_conversation_id": controller_conversation_id.to_string(),
        "coordination_turn_id": coordination_turn_id.to_string(),
        "result_sequence": result_sequence,
        "decision": decision_name(decision),
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(context_scope(
            controller_conversation_id,
            coordination_turn_id,
        )),
        EventKind::ControllerCoordinationResultContextDecisionRecorded,
        encoded,
    )
}

pub fn replay_controller_coordination_result_context_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControllerCoordinationResultContextRecord>, String> {
    let mut by_turn =
        BTreeMap::<LocalTurnId, ControllerCoordinationResultContextRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ControllerCoordinationResultContextDecisionRecorded {
            continue;
        }

        let value = typed_payload(event)?;
        let controller_conversation_id =
            LocalConversationId::from_str(required_string(&value, "controller_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "coordination result context decision at sequence {} has invalid controller conversation id: {error}",
                        event.sequence
                    )
                })?;
        let coordination_turn_id =
            LocalTurnId::from_str(required_string(&value, "coordination_turn_id")?).map_err(
                |error| {
                    format!(
                        "coordination result context decision at sequence {} has invalid coordination turn id: {error}",
                        event.sequence
                    )
                },
            )?;
        let result_sequence = required_u64(&value, "result_sequence")?;
        let decision = parse_decision(required_string(&value, "decision")?)?;
        validate_scope(event, controller_conversation_id, coordination_turn_id)?;

        let prior = &events[..index];
        let coordination = replay_controller_coordination_audit(prior)?
            .into_iter()
            .find(|record| record.coordination_turn_id == coordination_turn_id)
            .ok_or_else(|| {
                format!(
                    "coordination result context decision at sequence {} references turn {} before coordination start",
                    event.sequence, coordination_turn_id
                )
            })?;
        if coordination.controller_conversation_id != controller_conversation_id {
            return Err(format!(
                "coordination result context decision at sequence {} targets conversation {}, but turn {} belongs to {}",
                event.sequence,
                controller_conversation_id,
                coordination_turn_id,
                coordination.controller_conversation_id
            ));
        }
        let terminal_result_sequence = coordination.result_sequence.ok_or_else(|| {
            format!(
                "coordination result context decision at sequence {} references turn {} before terminal result",
                event.sequence, coordination_turn_id
            )
        })?;
        if terminal_result_sequence != result_sequence {
            return Err(format!(
                "coordination result context decision at sequence {} references result #{}, turn {} terminal result is #{}",
                event.sequence,
                result_sequence,
                coordination_turn_id,
                terminal_result_sequence
            ));
        }
        if result_sequence >= event.sequence {
            return Err(format!(
                "coordination result context decision at sequence {} does not follow result sequence {}",
                event.sequence, result_sequence
            ));
        }

        match by_turn.get_mut(&coordination_turn_id) {
            Some(record) => {
                if record.controller_conversation_id != controller_conversation_id
                    || record.result_sequence != result_sequence
                {
                    return Err(format!(
                        "coordination result context decision at sequence {} conflicts with earlier turn {} provenance",
                        event.sequence, coordination_turn_id
                    ));
                }
                record.decision = decision;
                record.last_decision_sequence = event.sequence;
            }
            None => {
                by_turn.insert(
                    coordination_turn_id,
                    ControllerCoordinationResultContextRecord {
                        controller_conversation_id,
                        coordination_turn_id,
                        result_sequence,
                        decision,
                        first_decision_sequence: event.sequence,
                        last_decision_sequence: event.sequence,
                    },
                );
            }
        }
    }

    let mut records = by_turn.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.first_decision_sequence);
    Ok(records)
}

pub fn replay_admitted_controller_coordination_result_context(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<ControllerCoordinationResultContextRecord>, String> {
    Ok(replay_controller_coordination_result_context_audit(events)?
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
    coordination_turn_id: LocalTurnId,
) -> String {
    format!(
        "controller-coordination-result-context:{controller_conversation_id}:{coordination_turn_id}"
    )
}

const fn decision_name(decision: ControllerCoordinationResultContextDecision) -> &'static str {
    match decision {
        ControllerCoordinationResultContextDecision::Admit => "admit",
        ControllerCoordinationResultContextDecision::Exclude => "exclude",
    }
}

fn parse_decision(value: &str) -> Result<ControllerCoordinationResultContextDecision, String> {
    match value {
        "admit" => Ok(ControllerCoordinationResultContextDecision::Admit),
        "exclude" => Ok(ControllerCoordinationResultContextDecision::Exclude),
        _ => Err(format!(
            "unsupported controller coordination result context decision '{value}'"
        )),
    }
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed controller coordination result context decision at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "controller coordination result context decision at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "controller coordination result context decision at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str)
        != Some("controller_coordination_result_context_decision")
    {
        return Err(format!(
            "controller coordination result context decision at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
) -> Result<(), String> {
    let expected = context_scope(controller_conversation_id, coordination_turn_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "controller coordination result context decision at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!(
            "typed controller coordination result context decision is missing integer field '{field}'"
        )
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!(
            "typed controller coordination result context decision is missing string field '{field}'"
        )
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
