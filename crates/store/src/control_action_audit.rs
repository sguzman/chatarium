//! Durable worker-side application audit for acknowledged mutating controls.
//!
//! Control admission, dispatch, delivery, acknowledgement, worker action intent,
//! lifecycle mutation, and action result remain separate durable facts.

use crate::control_inbox::replay_worker_control_inbox_for_conversation;
use crate::worker_audit::{
    decode_worker_transition_event, replay_worker_audit, WorkerControlTransitionProvenance,
};
use crate::{EventEnvelope, EventStore};
use chatarium_core::control::{validate_control_admission, ControlId, WorkerControlKind};
use chatarium_core::orchestration::{WorkerAction, WorkerGoalId, WorkerId, WorkerPhase};
use chatarium_core::routing::RouteId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SCHEMA: &str = "chatarium-worker-control-action-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerControlActionRecord {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub goal_id: WorkerGoalId,
    pub kind: WorkerControlKind,
    pub acknowledged_sequence: u64,
    pub from_phase: WorkerPhase,
    pub started_sequence: u64,
    pub lifecycle_sequence: Option<u64>,
    pub resulting_phase: Option<WorkerPhase>,
    pub result_sequence: Option<u64>,
}

impl WorkerControlActionRecord {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.result_sequence.is_some()
    }
}

pub fn record_worker_control_action_started(
    store: &mut impl EventStore,
    control_id: ControlId,
    route_id: RouteId,
    worker_id: WorkerId,
    worker_conversation_id: LocalConversationId,
    goal_id: WorkerGoalId,
    kind: WorkerControlKind,
    acknowledged_sequence: u64,
    from_phase: WorkerPhase,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_control_action_started",
        "control_id": control_id.get(),
        "route_id": route_id.get(),
        "worker_id": worker_id.get(),
        "worker_conversation_id": worker_conversation_id.to_string(),
        "goal_id": goal_id.get(),
        "kind": mutating_kind_name(kind).map_err(invalid_data)?,
        "acknowledged_sequence": acknowledged_sequence,
        "from_phase": phase_name(from_phase),
    });
    append_typed(
        store,
        action_scope(worker_conversation_id, route_id, control_id),
        EventKind::WorkerControlActionStarted,
        payload,
    )
}

pub fn record_worker_control_action_result(
    store: &mut impl EventStore,
    record: WorkerControlActionRecord,
    lifecycle_sequence: u64,
    resulting_phase: WorkerPhase,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "worker_control_action_result",
        "control_id": record.control_id.get(),
        "route_id": record.route_id.get(),
        "worker_id": record.worker_id.get(),
        "worker_conversation_id": record.worker_conversation_id.to_string(),
        "goal_id": record.goal_id.get(),
        "kind": mutating_kind_name(record.kind).map_err(invalid_data)?,
        "action_started_sequence": record.started_sequence,
        "lifecycle_sequence": lifecycle_sequence,
        "resulting_phase": phase_name(resulting_phase),
    });
    append_typed(
        store,
        action_scope(
            record.worker_conversation_id,
            record.route_id,
            record.control_id,
        ),
        EventKind::WorkerControlActionResultRecorded,
        payload,
    )
}

