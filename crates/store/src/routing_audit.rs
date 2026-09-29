//! Typed durable audit records for supervisory routing.
//!
//! Routing audit authority lives in the append-only journal. This module persists
//! only route identity, policy/decision provenance, dispatch fact, and a generic
//! result/error observation. It intentionally does not persist arbitrary routed
//! message/tool payloads or define MCP/XML semantics.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::routing::{
    DecisionAuthority, DispatchPermit, RouteClass, RouteEndpointId, RouteGate, RouteGateError,
    RouteGateState, RouteId, RoutePolicy, RouteRequest,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const ROUTING_AUDIT_SCHEMA: &str = "chatarium-routing-audit";
const ROUTING_AUDIT_VERSION: u64 = 1;

/// Explicit user decision durably recorded for a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteUserDecision {
    /// User explicitly allowed the route.
    Allow,
    /// User explicitly denied the route.
    Deny,
}

/// Generic routing-layer observation recorded after dispatch.
///
/// These values do not imply worker-goal completion, assistant completion, remote
/// acceptance, or application/tool semantic success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteResultObservation {
    /// A generic result observation was produced by the future routing/adapter layer.
    ObservedResult,
    /// A generic error observation was produced by the future routing/adapter layer.
    ObservedError,
}

/// Restart-replayable audit state for one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteAuditRecord {
    /// Original typed route request.
    pub request: RouteRequest,
    /// Policy requirement when the route was proposed.
    pub initial_policy: RoutePolicy,
    /// Current replayed policy/dispatch state.
    pub gate_state: RouteGateState,
    /// Latest explicit user decision before dispatch, if any.
    pub latest_user_decision: Option<RouteUserDecision>,
    /// Durable sequence of the proposal.
    pub proposed_sequence: u64,
    /// Last routing event applied to this record.
    pub last_sequence: u64,
    /// Durable sequence that recorded one-shot dispatch, if any.
    pub dispatch_sequence: Option<u64>,
    /// Generic post-dispatch result/error observation, if any.
    pub result: Option<RouteResultObservation>,
    /// Durable sequence of the generic result/error observation, if any.
    pub result_sequence: Option<u64>,
}

/// Append one typed route proposal.
///
/// The JSONL store's durability boundary determines when this function returns
/// success.
pub fn record_route_proposed(
    store: &mut impl EventStore,
    request: RouteRequest,
    policy: RoutePolicy,
) -> std::io::Result<u64> {
    append_typed(
        store,
        request.id,
        EventKind::RouteProposed,
        json!({
            "schema": ROUTING_AUDIT_SCHEMA,
            "version": ROUTING_AUDIT_VERSION,
            "record": "proposal",
            "route_id": request.id.get(),
            "source": request.source.get(),
            "destination": request.destination.get(),
            "class": route_class_name(request.class),
            "policy": route_policy_name(policy),
        }),
    )
}

