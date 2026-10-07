//! Durable worker-side execution intent for acknowledged Continue controls.
//!
//! A Continue control already consumed finite continuation authority when it was
//! admitted. This audit does not mint or consume another permit. It records that
//! the destination worker explicitly began one continuation execution and gives
//! that execution its own non-authored LocalTurnId for later transport evidence.
//!
//! Starting execution is not a WorkerLifecycle transition and is not evidence
//! that remote inference completed.

use crate::EventEnvelope;
use crate::EventStore;
use crate::authored::local_turn_scope;
use crate::continuation_audit::replay_continuation_audit;
use crate::control_audit::replay_control_audit;
use crate::control_inbox::replay_worker_control_inbox_for_conversation;
use crate::worker_audit::replay_worker_audit;
use chatarium_core::control::{ControlId, WorkerControlKind};
use chatarium_core::orchestration::{ContinuationLeaseId, WorkerGoalId, WorkerId};
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SCHEMA: &str = "chatarium-worker-continuation-execution-audit";
const VERSION: u64 = 1;

/// Durable remote-transport evidence for one non-authored continuation turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContinuationExecutionTransportState {
    pub dispatch_sequence: Option<u64>,
    pub acceptance_sequence: Option<u64>,
    pub stream_started_sequence: Option<u64>,
    pub latest_output_sequence: Option<u64>,
    pub completion_sequence: Option<u64>,
    pub failure_sequence: Option<u64>,
    pub interruption_sequence: Option<u64>,
}

impl ContinuationExecutionTransportState {
    #[must_use]
    pub const fn was_dispatched(self) -> bool {
        self.dispatch_sequence.is_some()
    }

    #[must_use]
    pub const fn is_completed(self) -> bool {
        self.completion_sequence.is_some()
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        self.completion_sequence.is_some()
            || self.failure_sequence.is_some()
            || self.interruption_sequence.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationExecutionOutcome {
    Completed,
    FailedObserved,
    Interrupted,
}

impl ContinuationExecutionOutcome {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::FailedObserved => "failed_observed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// One explicit worker-side bounded Continue execution and its terminal result, if observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerContinuationExecutionRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub goal_id: WorkerGoalId,
    pub lease_id: ContinuationLeaseId,
    pub permit_ordinal: u32,
    pub acknowledged_sequence: u64,
    pub execution_turn_id: LocalTurnId,
    pub started_sequence: u64,
    pub outcome: Option<ContinuationExecutionOutcome>,
    pub terminal_sequence: Option<u64>,
    pub result_sequence: Option<u64>,
}

/// Append one worker-side continuation execution start.
///
/// The caller supplies a fresh LocalTurnId. The turn identity is execution
/// provenance only; this function does not append a user-authored message and
/// does not dispatch remote inference.
pub fn record_worker_continuation_execution_started(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    worker_id: WorkerId,
    worker_conversation_id: LocalConversationId,
    goal_id: WorkerGoalId,
    lease_id: ContinuationLeaseId,
    permit_ordinal: u32,
    acknowledged_sequence: u64,
    execution_turn_id: LocalTurnId,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_continuation_execution_started",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "worker_id": worker_id.get(),
        "worker_conversation_id": worker_conversation_id.to_string(),
        "goal_id": goal_id.get(),
        "lease_id": lease_id.get(),
        "permit_ordinal": permit_ordinal,
        "acknowledged_sequence": acknowledged_sequence,
        "execution_turn_id": execution_turn_id.to_string(),
    });
    append_typed(
        store,
        execution_scope(worker_conversation_id, route_id, control_id),
        EventKind::WorkerContinuationExecutionStarted,
        payload,
    )
}

/// Append one terminal continuation-execution result derived from durable remote evidence.
pub fn record_worker_continuation_execution_result(
    store: &mut impl EventStore,
    record: WorkerContinuationExecutionRecord,
    outcome: ContinuationExecutionOutcome,
    terminal_sequence: u64,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_continuation_execution_result",
        "control_id": record.control_id.get(),
        "route_id": record.route_id.get(),
        "worker_id": record.worker_id.get(),
        "worker_conversation_id": record.worker_conversation_id.to_string(),
        "goal_id": record.goal_id.get(),
        "lease_id": record.lease_id.get(),
        "permit_ordinal": record.permit_ordinal,
        "execution_turn_id": record.execution_turn_id.to_string(),
        "execution_started_sequence": record.started_sequence,
        "outcome": outcome.stable_name(),
        "terminal_sequence": terminal_sequence,
    });
    append_typed(
        store,
        execution_scope(
            record.worker_conversation_id,
            record.route_id,
            record.control_id,
        ),
        EventKind::WorkerContinuationExecutionResultRecorded,
        payload,
    )
}