pub fn replay_worker_control_action_audit(
    events: &[EventEnvelope],
) -> Result<Vec<WorkerControlActionRecord>, String> {
    let mut records = BTreeMap::<RouteId, WorkerControlActionRecord>::new();
    let mut control_owner = BTreeMap::<ControlId, RouteId>::new();
    let mut transition_owner = BTreeSet::<u64>::new();

    for (index, event) in events.iter().enumerate() {
        match event.kind {
            EventKind::WorkerControlActionStarted => {
                let value = typed_payload(event, "worker_control_action_started")?;
                let control_id = ControlId::new(required_u64(&value, "control_id")?);
                let route_id = RouteId::new(required_u64(&value, "route_id")?);
                let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
                let worker_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
                let kind = parse_mutating_kind(required_string(&value, "kind")?)?;
                let acknowledged_sequence = required_u64(&value, "acknowledged_sequence")?;
                let from_phase = parse_phase(required_string(&value, "from_phase")?)?;
                validate_scope(
                    event,
                    worker_conversation_id,
                    route_id,
                    control_id,
                )?;

                if records.contains_key(&route_id) {
                    return Err(format!(
                        "worker control route {} already has an action start before sequence {}",
                        route_id.get(),
                        event.sequence
                    ));
                }
                if let Some(existing_route) = control_owner.get(&control_id) {
                    return Err(format!(
                        "worker control {} already has an action on route {}; cannot also start route {} at sequence {}",
                        control_id.get(),
                        existing_route.get(),
                        route_id.get(),
                        event.sequence
                    ));
                }

                let prior = &events[..index];
                let item =
                    replay_worker_control_inbox_for_conversation(prior, worker_conversation_id)?
                        .into_iter()
                        .find(|item| item.route_id == route_id)
                        .ok_or_else(|| {
                            format!(
                                "worker control action at sequence {} references route {} before delivery",
                                event.sequence,
                                route_id.get()
                            )
                        })?;
                if item.control_id != control_id
                    || item.worker_id != worker_id
                    || item.goal_id != goal_id
                    || item.kind != kind
                {
                    return Err(format!(
                        "worker control action at sequence {} disagrees with delivered control provenance",
                        event.sequence
                    ));
                }
                let inbox_ack = item.acknowledged_sequence.ok_or_else(|| {
                    format!(
                        "worker control action at sequence {} references unacknowledged route {}",
                        event.sequence,
                        route_id.get()
                    )
                })?;
                if inbox_ack != acknowledged_sequence {
                    return Err(format!(
                        "worker control action at sequence {} acknowledgement sequence disagrees with inbox",
                        event.sequence
                    ));
                }
                if acknowledged_sequence >= event.sequence {
                    return Err(format!(
                        "worker control action at sequence {} does not follow acknowledgement sequence {}",
                        event.sequence,
                        acknowledged_sequence
                    ));
                }

                let worker = replay_worker_audit(prior)?
                    .into_iter()
                    .find(|record| record.worker_id == worker_id)
                    .ok_or_else(|| {
                        format!(
                            "worker control action at sequence {} references worker {} without lifecycle state",
                            event.sequence,
                            worker_id.get()
                        )
                    })?;
                let replayed_from = validate_control_admission(goal_id, kind, &worker.lifecycle)
                    .map_err(|error| {
                        format!(
                            "worker control action at sequence {} is stale/invalid: {error}",
                            event.sequence
                        )
                    })?;
                if replayed_from != from_phase {
                    return Err(format!(
                        "worker control action at sequence {} claims source phase {}, replay observed {}",
                        event.sequence,
                        phase_name(from_phase),
                        phase_name(replayed_from)
                    ));
                }

                records.insert(
                    route_id,
                    WorkerControlActionRecord {
                        control_id,
                        route_id,
                        worker_id,
                        worker_conversation_id,
                        goal_id,
                        kind,
                        acknowledged_sequence,
                        from_phase,
                        started_sequence: event.sequence,
                        lifecycle_sequence: None,
                        resulting_phase: None,
                        result_sequence: None,
                    },
                );
                control_owner.insert(control_id, route_id);
            }
            EventKind::WorkerLifecycleTransitionRecorded => {
                let Some(decoded) = decode_worker_transition_event(event)? else {
                    continue;
                };
                let Some(provenance) = decoded.control_provenance else {
                    continue;
                };
                let record = records.get_mut(&provenance.route_id).ok_or_else(|| {
                    format!(
                        "control-correlated worker transition at sequence {} references route {} before action start",
                        event.sequence,
                        provenance.route_id.get()
                    )
                })?;
                if record.control_id != provenance.control_id
                    || record.started_sequence != provenance.action_started_sequence
                    || record.worker_id != decoded.worker_id
                    || record.goal_id != decoded.goal_id
                    || decoded.action != action_for_kind(record.kind)?
                {
                    return Err(format!(
                        "control-correlated worker transition at sequence {} disagrees with action start provenance",
                        event.sequence
                    ));
                }
                if record.lifecycle_sequence.is_some() {
                    return Err(format!(
                        "worker control route {} has multiple correlated lifecycle transitions",
                        record.route_id.get()
                    ));
                }
                if event.sequence <= record.started_sequence {
                    return Err(format!(
                        "worker control route {} lifecycle transition does not follow action start",
                        record.route_id.get()
                    ));
                }
                if !transition_owner.insert(event.sequence) {
                    return Err(format!(
                        "worker lifecycle transition {} is correlated more than once",
                        event.sequence
                    ));
                }

                let resulting_phase = replay_worker_audit(&events[..=index])?
                    .into_iter()
                    .find(|worker| worker.worker_id == record.worker_id)
                    .ok_or_else(|| {
                        format!(
                            "worker {} disappeared after correlated lifecycle transition",
                            record.worker_id.get()
                        )
                    })?
                    .lifecycle
                    .phase();
                let expected = resulting_phase_for_kind(record.kind)?;
                if resulting_phase != expected {
                    return Err(format!(
                        "worker control route {} transition resulted in {}, expected {}",
                        record.route_id.get(),
                        phase_name(resulting_phase),
                        phase_name(expected)
                    ));
                }

                record.lifecycle_sequence = Some(event.sequence);
                record.resulting_phase = Some(resulting_phase);
            }
            EventKind::WorkerControlActionResultRecorded => {
                let value = typed_payload(event, "worker_control_action_result")?;
                let control_id = ControlId::new(required_u64(&value, "control_id")?);
                let route_id = RouteId::new(required_u64(&value, "route_id")?);
                let worker_id = WorkerId::new(required_u64(&value, "worker_id")?);
                let worker_conversation_id = parse_conversation_id(&value, event.sequence)?;
                let goal_id = WorkerGoalId::new(required_u64(&value, "goal_id")?);
                let kind = parse_mutating_kind(required_string(&value, "kind")?)?;
                let action_started_sequence = required_u64(&value, "action_started_sequence")?;
                let lifecycle_sequence = required_u64(&value, "lifecycle_sequence")?;
                let resulting_phase = parse_phase(required_string(&value, "resulting_phase")?)?;
                validate_scope(
                    event,
                    worker_conversation_id,
                    route_id,
                    control_id,
                )?;

                let record = records.get_mut(&route_id).ok_or_else(|| {
                    format!(
                        "worker control action result at sequence {} references route {} before action start",
                        event.sequence,
                        route_id.get()
                    )
                })?;
                if record.result_sequence.is_some() {
                    return Err(format!(
                        "worker control route {} already has an action result",
                        route_id.get()
                    ));
                }
                if record.control_id != control_id
                    || record.worker_id != worker_id
                    || record.worker_conversation_id != worker_conversation_id
                    || record.goal_id != goal_id
                    || record.kind != kind
                    || record.started_sequence != action_started_sequence
                    || record.lifecycle_sequence != Some(lifecycle_sequence)
                    || record.resulting_phase != Some(resulting_phase)
                {
                    return Err(format!(
                        "worker control action result at sequence {} disagrees with started/correlated action provenance",
                        event.sequence
                    ));
                }
                if lifecycle_sequence >= event.sequence {
                    return Err(format!(
                        "worker control action result at sequence {} does not follow lifecycle transition {}",
                        event.sequence,
                        lifecycle_sequence
                    ));
                }
                record.result_sequence = Some(event.sequence);
            }
            _ => {}
        }
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.started_sequence);
    Ok(values)
}