/// Append one explicit user allow/deny decision for a route.
pub fn record_route_user_decision(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> std::io::Result<u64> {
    append_typed(
        store,
        route_id,
        EventKind::RouteUserDecisionRecorded,
        json!({
            "schema": ROUTING_AUDIT_SCHEMA,
            "version": ROUTING_AUDIT_VERSION,
            "record": "user_decision",
            "route_id": route_id.get(),
            "decision": user_decision_name(decision),
        }),
    )
}

/// Append one consumed dispatch permit.
///
/// Taking a typed DispatchPermit ensures callers cannot use this helper to
/// manufacture a dispatch fact without first passing the core route gate.
pub fn record_route_dispatched(
    store: &mut impl EventStore,
    permit: DispatchPermit,
) -> std::io::Result<u64> {
    append_typed(
        store,
        permit.route_id(),
        EventKind::RouteDispatched,
        json!({
            "schema": ROUTING_AUDIT_SCHEMA,
            "version": ROUTING_AUDIT_VERSION,
            "record": "dispatch",
            "route_id": permit.route_id().get(),
            "authorized_by": decision_authority_name(permit.authorized_by()),
        }),
    )
}

/// Append one generic routing-layer result/error observation.
///
/// This is intentionally content-free and does not imply worker/task completion.
pub fn record_route_result(
    store: &mut impl EventStore,
    route_id: RouteId,
    observation: RouteResultObservation,
) -> std::io::Result<u64> {
    append_typed(
        store,
        route_id,
        EventKind::RouteResultObserved,
        json!({
            "schema": ROUTING_AUDIT_SCHEMA,
            "version": ROUTING_AUDIT_VERSION,
            "record": "result",
            "route_id": route_id.get(),
            "observation": result_observation_name(observation),
        }),
    )
}

/// Reconstruct all typed routing audit state from authoritative journal events.
///
/// Unrelated journal events are ignored. Routing history fails closed if its typed
/// payloads or transition order are inconsistent.
pub fn replay_routing_audit(events: &[EventEnvelope]) -> Result<Vec<RouteAuditRecord>, String> {
    let mut routes = BTreeMap::<RouteId, ReplayRoute>::new();

    for event in events {
        match event.kind {
            EventKind::RouteProposed => replay_proposal(&mut routes, event)?,
            EventKind::RouteUserDecisionRecorded => replay_user_decision(&mut routes, event)?,
            EventKind::RouteDispatched => replay_dispatch(&mut routes, event)?,
            EventKind::RouteResultObserved => replay_result(&mut routes, event)?,
            _ => {}
        }
    }

    let mut records = routes
        .into_values()
        .map(ReplayRoute::into_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.proposed_sequence);
    Ok(records)
}

struct ReplayRoute {
    request: RouteRequest,
    initial_policy: RoutePolicy,
    gate: RouteGate,
    latest_user_decision: Option<RouteUserDecision>,
    proposed_sequence: u64,
    last_sequence: u64,
    dispatch_sequence: Option<u64>,
    result: Option<RouteResultObservation>,
    result_sequence: Option<u64>,
}

impl ReplayRoute {
    fn into_record(self) -> RouteAuditRecord {
        RouteAuditRecord {
            request: self.request,
            initial_policy: self.initial_policy,
            gate_state: self.gate.state(),
            latest_user_decision: self.latest_user_decision,
            proposed_sequence: self.proposed_sequence,
            last_sequence: self.last_sequence,
            dispatch_sequence: self.dispatch_sequence,
            result: self.result,
            result_sequence: self.result_sequence,
        }
    }
}

fn replay_proposal(
    routes: &mut BTreeMap<RouteId, ReplayRoute>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "proposal")?;
    let route_id = route_id(&payload)?;
    validate_scope(event, route_id)?;

    if routes.contains_key(&route_id) {
        return Err(format!(
            "duplicate routing proposal for route {} at sequence {}",
            route_id.get(),
            event.sequence
        ));
    }

    let request = RouteRequest {
        id: route_id,
        source: RouteEndpointId::new(required_u64(&payload, "source")?),
        destination: RouteEndpointId::new(required_u64(&payload, "destination")?),
        class: parse_route_class(required_string(&payload, "class")?)?,
    };
    let policy = parse_route_policy(required_string(&payload, "policy")?)?;

    routes.insert(
        route_id,
        ReplayRoute {
            request,
            initial_policy: policy,
            gate: RouteGate::new(request, policy),
            latest_user_decision: None,
            proposed_sequence: event.sequence,
            last_sequence: event.sequence,
            dispatch_sequence: None,
            result: None,
            result_sequence: None,
        },
    );
    Ok(())
}

fn replay_user_decision(
    routes: &mut BTreeMap<RouteId, ReplayRoute>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "user_decision")?;
    let route_id = route_id(&payload)?;
    validate_scope(event, route_id)?;

    let decision = parse_user_decision(required_string(&payload, "decision")?)?;
    let route = routes.get_mut(&route_id).ok_or_else(|| {
        format!(
            "routing user decision for route {} at sequence {} appeared before proposal",
            route_id.get(),
            event.sequence
        )
    })?;

    match decision {
        RouteUserDecision::Allow => route.gate.user_allow(),
        RouteUserDecision::Deny => route.gate.user_deny(),
    }
    .map_err(|error| route_gate_error(route_id, event.sequence, error))?;

    route.latest_user_decision = Some(decision);
    route.last_sequence = event.sequence;
    Ok(())
}

