//! Durable terminal observations for approved, dispatched local tool calls.
//!
//! This is an adapter-result audit, not an execution engine or XML/MCP wire
//! format. It does not execute a tool, infer success from dispatch, or treat
//! model text as a trusted result. A missing outcome remains unresolved.

use crate::routing_audit::replay_routing_audit;
use crate::tool_call_audit::replay_tool_call_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteGateState, RouteId, RoutePolicy,
};
use chatarium_core::session::SessionId;
use chatarium_core::tool::{ToolCallId, ToolProviderId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "chatarium-tool-outcome-audit";
const VERSION: u64 = 1;
/// Explicit cap for one terminal adapter observation. This never truncates text.
pub const MAX_OUTCOME_BYTES: usize = 1_048_576;

/// Terminal outcome asserted by a future trusted local tool adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallOutcomeKind {
    Result,
    Error,
}

impl ToolCallOutcomeKind {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Result => "result",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallOutcomeRecord {
    pub call_id: ToolCallId,
    pub route_id: RouteId,
    pub provider_id: ToolProviderId,
    pub source_session_id: SessionId,
    pub kind: ToolCallOutcomeKind,
    /// Exact result/error body supplied by the adapter, never normalized.
    pub text: String,
    pub call_recorded_sequence: u64,
    pub route_bound_sequence: u64,
    pub dispatch_sequence: u64,
    pub observed_sequence: u64,
}

/// Append a terminal observation made by an adapter.
///
/// This low-level append does not grant permission. Callers must keep the
/// adapter off the render thread and pass through the existing route gate.
/// Replay independently fails closed on an absent/denied/undispatched route.
pub fn record_tool_call_outcome(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
    kind: ToolCallOutcomeKind,
    text: impl Into<String>,
) -> std::io::Result<u64> {
    let text = text.into();
    if text.len() > MAX_OUTCOME_BYTES
        || (kind == ToolCallOutcomeKind::Error && text.trim().is_empty())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tool outcome is too large or has empty error text",
        ));
    }
    append_typed(
        store,
        tool_outcome_scope(call_id, route_id),
        outcome_value(call_id, route_id, kind, &text),
    )
}

/// Validate a complete prospective terminal observation before mutating the journal.
///
/// Future adapters must use this checked boundary, not the low-level append
/// helper. It runs the exact authoritative replay contract against an in-memory
/// prospective event before crossing the durable write boundary.
pub fn append_tool_call_outcome_checked(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
    kind: ToolCallOutcomeKind,
    text: impl Into<String>,
) -> Result<EventEnvelope, String> {
    let text = text.into();
    if text.len() > MAX_OUTCOME_BYTES
        || (kind == ToolCallOutcomeKind::Error && text.trim().is_empty())
    {
        return Err("tool outcome is too large or has empty error text".to_owned());
    }
    let sequence = u64::try_from(store.events().len())
        .map_err(|error| error.to_string())?
        .checked_add(1)
        .ok_or_else(|| "tool outcome journal sequence overflow".to_owned())?;
    let payload = serde_json::to_string(&outcome_value(call_id, route_id, kind, &text))
        .map_err(|error| error.to_string())?;
    let mut prospective = store.events().to_vec();
    prospective.push(EventEnvelope {
        sequence,
        at_unix_ms: 0,
        scope: Some(tool_outcome_scope(call_id, route_id)),
        kind: EventKind::ToolCallOutcomeObserved,
        payload,
    });
    let validated = replay_tool_call_outcome_audit(&prospective)?;
    if !validated
        .iter()
        .any(|record| record.call_id == call_id && record.observed_sequence == sequence)
    {
        return Err("prospective tool outcome was not projected".to_owned());
    }

    record_tool_call_outcome(store, call_id, route_id, kind, text)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "durable tool outcome append returned no event".to_owned())
}