/// Replay all explicit worker-side continuation executions.
///
/// Replay fails closed unless the exact delivered/acknowledged Continue control,
/// consumed permit, current worker/goal, and continuation-eligible worker phase
/// all agree at the point execution began.
pub fn replay_worker_continuation_execution_audit(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerContinuationExecutionRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, WorkerContinuationExecutionRecord>::new();
    let mut control_owner = BTreeMap::<ControlId, RouteId>::new();
    let mut execution_turns = BTreeSet::<LocalTurnId>::new();

    for (index, event) in events.iter().enumerate() {
        match event.kind {
            EventKind::WorkerContinuationExecutionStarted => {
                let value = typed_payload(event, "worker_continuation_execution_started")?;
                let control_id = ControlId::new(required_u64(&value, "control_id")?);
                let route_id = RouteId::new(required_u64(&value, "route_id")?);
                let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
                let worker_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
                let lease_id = ContinuationLeaseId::new(required_u64(&value, "lease_id")?);
                let permit_ordinal = required_u32(&value, "permit_ordinal")?;
                let acknowledged_sequence = required_u64(&value, "acknowledged_sequence")?;
                let execution_turn_id = parse_turn_id(&value, event.sequence)?;
                validate_scope(event, worker_conversation_id, route_id, control_id)?;

                if by_route.contains_key(&route_id) {
                    return Err(format!(
                        "worker Continue route {} already has execution start before sequence {}",
                        route_id.get(),
                        event.sequence
                    ));
                }
                if let Some(existing_route) = control_owner.get(&control_id) {
                    return Err(format!(
                        "worker Continue control {} already executes on route {}; cannot also execute route {} at sequence {}",
                        control_id.get(),
                        existing_route.get(),
                        route_id.get(),
                        event.sequence
                    ));
                }
                if !execution_turns.insert(execution_turn_id) {
                    return Err(format!(
                        "worker continuation execution turn {} is reused at sequence {}",
                        execution_turn_id, event.sequence
                    ));
                }

                let prior = &events[..index];
                let item = replay_worker_control_inbox_for_conversation(
                    prior,
                    worker_conversation_id,
                )?
                .into_iter()
                .find(|item| item.route_id == route_id)
                .ok_or_else(|| {
                    format!(
                        "worker continuation execution at sequence {} references route {} before delivery",
                        event.sequence,
                        route_id.get()
                    )
                })?;
                if item.control_id != control_id
                    || item.worker_id != worker_id
                    || item.goal_id != goal_id
                {
                    return Err(format!(
                        "worker continuation execution at sequence {} disagrees with delivered control provenance",
                        event.sequence
                    ));
                }
                let WorkerControlKind::Continue {
                    permit_ordinal: inbox_ordinal,
                } = item.kind
                else {
                    return Err(format!(
                        "worker continuation execution at sequence {} references non-Continue control {}",
                        event.sequence,
                        control_id.get()
                    ));
                };
                if inbox_ordinal != permit_ordinal {
                    return Err(format!(
                        "worker continuation execution at sequence {} permit ordinal disagrees with delivered Continue control",
                        event.sequence
                    ));
                }

                let inbox_ack = item.acknowledged_sequence.ok_or_else(|| {
                    format!(
                        "worker continuation execution at sequence {} references unacknowledged route {}",
                        event.sequence,
                        route_id.get()
                    )
                })?;
                if inbox_ack != acknowledged_sequence || acknowledged_sequence >= event.sequence {
                    return Err(format!(
                        "worker continuation execution at sequence {} has invalid acknowledgement provenance",
                        event.sequence
                    ));
                }

                let control = replay_control_audit(prior)?
                    .into_iter()
                    .find(|record| record.control_id == control_id)
                    .ok_or_else(|| {
                        format!(
                            "worker continuation execution at sequence {} references missing control {}",
                            event.sequence,
                            control_id.get()
                        )
                    })?;
                let permit_ref = control.continuation_permit.ok_or_else(|| {
                    format!(
                        "worker continuation execution at sequence {} references Continue control {} without permit provenance",
                        event.sequence,
                        control_id.get()
                    )
                })?;
                if permit_ref.lease_id != lease_id || permit_ref.ordinal != permit_ordinal {
                    return Err(format!(
                        "worker continuation execution at sequence {} lease/permit identity disagrees with admitted control",
                        event.sequence
                    ));
                }

                let lease = replay_continuation_audit(prior)?
                    .into_iter()
                    .find(|record| record.lease_id == lease_id)
                    .ok_or_else(|| {
                        format!(
                            "worker continuation execution at sequence {} references missing continuation lease {}",
                            event.sequence,
                            lease_id.get()
                        )
                    })?;
                if lease.worker_id != worker_id || lease.goal_id != goal_id {
                    return Err(format!(
                        "worker continuation execution at sequence {} lease worker/goal provenance disagrees",
                        event.sequence
                    ));
                }
                let permit = lease
                    .permits
                    .iter()
                    .find(|permit| permit.ordinal == permit_ordinal)
                    .ok_or_else(|| {
                        format!(
                            "worker continuation execution at sequence {} references missing permit {}:{}",
                            event.sequence,
                            lease_id.get(),
                            permit_ordinal
                        )
                    })?;
                if permit.consumed_by != Some(control_id) {
                    return Err(format!(
                        "worker continuation execution at sequence {} permit {}:{} is not consumed by control {}",
                        event.sequence,
                        lease_id.get(),
                        permit_ordinal,
                        control_id.get()
                    ));
                }
                if permit
                    .consumed_sequence
                    .is_none_or(|sequence| sequence >= event.sequence)
                {
                    return Err(format!(
                        "worker continuation execution at sequence {} does not follow durable permit consumption",
                        event.sequence
                    ));
                }

                let worker = replay_worker_audit(prior)?
                    .into_iter()
                    .find(|record| record.worker_id == worker_id)
                    .ok_or_else(|| {
                        format!(
                            "worker continuation execution at sequence {} references worker {} without lifecycle state",
                            event.sequence,
                            worker_id.get()
                        )
                    })?;
                if worker.lifecycle.goal_id() != Some(goal_id) {
                    return Err(format!(
                        "worker continuation execution at sequence {} targets stale goal {}",
                        event.sequence,
                        goal_id.get()
                    ));
                }
                if !worker.lifecycle.phase().allows_continuation() {
                    return Err(format!(
                        "worker continuation execution at sequence {} cannot start while worker {} is {:?}",
                        event.sequence,
                        worker_id.get(),
                        worker.lifecycle.phase()
                    ));
                }

                let record = WorkerContinuationExecutionRecord {
                    control_id,
                    route_id,
                    worker_id,
                    worker_conversation_id,
                    goal_id,
                    lease_id,
                    permit_ordinal,
                    acknowledged_sequence,
                    execution_turn_id,
                    started_sequence: event.sequence,
                    outcome: None,
                    terminal_sequence: None,
                    result_sequence: None,
                };
                by_route.insert(route_id, record);
                control_owner.insert(control_id, route_id);
            }
            EventKind::WorkerContinuationExecutionResultRecorded => {
                let value = typed_payload(event, "worker_continuation_execution_result")?;
                let control_id = ControlId::new(required_u64(&value, "control_id")?);
                let route_id = RouteId::new(required_u64(&value, "route_id")?);
                let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
                let worker_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
                let lease_id = ContinuationLeaseId::new(required_u64(&value, "lease_id")?);
                let permit_ordinal = required_u32(&value, "permit_ordinal")?;
                let execution_turn_id = parse_turn_id(&value, event.sequence)?;
                let execution_started_sequence =
                    required_u64(&value, "execution_started_sequence")?;
                let outcome = parse_outcome(required_string(&value, "outcome")?)?;
                let terminal_sequence = required_u64(&value, "terminal_sequence")?;
                validate_scope(event, worker_conversation_id, route_id, control_id)?;

                let record = by_route.get_mut(&route_id).ok_or_else(|| {
                    format!(
                        "worker continuation result at sequence {} references route {} before execution start",
                        event.sequence,
                        route_id.get()
                    )
                })?;
                if record.result_sequence.is_some() {
                    return Err(format!(
                        "worker continuation route {} already has a terminal result",
                        route_id.get()
                    ));
                }
                if record.control_id != control_id
                    || record.worker_id != worker_id
                    || record.worker_conversation_id != worker_conversation_id
                    || record.goal_id != goal_id
                    || record.lease_id != lease_id
                    || record.permit_ordinal != permit_ordinal
                    || record.execution_turn_id != execution_turn_id
                    || record.started_sequence != execution_started_sequence
                {
                    return Err(format!(
                        "worker continuation result at sequence {} disagrees with execution-start provenance",
                        event.sequence
                    ));
                }
                if terminal_sequence >= event.sequence {
                    return Err(format!(
                        "worker continuation result at sequence {} does not follow terminal transport event {}",
                        event.sequence, terminal_sequence
                    ));
                }

                let transport =
                    continuation_execution_transport_state(&events[..index], execution_turn_id)?;
                let (expected_outcome, expected_terminal) =
                    transport_terminal_outcome(transport).ok_or_else(|| {
                        format!(
                            "worker continuation result at sequence {} has no prior terminal transport evidence",
                            event.sequence
                        )
                    })?;
                if outcome != expected_outcome || terminal_sequence != expected_terminal {
                    return Err(format!(
                        "worker continuation result at sequence {} disagrees with terminal transport evidence",
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

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.started_sequence);
    for record in &records {
        continuation_execution_transport_state(events, record.execution_turn_id)?;
    }
    Ok(records)
}

/// Replay remote transport evidence scoped to one non-authored continuation turn.
///
/// The execution turn may carry the same durable remote observation kinds as an
/// authored turn, but it must never contain UserMessageCommitted.
pub fn continuation_execution_transport_state(
    events: &[EventEnvelope],
    execution_turn_id: LocalTurnId,
) -> Result<ContinuationExecutionTransportState, String> {
    let scope = local_turn_scope(execution_turn_id);
    let mut state = ContinuationExecutionTransportState::default();

    for event in events
        .iter()
        .filter(|event| event.scope.as_deref() == Some(scope.as_str()))
    {
        if event.kind == EventKind::UserMessageCommitted {
            return Err(format!(
                "non-authored continuation turn {} contains a user-message commit at sequence {}",
                execution_turn_id, event.sequence
            ));
        }

        match event.kind {
            EventKind::DispatchAttempted => {
                if state.dispatch_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "continuation turn {} has multiple dispatch attempts",
                        execution_turn_id
                    ));
                }
            }
            EventKind::RemoteAcceptanceObserved => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                if state.acceptance_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "continuation turn {} has multiple remote-acceptance observations",
                        execution_turn_id
                    ));
                }
            }
            EventKind::AssistantStreamStarted => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                if state
                    .stream_started_sequence
                    .replace(event.sequence)
                    .is_some()
                {
                    return Err(format!(
                        "continuation turn {} has multiple assistant-stream starts",
                        execution_turn_id
                    ));
                }
            }
            EventKind::AssistantDeltaObserved | EventKind::AssistantSnapshotObserved => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                state.latest_output_sequence = Some(event.sequence);
            }
            EventKind::AssistantCompletionObserved => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                if state.completion_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "continuation turn {} has multiple completion observations",
                        execution_turn_id
                    ));
                }
                if state.failure_sequence.is_some() || state.interruption_sequence.is_some() {
                    return Err(format!(
                        "continuation turn {} records completion after failure/interruption",
                        execution_turn_id
                    ));
                }
                state.latest_output_sequence = Some(event.sequence);
            }
            EventKind::RemoteFailureObserved => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                if state.failure_sequence.replace(event.sequence).is_some() {
                    return Err(format!(
                        "continuation turn {} has multiple definitive failure observations",
                        execution_turn_id
                    ));
                }
                if state.completion_sequence.is_some() {
                    return Err(format!(
                        "continuation turn {} records remote failure after completion",
                        execution_turn_id
                    ));
                }
            }
            EventKind::TransportInterrupted => {
                require_dispatch(state, execution_turn_id, event.sequence)?;
                if state
                    .interruption_sequence
                    .replace(event.sequence)
                    .is_some()
                {
                    return Err(format!(
                        "continuation turn {} has multiple interruption observations",
                        execution_turn_id
                    ));
                }
                if state.completion_sequence.is_some() {
                    return Err(format!(
                        "continuation turn {} records interruption after completion",
                        execution_turn_id
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(state)
}

/// Reconstruct the latest durable continuation assistant output.
///
/// Continuation transport stores cumulative assistant snapshots and a terminal
/// completion snapshot in the same turn-scoped response-observation envelope as
/// ordinary inference. This projection intentionally ignores authored
/// transcript rows and reads only the non-authored continuation turn scope.
pub fn continuation_execution_output_text(
    events: &[EventEnvelope],
    execution_turn_id: LocalTurnId,
) -> Result<Option<String>, String> {
    continuation_execution_transport_state(events, execution_turn_id)?;

    let scope = local_turn_scope(execution_turn_id);
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
                "continuation turn {} has malformed assistant output payload at sequence {}: {error}",
                execution_turn_id, event.sequence
            )
        })?;
        match value.get("text") {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) => {
                latest = Some(text.clone());
            }
            Some(_) => {
                return Err(format!(
                    "continuation turn {} assistant output at sequence {} has non-string text",
                    execution_turn_id, event.sequence
                ));
            }
        }
    }

    Ok(latest.filter(|text| !text.trim().is_empty()))
}

