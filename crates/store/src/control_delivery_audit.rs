//! Durable delivery provenance for dispatched local worker controls.
//!
//! Dispatch authorization and worker lifecycle mutation remain separate. This
//! audit records only that one already-validated dispatched controller control
//! reached the local worker conversation/control inbox.

use crate::EventEnvelope;
use crate::EventStore;
use crate::control_dispatch_audit::replay_validated_control_dispatches;
use crate::local_conversation_worker_audit::replay_local_conversation_worker_bindings;
use chatarium_core::EventKind;
use chatarium_core::LocalConversationId;
use chatarium_core::control::ControlId;
use chatarium_core::control_provenance::ControlIssuer;
use chatarium_core::orchestration::WorkerId;
use chatarium_core::routing::RouteId;
use chatarium_core::session::SessionId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-worker-control-delivery-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerControlDeliveryRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub worker_session_id: SessionId,
    pub controller_session_id: SessionId,
    pub dispatch_sequence: u64,
    pub delivered_sequence: u64,
}

pub fn record_worker_control_delivered(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    worker_id: WorkerId,
    worker_conversation_id: LocalConversationId,
    worker_session_id: SessionId,
    controller_session_id: SessionId,
    dispatch_sequence: u64,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_control_delivered",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "worker_id": worker_id.get(),
        "worker_conversation_id": worker_conversation_id.to_string(),
        "worker_session_id": worker_session_id.get(),
        "controller_session_id": controller_session_id.get(),
        "dispatch_sequence": dispatch_sequence,
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(delivery_scope(route_id, control_id)),
        EventKind::WorkerControlDelivered,
        encoded,
    )
}

pub fn replay_worker_control_delivery_audit(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerControlDeliveryRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, WorkerControlDeliveryRecord>::new();
    let mut control_owner = BTreeMap::<ControlId, RouteId>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::WorkerControlDelivered {
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
                        "worker control delivery at sequence {} has invalid worker conversation id: {error}",
                        event.sequence
                    )
                })?;
        let worker_session_id = SessionId::new(required_u64(&value, "worker_session_id")?);
        let controller_session_id = SessionId::new(required_u64(&value, "controller_session_id")?);
        let dispatch_sequence = required_u64(&value, "dispatch_sequence")?;
        validate_scope(event, route_id, control_id)?;

        if let Some(existing) = by_route.get(&route_id) {
            return Err(format!(
                "controller control route {} already delivered control {} at sequence {}; cannot deliver again at sequence {}",
                route_id.get(),
                existing.control_id.get(),
                existing.delivered_sequence,
                event.sequence
            ));
        }
        if let Some(existing_route) = control_owner.get(&control_id) {
            return Err(format!(
                "controller control {} is already delivered on route {}; cannot also deliver on route {} at sequence {}",
                control_id.get(),
                existing_route.get(),
                route_id.get(),
                event.sequence
            ));
        }

        let prior = &events[..index];
        let dispatch = replay_validated_control_dispatches(prior)?
            .into_iter()
            .find(|record| record.route.id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control delivery at sequence {} references route {} before validated dispatch",
                    event.sequence,
                    route_id.get()
                )
            })?;

        if dispatch.control_id != control_id
            || dispatch.worker_id != worker_id
            || dispatch.dispatch_sequence != dispatch_sequence
        {
            return Err(format!(
                "worker control delivery at sequence {} disagrees with validated dispatch for route {}",
                event.sequence,
                route_id.get()
            ));
        }

        let expected_controller = match dispatch.issuer {
            ControlIssuer::ControllerSession(session_id) => session_id,
            ControlIssuer::User => {
                return Err(format!(
                    "worker control delivery at sequence {} references non-controller-issued route {}",
                    event.sequence,
                    route_id.get()
                ));
            }
        };
        if expected_controller != controller_session_id {
            return Err(format!(
                "worker control delivery at sequence {} controller session disagrees with validated dispatch",
                event.sequence
            ));
        }
        if dispatch.worker_session_id != Some(worker_session_id) {
            return Err(format!(
                "worker control delivery at sequence {} worker session disagrees with validated dispatch",
                event.sequence
            ));
        }

        let owner = replay_local_conversation_worker_bindings(prior)?
            .into_iter()
            .find(|binding| binding.worker_id == worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control delivery at sequence {} targets worker {} without local conversation ownership",
                    event.sequence,
                    worker_id.get()
                )
            })?;
        if owner.conversation_id != worker_conversation_id {
            return Err(format!(
                "worker control delivery at sequence {} worker conversation disagrees with durable WorkerId ownership",
                event.sequence
            ));
        }
        if dispatch_sequence >= event.sequence {
            return Err(format!(
                "worker control delivery at sequence {} does not follow dispatch sequence {}",
                event.sequence, dispatch_sequence
            ));
        }

        let record = WorkerControlDeliveryRecord {
            control_id,
            route_id,
            worker_id,
            worker_conversation_id,
            worker_session_id,
            controller_session_id,
            dispatch_sequence,
            delivered_sequence: event.sequence,
        };
        by_route.insert(route_id, record);
        control_owner.insert(control_id, route_id);
    }

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.delivered_sequence);
    Ok(records)
}

#[must_use]
pub fn delivery_scope(route_id: RouteId, control_id: ControlId) -> String {
    format!(
        "worker-control-delivery:{}:{}",
        route_id.get(),
        control_id.get()
    )
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed worker control delivery at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "worker control delivery at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "worker control delivery at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("worker_control_delivered") {
        return Err(format!(
            "worker control delivery at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    route_id: RouteId,
    control_id: ControlId,
) -> Result<(), String> {
    let expected = delivery_scope(route_id, control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker control delivery at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed worker control delivery is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed worker control delivery is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