/// Rebuild exact terminal tool results/errors from authoritative journal events.
///
/// Each outcome resolves its call/route against the journal prefix preceding
/// the observation; later unrelated changes cannot reinterpret its identity.
pub fn replay_tool_call_outcome_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolCallOutcomeRecord>, String> {
    let mut outcomes = BTreeMap::<ToolCallId, ToolCallOutcomeRecord>::new();
    let mut routed_outcomes = BTreeSet::<RouteId>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ToolCallOutcomeObserved {
            continue;
        }
        let value = typed_payload(event)?;
        let call_id = ToolCallId::new(required_u64(&value, "call_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let kind = match required_string(&value, "kind")? {
            "result" => ToolCallOutcomeKind::Result,
            "error" => ToolCallOutcomeKind::Error,
            other => {
                return Err(format!(
                    "tool outcome at sequence {} has unsupported kind '{other}'",
                    event.sequence
                ));
            }
        };
        let text = required_string(&value, "text")?.to_owned();
        validate_scope(event, call_id, route_id)?;
        if text.len() > MAX_OUTCOME_BYTES {
            return Err(format!(
                "tool outcome at sequence {} exceeds maximum body size",
                event.sequence
            ));
        }
        if kind == ToolCallOutcomeKind::Error && text.trim().is_empty() {
            return Err(format!(
                "tool error at sequence {} has empty error body",
                event.sequence
            ));
        }
        if outcomes.contains_key(&call_id) || !routed_outcomes.insert(route_id) {
            return Err(format!(
                "duplicate terminal tool outcome for call {} or route {} at sequence {}",
                call_id.get(),
                route_id.get(),
                event.sequence
            ));
        }

        let prior = &events[..index];
        let call = replay_tool_call_audit(prior)?
            .into_iter()
            .find(|record| record.call_id == call_id)
            .ok_or_else(|| {
                format!(
                    "tool outcome at sequence {} references missing call {}",
                    event.sequence,
                    call_id.get()
                )
            })?;
        if call.route_id != Some(route_id) {
            return Err(format!(
                "tool outcome at sequence {} references route {} not bound to call {}",
                event.sequence,
                route_id.get(),
                call_id.get()
            ));
        }
        let route_bound_sequence = call.route_bound_sequence.ok_or_else(|| {
            format!(
                "tool outcome at sequence {} references call {} without durable route binding",
                event.sequence,
                call_id.get()
            )
        })?;
        let route = replay_routing_audit(prior)?
            .into_iter()
            .find(|record| record.request.id == route_id)
            .ok_or_else(|| {
                format!(
                    "tool outcome at sequence {} references missing route {}",
                    event.sequence,
                    route_id.get()
                )
            })?;
        if route.request.class != RouteClass::ToolCall
            || route.initial_policy != RoutePolicy::RequireApproval
        {
            return Err(format!(
                "tool outcome at sequence {} references route {} without explicit-approval tool policy",
                event.sequence,
                route_id.get()
            ));
        }
        if route.gate_state
            != (RouteGateState::Dispatched {
                authorized_by: DecisionAuthority::User,
            })
        {
            return Err(format!(
                "tool outcome at sequence {} references route {} before explicitly approved dispatch",
                event.sequence,
                route_id.get()
            ));
        }
        let dispatch_sequence = route.dispatch_sequence.ok_or_else(|| {
            format!(
                "tool outcome at sequence {} references undispatched route {}",
                event.sequence,
                route_id.get()
            )
        })?;
        if !(call.recorded_sequence < route_bound_sequence
            && route_bound_sequence < dispatch_sequence
            && dispatch_sequence < event.sequence)
        {
            return Err(format!(
                "tool outcome at sequence {} has invalid call/bind/dispatch order",
                event.sequence
            ));
        }
        outcomes.insert(
            call_id,
            ToolCallOutcomeRecord {
                call_id,
                route_id,
                provider_id: call.provider_id,
                source_session_id: call.source_session_id,
                kind,
                text,
                call_recorded_sequence: call.recorded_sequence,
                route_bound_sequence,
                dispatch_sequence,
                observed_sequence: event.sequence,
            },
        );
    }

    let mut records = outcomes.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.observed_sequence);
    Ok(records)
}