fn replay_dispatch(
    routes: &mut BTreeMap<RouteId, ReplayRoute>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "dispatch")?;
    let route_id = route_id(&payload)?;
    validate_scope(event, route_id)?;

    let persisted_authority =
        parse_decision_authority(required_string(&payload, "authorized_by")?)?;
    let route = routes.get_mut(&route_id).ok_or_else(|| {
        format!(
            "routing dispatch for route {} at sequence {} appeared before proposal",
            route_id.get(),
            event.sequence
        )
    })?;

    if route.dispatch_sequence.is_some() {
        return Err(format!(
            "duplicate routing dispatch for route {} at sequence {}",
            route_id.get(),
            event.sequence
        ));
    }

    let permit = route
        .gate
        .authorize_dispatch(route_id)
        .map_err(|error| route_gate_error(route_id, event.sequence, error))?;

    if permit.authorized_by() != persisted_authority {
        return Err(format!(
            "routing dispatch authority mismatch for route {} at sequence {}: journal says {:?}, replay gate says {:?}",
            route_id.get(),
            event.sequence,
            persisted_authority,
            permit.authorized_by()
        ));
    }

    route.dispatch_sequence = Some(event.sequence);
    route.last_sequence = event.sequence;
    Ok(())
}

fn replay_result(
    routes: &mut BTreeMap<RouteId, ReplayRoute>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "result")?;
    let route_id = route_id(&payload)?;
    validate_scope(event, route_id)?;

    let observation = parse_result_observation(required_string(&payload, "observation")?)?;
    let route = routes.get_mut(&route_id).ok_or_else(|| {
        format!(
            "routing result for route {} at sequence {} appeared before proposal",
            route_id.get(),
            event.sequence
        )
    })?;

    if route.dispatch_sequence.is_none() || !route.gate.state().is_dispatched() {
        return Err(format!(
            "routing result for route {} at sequence {} appeared before dispatch",
            route_id.get(),
            event.sequence
        ));
    }
    if route.result.is_some() {
        return Err(format!(
            "duplicate routing result for route {} at sequence {}",
            route_id.get(),
            event.sequence
        ));
    }

    route.result = Some(observation);
    route.result_sequence = Some(event.sequence);
    route.last_sequence = event.sequence;
    Ok(())
}

fn append_typed(
    store: &mut impl EventStore,
    route_id: RouteId,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(route_scope(route_id)), kind, encoded)
}

/// Stable journal scope for one route.
#[must_use]
pub fn route_scope(route_id: RouteId) -> String {
    format!("route:{}", route_id.get())
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed routing payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(ROUTING_AUDIT_SCHEMA) {
        return Err(format!(
            "routing event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "routing event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != ROUTING_AUDIT_VERSION {
        return Err(format!(
            "unsupported routing audit payload version {version} at sequence {}",
            event.sequence
        ));
    }
    let record = required_string(&value, "record")?;
    if record != expected_record {
        return Err(format!(
            "routing event at sequence {} has record '{record}', expected '{expected_record}'",
            event.sequence
        ));
    }
    Ok(value)
}

fn route_id(value: &Value) -> Result<RouteId, String> {
    Ok(RouteId::new(required_u64(value, "route_id")?))
}

fn validate_scope(event: &EventEnvelope, route_id: RouteId) -> Result<(), String> {
    let expected = route_scope(route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "routing event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed routing payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed routing payload is missing string field '{field}'"))
}

const fn route_class_name(value: RouteClass) -> &'static str {
    match value {
        RouteClass::SessionMessage => "session_message",
        RouteClass::OrchestrationControl => "orchestration_control",
        RouteClass::ToolCall => "tool_call",
    }
}

fn parse_route_class(value: &str) -> Result<RouteClass, String> {
    match value {
        "session_message" => Ok(RouteClass::SessionMessage),
        "orchestration_control" => Ok(RouteClass::OrchestrationControl),
        "tool_call" => Ok(RouteClass::ToolCall),
        other => Err(format!("unknown routing class '{other}'")),
    }
}

const fn route_policy_name(value: RoutePolicy) -> &'static str {
    match value {
        RoutePolicy::Allow => "allow",
        RoutePolicy::Deny => "deny",
        RoutePolicy::RequireApproval => "require_approval",
    }
}