#[must_use]
pub fn action_scope(
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
    control_id: ControlId,
) -> String {
    format!(
        "worker-control-action:{worker_conversation_id}:{}:{}",
        route_id.get(),
        control_id.get()
    )
}

pub fn action_for_kind(kind: WorkerControlKind) -> Result<WorkerAction, String> {
    match kind {
        WorkerControlKind::StartOrResume => Ok(WorkerAction::StartOrResume),
        WorkerControlKind::Stop => Ok(WorkerAction::Stop),
        WorkerControlKind::Continue { .. } => {
            Err("Continue control application is not implemented in this slice".to_owned())
        }
        WorkerControlKind::StatusRequest => {
            Err("StatusRequest is non-mutating and must use the status-result path".to_owned())
        }
    }
}

pub fn resulting_phase_for_kind(kind: WorkerControlKind) -> Result<WorkerPhase, String> {
    match kind {
        WorkerControlKind::StartOrResume => Ok(WorkerPhase::Working),
        WorkerControlKind::Stop => Ok(WorkerPhase::Stopped),
        WorkerControlKind::Continue { .. } => {
            Err("Continue control application is not implemented in this slice".to_owned())
        }
        WorkerControlKind::StatusRequest => {
            Err("StatusRequest is non-mutating and has no lifecycle result phase".to_owned())
        }
    }
}