pub fn transport_terminal_outcome(
    state: ContinuationExecutionTransportState,
) -> Option<(ContinuationExecutionOutcome, u64)> {
    if let Some(sequence) = state.completion_sequence {
        Some((ContinuationExecutionOutcome::Completed, sequence))
    } else if let Some(sequence) = state.failure_sequence {
        Some((ContinuationExecutionOutcome::FailedObserved, sequence))
    } else {
        state
            .interruption_sequence
            .map(|sequence| (ContinuationExecutionOutcome::Interrupted, sequence))
    }
}

fn require_dispatch(
    state: ContinuationExecutionTransportState,
    execution_turn_id: LocalTurnId,
    sequence: u64,
) -> Result<(), String> {
    if state.dispatch_sequence.is_none() {
        return Err(format!(
            "continuation turn {} has remote evidence at sequence {} before dispatch",
            execution_turn_id, sequence
        ));
    }
    Ok(())
}

#[must_use]
pub fn execution_scope(
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
    control_id: ControlId,
) -> String {
    format!(
        "worker-continuation-execution:{worker_conversation_id}:{}:{}",
        route_id.get(),
        control_id.get()
    )
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
            "malformed worker continuation execution payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "worker continuation execution at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "worker continuation execution at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "worker continuation execution at sequence {} has record {:?}, expected {:?}",
            event.sequence,
            value.get("record").and_then(Value::as_str),
            expected_record
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
    let expected = execution_scope(worker_conversation_id, route_id, control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker continuation execution at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn parse_conversation_id(value: &Value, sequence: u64) -> Result<LocalConversationId, String> {
    LocalConversationId::from_str(required_string(value, "worker_conversation_id")?).map_err(
        |error| {
            format!(
                "worker continuation execution at sequence {sequence} has invalid worker conversation id: {error}"
            )
        },
    )
}

fn parse_outcome(value: &str) -> Result<ContinuationExecutionOutcome, String> {
    match value {
        "completed" => Ok(ContinuationExecutionOutcome::Completed),
        "failed_observed" => Ok(ContinuationExecutionOutcome::FailedObserved),
        "interrupted" => Ok(ContinuationExecutionOutcome::Interrupted),
        _ => Err(format!(
            "unsupported worker continuation execution outcome '{value}'"
        )),
    }
}

fn parse_turn_id(value: &Value, sequence: u64) -> Result<LocalTurnId, String> {
    LocalTurnId::from_str(required_string(value, "execution_turn_id")?).map_err(|error| {
        format!(
            "worker continuation execution at sequence {sequence} has invalid execution turn id: {error}"
        )
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed worker continuation execution is missing integer field '{field}'")
    })
}

fn required_u32(value: &Value, field: &str) -> Result<u32, String> {
    let value = required_u64(value, field)?;
    u32::try_from(value)
        .map_err(|_| format!("worker continuation execution field '{field}' exceeds u32"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed worker continuation execution is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}