fn parse_route_policy(value: &str) -> Result<RoutePolicy, String> {
    match value {
        "allow" => Ok(RoutePolicy::Allow),
        "deny" => Ok(RoutePolicy::Deny),
        "require_approval" => Ok(RoutePolicy::RequireApproval),
        other => Err(format!("unknown routing policy '{other}'")),
    }
}

const fn user_decision_name(value: RouteUserDecision) -> &'static str {
    match value {
        RouteUserDecision::Allow => "allow",
        RouteUserDecision::Deny => "deny",
    }
}

fn parse_user_decision(value: &str) -> Result<RouteUserDecision, String> {
    match value {
        "allow" => Ok(RouteUserDecision::Allow),
        "deny" => Ok(RouteUserDecision::Deny),
        other => Err(format!("unknown routing user decision '{other}'")),
    }
}

const fn decision_authority_name(value: DecisionAuthority) -> &'static str {
    match value {
        DecisionAuthority::Policy => "policy",
        DecisionAuthority::User => "user",
    }
}

fn parse_decision_authority(value: &str) -> Result<DecisionAuthority, String> {
    match value {
        "policy" => Ok(DecisionAuthority::Policy),
        "user" => Ok(DecisionAuthority::User),
        other => Err(format!("unknown routing decision authority '{other}'")),
    }
}

const fn result_observation_name(value: RouteResultObservation) -> &'static str {
    match value {
        RouteResultObservation::ObservedResult => "observed_result",
        RouteResultObservation::ObservedError => "observed_error",
    }
}

fn parse_result_observation(value: &str) -> Result<RouteResultObservation, String> {
    match value {
        "observed_result" => Ok(RouteResultObservation::ObservedResult),
        "observed_error" => Ok(RouteResultObservation::ObservedError),
        other => Err(format!("unknown routing result observation '{other}'")),
    }
}