fn outcome_value(
    call_id: ToolCallId,
    route_id: RouteId,
    kind: ToolCallOutcomeKind,
    text: &str,
) -> Value {
    json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "tool_call_outcome_observed",
        "call_id": call_id.get(),
        "route_id": route_id.get(),
        "kind": kind.stable_name(),
        "text": text,
    })
}

/// Stable journal correlation for one tool call's terminal result.
#[must_use]
pub fn tool_outcome_scope(call_id: ToolCallId, route_id: RouteId) -> String {
    format!("tool-outcome:{}:{}", call_id.get(), route_id.get())
}

fn append_typed(
    store: &mut impl EventStore,
    scope: String,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(scope), EventKind::ToolCallOutcomeObserved, encoded)
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed tool outcome at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
        || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        || value.get("record").and_then(Value::as_str) != Some("tool_call_outcome_observed")
    {
        return Err(format!(
            "tool outcome at sequence {} has missing/unsupported schema, version or record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    call_id: ToolCallId,
    route_id: RouteId,
) -> Result<(), String> {
    let expected = tool_outcome_scope(call_id, route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "tool outcome at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("tool outcome is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("tool outcome is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::routing_audit::{
        RouteUserDecision, record_route_dispatched, record_route_proposed,
        record_route_user_decision,
    };
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use crate::tool_call_audit::{record_tool_call, record_tool_call_route_bound};
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use chatarium_core::routing::{RouteEndpointId, RouteGate, RouteRequest};
    use chatarium_core::session::SessionEndpointBinding;
    use chatarium_core::tool::{ToolOperationName, ToolProviderEndpointBinding, ToolProviderName};

    const SESSION: SessionId = SessionId::new(1);
    const SOURCE: RouteEndpointId = RouteEndpointId::new(10);
    const DESTINATION: RouteEndpointId = RouteEndpointId::new(20);
    const PROVIDER: ToolProviderId = ToolProviderId::new(3);
    const CALL: ToolCallId = ToolCallId::new(5);
    const ROUTE: RouteId = RouteId::new(7);

    fn setup(store: &mut impl EventStore) -> RouteRequest {
        record_local_session_registered(store, SESSION).unwrap();
        record_session_endpoint_bound(store, SessionEndpointBinding::new(SESSION, SOURCE)).unwrap();
        record_tool_provider_registered(
            store,
            PROVIDER,
            &ToolProviderName::new("local-tool").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            store,
            ToolProviderEndpointBinding::new(PROVIDER, DESTINATION),
        )
        .unwrap();
        record_tool_call(
            store,
            CALL,
            SESSION,
            PROVIDER,
            &ToolOperationName::new("inspect").unwrap(),
            " exact args ",
        )
        .unwrap();
        let route = RouteRequest {
            id: ROUTE,
            source: SOURCE,
            destination: DESTINATION,
            class: RouteClass::ToolCall,
        };
        record_route_proposed(store, route, RoutePolicy::RequireApproval).unwrap();
        record_tool_call_route_bound(store, CALL, ROUTE).unwrap();
        route
    }

    fn dispatch(store: &mut impl EventStore, route: RouteRequest) {
        record_route_user_decision(store, ROUTE, RouteUserDecision::Allow).unwrap();
        let mut gate = RouteGate::new(route, RoutePolicy::RequireApproval);
        gate.user_allow().unwrap();
        let permit = gate.authorize_dispatch(ROUTE).unwrap();
        record_route_dispatched(store, permit).unwrap();
    }

    #[test]
    fn result_preserves_exact_text_and_source_provenance() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        dispatch(&mut store, route);
        let sequence = record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            " exact result\nwith spacing ",
        )
        .unwrap();

        let records = replay_tool_call_outcome_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].call_id, CALL);
        assert_eq!(records[0].provider_id, PROVIDER);
        assert_eq!(records[0].source_session_id, SESSION);
        assert_eq!(records[0].text, " exact result\nwith spacing ");
        assert_eq!(records[0].observed_sequence, sequence);
        assert!(records[0].dispatch_sequence < records[0].observed_sequence);
    }

    #[test]
    fn error_is_terminal_and_duplicate_is_rejected() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        dispatch(&mut store, route);
        record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Error,
            "adapter failed",
        )
        .unwrap();
        record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            "late result",
        )
        .unwrap();
        assert!(
            replay_tool_call_outcome_audit(store.events())
                .unwrap_err()
                .contains("duplicate terminal")
        );
    }

    #[test]
    fn missing_or_denied_dispatch_cannot_produce_result() {
        let mut store = MemoryEventStore::default();
        setup(&mut store);
        record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            "fabricated",
        )
        .unwrap();
        assert!(
            replay_tool_call_outcome_audit(store.events())
                .unwrap_err()
                .contains("before explicitly approved dispatch")
        );

        let mut store = MemoryEventStore::default();
        setup(&mut store);
        record_route_user_decision(&mut store, ROUTE, RouteUserDecision::Deny).unwrap();
        record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Error,
            "denied",
        )
        .unwrap();
        assert!(
            replay_tool_call_outcome_audit(store.events())
                .unwrap_err()
                .contains("before explicitly approved dispatch")
        );
    }

    #[test]
    fn route_mismatch_and_invalid_bodies_fail_closed() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        dispatch(&mut store, route);
        record_tool_call_outcome(
            &mut store,
            CALL,
            RouteId::new(99),
            ToolCallOutcomeKind::Result,
            "wrong route",
        )
        .unwrap();
        assert!(
            replay_tool_call_outcome_audit(store.events())
                .unwrap_err()
                .contains("not bound")
        );

        assert!(
            record_tool_call_outcome(
                &mut MemoryEventStore::default(),
                CALL,
                ROUTE,
                ToolCallOutcomeKind::Error,
                "   ",
            )
            .is_err()
        );
        assert!(
            record_tool_call_outcome(
                &mut MemoryEventStore::default(),
                CALL,
                ROUTE,
                ToolCallOutcomeKind::Result,
                "x".repeat(MAX_OUTCOME_BYTES + 1),
            )
            .is_err()
        );
    }

    #[test]
    fn checked_outcome_rejects_early_and_duplicate_without_mutating_journal() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        let before_approval = store.events().len();
        assert!(
            append_tool_call_outcome_checked(
                &mut store,
                CALL,
                ROUTE,
                ToolCallOutcomeKind::Result,
                "too soon",
            )
            .unwrap_err()
            .contains("before explicitly approved dispatch")
        );
        assert_eq!(store.events().len(), before_approval);

        dispatch(&mut store, route);
        let event = append_tool_call_outcome_checked(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            " exact ",
        )
        .unwrap();
        assert_eq!(event.kind, EventKind::ToolCallOutcomeObserved);
        assert_eq!(
            replay_tool_call_outcome_audit(store.events()).unwrap()[0].text,
            " exact "
        );

        let after_first = store.events().len();
        assert!(
            append_tool_call_outcome_checked(
                &mut store,
                CALL,
                ROUTE,
                ToolCallOutcomeKind::Error,
                "duplicate",
            )
            .unwrap_err()
            .contains("duplicate terminal")
        );
        assert_eq!(store.events().len(), after_first);
    }

    #[test]
    fn checked_outcome_rejects_wrong_call_route_without_append() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        dispatch(&mut store, route);
        let before = store.events().len();
        assert!(
            append_tool_call_outcome_checked(
                &mut store,
                CALL,
                RouteId::new(99),
                ToolCallOutcomeKind::Result,
                "wrong",
            )
            .unwrap_err()
            .contains("not bound")
        );
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn unobserved_dispatch_remains_unresolved() {
        let mut store = MemoryEventStore::default();
        let route = setup(&mut store);
        dispatch(&mut store, route);
        assert!(
            replay_tool_call_outcome_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }
}