fn mutating_kind_name(kind: WorkerControlKind) -> Result<&'static str, String> {
    match kind {
        WorkerControlKind::StartOrResume => Ok("start_or_resume"),
        WorkerControlKind::Stop => Ok("stop"),
        WorkerControlKind::Continue { .. } => {
            Err("Continue control action audit is not implemented".to_owned())
        }
        WorkerControlKind::StatusRequest => Err("StatusRequest is not a mutating action".to_owned()),
    }
}

fn parse_mutating_kind(value: &str) -> Result<WorkerControlKind, String> {
    match value {
        "start_or_resume" => Ok(WorkerControlKind::StartOrResume),
        "stop" => Ok(WorkerControlKind::Stop),
        other => Err(format!("unsupported worker control action kind '{other}'")),
    }
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
        other => Err(format!("unknown worker phase '{other}'")),
    }
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed worker control action event at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "worker control action event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "worker control action event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "worker control action event at sequence {} has unexpected record",
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
    let expected = action_scope(worker_conversation_id, route_id, control_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "worker control action event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn parse_conversation_id(
    value: &Value,
    sequence: u64,
) -> Result<LocalConversationId, String> {
    LocalConversationId::from_str(required_string(value, "worker_conversation_id")?).map_err(
        |error| {
            format!(
                "worker control action event at sequence {sequence} has invalid conversation id: {error}"
            )
        },
    )
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed worker control action is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed worker control action is missing string field '{field}'"))
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

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_container_audit::record_chat_container_created;
    use crate::control_ack_audit::record_worker_control_acknowledged;
    use crate::control_audit::record_worker_control_admitted;
    use crate::control_delivery_audit::record_worker_control_delivered;
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::local_conversation_worker_audit::record_local_conversation_worker_bound;
    use crate::session_audit::{
        record_local_session_registered, record_worker_session_bound,
    };
    use crate::worker_audit::{
        record_worker_control_transition, record_worker_goal_assigned, record_worker_transition,
    };
    use crate::MemoryEventStore;
    use chatarium_core::chat_container::ChatContainerId;
    use chatarium_core::session::{SessionId, WorkerSessionBinding};

    fn acknowledged_control(
        store: &mut impl EventStore,
        kind: WorkerControlKind,
        phase: WorkerPhase,
    ) -> (
        LocalConversationId,
        WorkerId,
        WorkerGoalId,
        ControlId,
        RouteId,
        u64,
    ) {
        let conversation = LocalConversationId::new();
        let worker = WorkerId::new(1);
        let goal = WorkerGoalId::new(1);
        let control = ControlId::new(1);
        let route = RouteId::new(1);
        let session = SessionId::new(1);

        record_local_session_registered(store, session).unwrap();
        record_chat_container_created(store, ChatContainerId::new(1), session).unwrap();
        record_local_conversation_chat_container_bound(
            store,
            conversation,
            ChatContainerId::new(1),
        )
        .unwrap();
        record_local_conversation_worker_bound(store, conversation, worker).unwrap();
        record_worker_session_bound(store, WorkerSessionBinding::new(worker, session)).unwrap();
        record_worker_goal_assigned(store, worker, goal).unwrap();
        match phase {
            WorkerPhase::Ready => {}
            WorkerPhase::Working => {
                record_worker_transition(store, worker, goal, WorkerAction::StartOrResume).unwrap();
            }
            WorkerPhase::NeedsInput => {
                record_worker_transition(store, worker, goal, WorkerAction::StartOrResume).unwrap();
                record_worker_transition(store, worker, goal, WorkerAction::RequestInput).unwrap();
            }
            WorkerPhase::Blocked => {
                record_worker_transition(store, worker, goal, WorkerAction::StartOrResume).unwrap();
                record_worker_transition(store, worker, goal, WorkerAction::MarkBlocked).unwrap();
            }
            _ => panic!("test helper requires nonterminal phase"),
        }

        let lifecycle = replay_worker_audit(store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.worker_id == worker)
            .unwrap()
            .lifecycle;
        let admitted = match kind {
            WorkerControlKind::StartOrResume => {
                chatarium_core::control::WorkerControl::start_or_resume(
                    control, worker, goal, &lifecycle,
                )
                .unwrap()
            }
            WorkerControlKind::Stop => {
                chatarium_core::control::WorkerControl::stop(control, worker, goal, &lifecycle)
                    .unwrap()
            }
            _ => panic!("test helper only supports mutating controls"),
        };
        record_worker_control_admitted(store, &admitted).unwrap();

        let dispatch_sequence = store.events().last().unwrap().sequence + 1;
        store
            .append(
                EventKind::RouteDispatched,
                serde_json::json!({"placeholder": true}).to_string(),
            )
            .unwrap();
        record_worker_control_delivered(
            store,
            control,
            route,
            worker,
            conversation,
            session,
            SessionId::new(99),
            dispatch_sequence,
        )
        .unwrap();
        let delivered_sequence = store.events().last().unwrap().sequence;
        record_worker_control_acknowledged(
            store,
            control,
            route,
            worker,
            conversation,
            delivered_sequence,
        )
        .unwrap();
        let acknowledged_sequence = store.events().last().unwrap().sequence;

        (
            conversation,
            worker,
            goal,
            control,
            route,
            acknowledged_sequence,
        )
    }

    #[test]
    fn start_transition_and_result_replay_as_one_correlated_action() {
        let mut store = MemoryEventStore::default();
        let (conversation, worker, goal, control, route, ack) = acknowledged_control(
            &mut store,
            WorkerControlKind::StartOrResume,
            WorkerPhase::Ready,
        );

        let started = record_worker_control_action_started(
            &mut store,
            control,
            route,
            worker,
            conversation,
            goal,
            WorkerControlKind::StartOrResume,
            ack,
            WorkerPhase::Ready,
        )
        .unwrap();
        let lifecycle_sequence = record_worker_control_transition(
            &mut store,
            worker,
            goal,
            WorkerAction::StartOrResume,
            WorkerControlTransitionProvenance {
                control_id: control,
                route_id: route,
                action_started_sequence: started,
            },
        )
        .unwrap();

        let incomplete = replay_worker_control_action_audit(store.events()).unwrap();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].lifecycle_sequence, Some(lifecycle_sequence));
        assert_eq!(incomplete[0].resulting_phase, Some(WorkerPhase::Working));
        assert_eq!(incomplete[0].result_sequence, None);

        record_worker_control_action_result(
            &mut store,
            incomplete[0],
            lifecycle_sequence,
            WorkerPhase::Working,
        )
        .unwrap();

        let complete = replay_worker_control_action_audit(store.events()).unwrap();
        assert!(complete[0].is_complete());
        assert_eq!(complete[0].resulting_phase, Some(WorkerPhase::Working));
    }

    #[test]
    fn ordinary_lifecycle_transition_does_not_complete_control_action() {
        let mut store = MemoryEventStore::default();
        let (conversation, worker, goal, control, route, ack) = acknowledged_control(
            &mut store,
            WorkerControlKind::StartOrResume,
            WorkerPhase::Ready,
        );
        record_worker_control_action_started(
            &mut store,
            control,
            route,
            worker,
            conversation,
            goal,
            WorkerControlKind::StartOrResume,
            ack,
            WorkerPhase::Ready,
        )
        .unwrap();
        record_worker_transition(&mut store, worker, goal, WorkerAction::StartOrResume).unwrap();

        let records = replay_worker_control_action_audit(store.events()).unwrap();
        assert_eq!(records[0].lifecycle_sequence, None);
        assert_eq!(records[0].result_sequence, None);
    }

    #[test]
    fn stop_action_requires_matching_correlated_transition() {
        let mut store = MemoryEventStore::default();
        let (conversation, worker, goal, control, route, ack) =
            acknowledged_control(&mut store, WorkerControlKind::Stop, WorkerPhase::Working);
        let started = record_worker_control_action_started(
            &mut store,
            control,
            route,
            worker,
            conversation,
            goal,
            WorkerControlKind::Stop,
            ack,
            WorkerPhase::Working,
        )
        .unwrap();
        record_worker_control_transition(
            &mut store,
            worker,
            goal,
            WorkerAction::Stop,
            WorkerControlTransitionProvenance {
                control_id: control,
                route_id: route,
                action_started_sequence: started,
            },
        )
        .unwrap();

        let record = replay_worker_control_action_audit(store.events()).unwrap()[0];
        assert_eq!(record.resulting_phase, Some(WorkerPhase::Stopped));
    }
}
