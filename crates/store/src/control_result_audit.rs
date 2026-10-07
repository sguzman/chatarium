//! Durable non-mutating result for acknowledged worker StatusRequest controls.
//!
//! Status results snapshot already-existing worker lifecycle state. They do not
//! mutate lifecycle, transcript, routing, or inference context.

use crate::control_inbox::replay_worker_control_inbox_for_conversation;
use crate::worker_audit::replay_worker_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::control::{ControlId, WorkerControlKind};
use chatarium_core::orchestration::{WorkerGoalId, WorkerId, WorkerPhase};
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-worker-control-result-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerControlStatusResultRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub goal_id: WorkerGoalId,
    pub phase: WorkerPhase,
    pub acknowledged_sequence: u64,
    pub recorded_sequence: u64,
}

pub fn record_worker_control_status_result(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    worker_id: WorkerId,
    worker_conversation_id: LocalConversationId,
    goal_id: WorkerGoalId,
    phase: WorkerPhase,
    acknowledged_sequence: u64,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_control_status_result",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "worker_id": worker_id.get(),
        "worker_conversation_id": worker_conversation_id.to_string(),
        "goal_id": goal_id.get(),
        "phase": phase_name(phase),
        "acknowledged_sequence": acknowledged_sequence,
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(result_scope(worker_conversation_id, route_id, control_id)),
        EventKind::WorkerControlStatusResultRecorded,
        encoded,
    )
}

pub fn replay_worker_control_status_results(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerControlStatusResultRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, WorkerControlStatusResultRecord>::new();
    let mut control_owner = BTreeMap::<ControlId, RouteId>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::WorkerControlStatusResultRecorded {
            continue;
        }

        let value = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&value, "control_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
        let worker_conversation_id = LocalConversationId::from_str(required_string(
            &value,
            "worker_conversation_id",
        )?)
        .map_err(|error| {
            format!(
                "worker control status result at sequence {} has invalid conversation id: {error}",
                event.sequence
            )
        })?;
        let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
        let phase = parse_phase(required_string(&value, "phase")?)?;
        let acknowledged_sequence = required_u64(&value, "acknowledged_sequence")?;
        validate_scope(event, worker_conversation_id, route_id, control_id)?;

        if let Some(existing) = by_route.get(&route_id) {
            return Err(format!(
                "worker control route {} already has status result at sequence {}; cannot record again at sequence {}",
                route_id.get(),
                existing.recorded_sequence,
                event.sequence
            ));
        }
        if let Some(existing_route) = control_owner.get(&control_id) {
            return Err(format!(
                "worker control {} already has status result on route {}; cannot also record route {} at sequence {}",
                control_id.get(),
                existing_route.get(),
                route_id.get(),
                event.sequence
            ));
        }

        let prior = &events[..index];
        let item = replay_worker_control_inbox_for_conversation(prior, worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control status result at sequence {} references route {} before delivery",
                    event.sequence,
                    route_id.get()
                )
            })?;

        if item.control_id != control_id
            || item.worker_id != worker_id
            || item.goal_id != goal_id
            || item.kind != WorkerControlKind::StatusRequest
        {
            return Err(format!(
                "worker control status result at sequence {} disagrees with delivered StatusRequest provenance",
                event.sequence
            ));
        }

        let inbox_ack = item.acknowledged_sequence.ok_or_else(|| {
            format!(
                "worker control status result at sequence {} references unacknowledged route {}",
                event.sequence,
                route_id.get()
            )
        })?;
        if inbox_ack != acknowledged_sequence {
            return Err(format!(
                "worker control status result at sequence {} acknowledgement sequence disagrees with inbox",
                event.sequence
            ));
        }

        let worker = replay_worker_audit(prior)?
            .into_iter()
            .find(|record| record.worker_id == worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control status result at sequence {} references worker {} without lifecycle state",
                    event.sequence,
                    worker_id.get()
                )
            })?;
        if worker.lifecycle.goal_id() != Some(goal_id) {
            return Err(format!(
                "worker control status result at sequence {} targets goal {}, but worker {} current goal is {:?}",
                event.sequence,
                goal_id.get(),
                worker_id.get(),
                worker.lifecycle.goal_id().map(WorkerGoalId::get)
            ));
        }
        if worker.lifecycle.phase() != phase {
            return Err(format!(
                "worker control status result at sequence {} claims phase {}, but replayed worker phase is {}",
                event.sequence,
                phase_name(phase),
                phase_name(worker.lifecycle.phase())
            ));
        }
        if acknowledged_sequence >= event.sequence {
            return Err(format!(
                "worker control status result at sequence {} does not follow acknowledgement sequence {}",
                event.sequence, acknowledged_sequence
            ));
        }

        let record = WorkerControlStatusResultRecord {
            control_id,
            route_id,
            worker_id,
            worker_conversation_id,
            goal_id,
            phase,
            acknowledged_sequence,
            recorded_sequence: event.sequence,
        };
        by_route.insert(route_id, record);
        control_owner.insert(control_id, route_id);
    }

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.recorded_sequence);
    Ok(records)
}

#[must_use]
pub fn result_scope(
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
    control_id: ControlId,
) -> String {
    format!(
        "worker-control-status-result:{worker_conversation_id}:{}:{}",
        route_id.get(),
        control_id.get()
    )
}

const fn phase_name(phase: WorkerPhase) -> &'static str {
    match phase {
        WorkerPhase::Unassigned => "unassigned",
        WorkerPhase::Ready => "ready",
        WorkerPhase::Working => "working",
        WorkerPhase::NeedsInput => "needs_input",
        WorkerPhase::Blocked => "blocked",
        WorkerPhase::Completed => "completed",
        WorkerPhase::Failed => "failed",
        WorkerPhase::Stopped => "stopped",
    }
}

fn parse_phase(value: &str) -> Result<WorkerPhase, String> {
    match value {
        "unassigned" => Ok(WorkerPhase::Unassigned),
        "ready" => Ok(WorkerPhase::Ready),
        "working" => Ok(WorkerPhase::Working),
        "needs_input" => Ok(WorkerPhase::NeedsInput),
        "blocked" => Ok(WorkerPhase::Blocked),
        "completed" => Ok(WorkerPhase::Completed),
        "failed" => Ok(WorkerPhase::Failed),
        "stopped" => Ok(WorkerPhase::Stopped),
        other => Err(format!("unknown worker status result phase '{other}'")),
    }
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed worker control status result at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "worker control status result at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "worker control status result at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("worker_control_status_result") {
        return Err(format!(
            "worker control status result at sequence {} has unexpected record",
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
    let expected = result_scope(worker_conversation_id, route_id, control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker control status result at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed worker control status result is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed worker control status result is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
