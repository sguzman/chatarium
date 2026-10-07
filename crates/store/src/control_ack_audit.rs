//! Durable acknowledgement of delivered worker controls.
//!
//! Acknowledgement means only that the local worker conversation explicitly saw
//! the delivered command. It is not execution, lifecycle mutation, status
//! evidence, or inference-context admission.

use crate::control_delivery_audit::replay_worker_control_delivery_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::control::ControlId;
use chatarium_core::orchestration::WorkerId;
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-worker-control-ack-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerControlAcknowledgementRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub delivered_sequence: u64,
    pub acknowledged_sequence: u64,
}

pub fn record_worker_control_acknowledged(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    worker_id: WorkerId,
    worker_conversation_id: LocalConversationId,
    delivered_sequence: u64,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_control_acknowledged",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "worker_id": worker_id.get(),
        "worker_conversation_id": worker_conversation_id.to_string(),
        "delivered_sequence": delivered_sequence,
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(ack_scope(worker_conversation_id, route_id, control_id)),
        EventKind::WorkerControlAcknowledged,
        encoded,
    )
}

pub fn replay_worker_control_acknowledgement_audit(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerControlAcknowledgementRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, WorkerControlAcknowledgementRecord>::new();
    let mut control_owner = BTreeMap::<ControlId, RouteId>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::WorkerControlAcknowledged {
            continue;
        }

        let value = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&value, "control_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
        let worker_conversation_id =
            LocalConversationId::from_str(required_string(&value, "worker_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "worker control acknowledgement at sequence {} has invalid conversation id: {error}",
                        event.sequence
                    )
                })?;
        let delivered_sequence = required_u64(&value, "delivered_sequence")?;
        validate_scope(event, worker_conversation_id, route_id, control_id)?;

        if let Some(existing) = by_route.get(&route_id) {
            return Err(format!(
                "worker control route {} was already acknowledged at sequence {}; cannot acknowledge again at sequence {}",
                route_id.get(),
                existing.acknowledged_sequence,
                event.sequence
            ));
        }
        if let Some(existing_route) = control_owner.get(&control_id) {
            return Err(format!(
                "worker control {} was already acknowledged on route {}; cannot also acknowledge route {} at sequence {}",
                control_id.get(),
                existing_route.get(),
                route_id.get(),
                event.sequence
            ));
        }

        let delivery = replay_worker_control_delivery_audit(&events[..index])?
            .into_iter()
            .find(|record| record.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control acknowledgement at sequence {} references route {} before delivery",
                    event.sequence,
                    route_id.get()
                )
            })?;
        if delivery.control_id != control_id
            || delivery.worker_id != worker_id
            || delivery.worker_conversation_id != worker_conversation_id
            || delivery.delivered_sequence != delivered_sequence
        {
            return Err(format!(
                "worker control acknowledgement at sequence {} disagrees with delivery provenance for route {}",
                event.sequence,
                route_id.get()
            ));
        }
        if delivered_sequence >= event.sequence {
            return Err(format!(
                "worker control acknowledgement at sequence {} does not follow delivery sequence {}",
                event.sequence, delivered_sequence
            ));
        }

        let record = WorkerControlAcknowledgementRecord {
            control_id,
            route_id,
            worker_id,
            worker_conversation_id,
            delivered_sequence,
            acknowledged_sequence: event.sequence,
        };
        by_route.insert(route_id, record);
        control_owner.insert(control_id, route_id);
    }

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.acknowledged_sequence);
    Ok(records)
}

#[must_use]
pub fn ack_scope(
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
    control_id: ControlId,
) -> String {
    format!(
        "worker-control-ack:{worker_conversation_id}:{}:{}",
        route_id.get(),
        control_id.get()
    )
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed worker control acknowledgement at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "worker control acknowledgement at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "worker control acknowledgement at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("worker_control_acknowledged") {
        return Err(format!(
            "worker control acknowledgement at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
    control_id: ControlId,
) -> Result<(), String> {
    let expected = ack_scope(worker_conversation_id, route_id, control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker control acknowledgement at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed worker control acknowledgement is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed worker control acknowledgement is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