fn route_gate_error(route_id: RouteId, sequence: u64, error: RouteGateError) -> String {
    format!(
        "invalid routing history for route {} at sequence {}: {error:?}",
        route_id.get(),
        sequence
    )
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JsonlEventStore, MemoryEventStore, projection::SqliteProjection};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const MASTER: RouteEndpointId = RouteEndpointId::new(10);
    const WORKER: RouteEndpointId = RouteEndpointId::new(20);
    const TOOL: RouteEndpointId = RouteEndpointId::new(30);

    fn route(id: u64, destination: RouteEndpointId, class: RouteClass) -> RouteRequest {
        RouteRequest {
            id: RouteId::new(id),
            source: MASTER,
            destination,
            class,
        }
    }

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-routing-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn append_raw_typed(
        store: &mut impl EventStore,
        route_id: RouteId,
        kind: EventKind,
        record: &str,
        extra: Value,
    ) {
        let mut object = json!({
            "schema": ROUTING_AUDIT_SCHEMA,
            "version": ROUTING_AUDIT_VERSION,
            "record": record,
            "route_id": route_id.get(),
        });
        let Value::Object(extra_fields) = extra else {
            panic!("test extra must be object");
        };
        object
            .as_object_mut()
            .expect("base object")
            .extend(extra_fields);
        store
            .append_scoped(
                Some(route_scope(route_id)),
                kind,
                serde_json::to_string(&object).unwrap(),
            )
            .unwrap();
    }

    fn reopen_records(path: &PathBuf) -> Vec<RouteAuditRecord> {
        let reopened = JsonlEventStore::open(path).expect("reopen");
        replay_routing_audit(reopened.events()).expect("replay")
    }

    #[test]
    fn proposed_route_survives_reopen() {
        let path = temp_path("proposal", "jsonl");
        let request = route(1, WORKER, RouteClass::SessionMessage);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
        }

        let records = reopen_records(&path);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request, request);
        assert_eq!(records[0].initial_policy, RoutePolicy::RequireApproval);
        assert_eq!(records[0].gate_state, RouteGateState::PendingApproval);
        assert_eq!(records[0].proposed_sequence, 1);
        assert_eq!(records[0].last_sequence, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn auto_allowed_route_survives_reopen_undispatched() {
        let path = temp_path("auto-allow", "jsonl");
        let request = route(1, WORKER, RouteClass::SessionMessage);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(
            record.gate_state,
            RouteGateState::Allowed {
                by: DecisionAuthority::Policy,
            }
        );
        assert_eq!(record.dispatch_sequence, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn user_approval_survives_reopen() {
        let path = temp_path("user-allow", "jsonl");
        let request = route(1, WORKER, RouteClass::OrchestrationControl);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
            record_route_user_decision(&mut store, request.id, RouteUserDecision::Allow).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.latest_user_decision, Some(RouteUserDecision::Allow));
        assert_eq!(
            record.gate_state,
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            }
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn user_denial_survives_reopen() {
        let path = temp_path("user-deny", "jsonl");
        let request = route(1, TOOL, RouteClass::ToolCall);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
            record_route_user_decision(&mut store, request.id, RouteUserDecision::Deny).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.latest_user_decision, Some(RouteUserDecision::Deny));
        assert_eq!(
            record.gate_state,
            RouteGateState::Denied {
                by: DecisionAuthority::User,
            }
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn policy_denial_then_user_override_survives_reopen() {
        let path = temp_path("override", "jsonl");
        let request = route(1, TOOL, RouteClass::ToolCall);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::Deny).unwrap();
            record_route_user_decision(&mut store, request.id, RouteUserDecision::Allow).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(
            record.gate_state,
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            }
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn dispatched_route_survives_reopen_with_authority_and_is_not_dispatchable_again() {
        let path = temp_path("dispatched", "jsonl");
        let request = route(1, WORKER, RouteClass::OrchestrationControl);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
            record_route_user_decision(&mut store, request.id, RouteUserDecision::Allow).unwrap();

            let mut gate = RouteGate::new(request, RoutePolicy::RequireApproval);
            gate.user_allow().unwrap();
            let permit = gate.authorize_dispatch(request.id).unwrap();
            record_route_dispatched(&mut store, permit).unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(
            record.gate_state,
            RouteGateState::Dispatched {
                authorized_by: DecisionAuthority::User,
            }
        );
        assert_eq!(record.dispatch_sequence, Some(3));

        let mut replayed_gate = RouteGate::new(record.request, record.initial_policy);
        replayed_gate.user_allow().unwrap();
        replayed_gate.authorize_dispatch(record.request.id).unwrap();
        assert_eq!(
            replayed_gate.authorize_dispatch(record.request.id),
            Err(RouteGateError::AlreadyDispatched)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_result_observation_survives_reopen() {
        for (label, observation) in [
            ("result", RouteResultObservation::ObservedResult),
            ("error", RouteResultObservation::ObservedError),
        ] {
            let path = temp_path(label, "jsonl");
            let request = route(1, WORKER, RouteClass::SessionMessage);
            {
                let mut store = JsonlEventStore::open(&path).unwrap();
                record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
                let mut gate = RouteGate::new(request, RoutePolicy::Allow);
                let permit = gate.authorize_dispatch(request.id).unwrap();
                record_route_dispatched(&mut store, permit).unwrap();
                record_route_result(&mut store, request.id, observation).unwrap();
            }

            let record = reopen_records(&path).remove(0);
            assert_eq!(record.result, Some(observation));
            assert_eq!(record.result_sequence, Some(3));
            assert_eq!(record.last_sequence, 3);
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn torn_tail_cannot_fabricate_route_decision_or_dispatch() {
        let path = temp_path("torn-tail", "jsonl");
        let request = route(1, WORKER, RouteClass::OrchestrationControl);
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"route_dispatched""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let record = reopen_records(&path).remove(0);
        assert_eq!(record.gate_state, RouteGateState::PendingApproval);
        assert_eq!(record.dispatch_sequence, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_typed_payload_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(route_scope(RouteId::new(1))),
                EventKind::RouteProposed,
                json!({
                    "schema": ROUTING_AUDIT_SCHEMA,
                    "version": ROUTING_AUDIT_VERSION,
                    "record": "proposal",
                    "route_id": 1,
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_routing_audit(store.events()).is_err());
    }

    #[test]
    fn result_before_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        record_route_result(
            &mut store,
            request.id,
            RouteResultObservation::ObservedResult,
        )
        .unwrap();

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("before dispatch"));
    }

    #[test]
    fn dispatch_before_proposal_is_rejected() {
        let mut store = MemoryEventStore::default();
        append_raw_typed(
            &mut store,
            RouteId::new(1),
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "policy"}),
        );

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("before proposal"));
    }

    #[test]
    fn duplicate_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();

        append_raw_typed(
            &mut store,
            request.id,
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "policy"}),
        );
        append_raw_typed(
            &mut store,
            request.id,
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "policy"}),
        );

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate routing dispatch"));
    }

    #[test]
    fn denied_route_cannot_claim_dispatch() {
        let mut store = MemoryEventStore::default();
        let request = route(1, TOOL, RouteClass::ToolCall);
        record_route_proposed(&mut store, request, RoutePolicy::Deny).unwrap();
        append_raw_typed(
            &mut store,
            request.id,
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "policy"}),
        );

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("Denied"));
    }

    #[test]
    fn persisted_dispatch_authority_must_match_replayed_gate() {
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        append_raw_typed(
            &mut store,
            request.id,
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "user"}),
        );

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("authority mismatch"));
    }

    #[test]
    fn user_decision_after_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        append_raw_typed(
            &mut store,
            request.id,
            EventKind::RouteDispatched,
            "dispatch",
            json!({"authorized_by": "policy"}),
        );
        record_route_user_decision(&mut store, request.id, RouteUserDecision::Deny).unwrap();

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("AlreadyDispatched"));
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        store
            .append_scoped(
                Some(route_scope(RouteId::new(99))),
                EventKind::RouteProposed,
                json!({
                    "schema": ROUTING_AUDIT_SCHEMA,
                    "version": ROUTING_AUDIT_VERSION,
                    "record": "proposal",
                    "route_id": request.id.get(),
                    "source": request.source.get(),
                    "destination": request.destination.get(),
                    "class": "session_message",
                    "policy": "allow",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_routing_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(replay_routing_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn multiple_routes_replay_independently_in_proposal_order() {
        let mut store = MemoryEventStore::default();
        let first = route(1, WORKER, RouteClass::SessionMessage);
        let second = route(2, TOOL, RouteClass::ToolCall);

        record_route_proposed(&mut store, first, RoutePolicy::RequireApproval).unwrap();
        record_route_proposed(&mut store, second, RoutePolicy::Deny).unwrap();
        record_route_user_decision(&mut store, first.id, RouteUserDecision::Allow).unwrap();

        let records = replay_routing_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].request.id, first.id);
        assert_eq!(records[1].request.id, second.id);
        assert_eq!(
            records[0].gate_state,
            RouteGateState::Allowed {
                by: DecisionAuthority::User,
            }
        );
        assert_eq!(
            records[1].gate_state,
            RouteGateState::Denied {
                by: DecisionAuthority::Policy,
            }
        );
    }

    #[test]
    fn generic_sqlite_projection_carries_routing_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        let request = route(1, WORKER, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        let projected = projection.events_of_kind(EventKind::RouteProposed).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0], store.events()[0]);

        drop(projection);
        let _ = fs::remove_file(path);
    }
}
