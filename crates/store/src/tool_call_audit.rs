//! Durable immutable tool-call intent and routing correlation.
//!
//! This audit is transport-neutral. It records exact local call intent and
//! proves that any correlated route is a pending RequireApproval ToolCall route.
//! It does not define the future XML/MCP wire envelope and does not execute tools.

use crate::routing_audit::replay_routing_audit;
use crate::session_audit::replay_session_audit;
use crate::tool_provider_audit::replay_tool_provider_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::routing::{RouteClass, RouteGateState, RouteId, RoutePolicy};
use chatarium_core::session::SessionId;
use chatarium_core::tool::{ToolCallId, ToolOperationName, ToolProviderId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "chatarium-tool-call-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallAuditRecord {
    pub call_id: ToolCallId,
    pub source_session_id: SessionId,
    pub provider_id: ToolProviderId,
    pub operation: ToolOperationName,
    /// Exact local argument/payload text. This is not yet a versioned MCP/XML wire envelope.
    pub arguments_text: String,
    pub recorded_sequence: u64,
    pub route_id: Option<RouteId>,
    pub route_bound_sequence: Option<u64>,
}

pub fn record_tool_call(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    source_session_id: SessionId,
    provider_id: ToolProviderId,
    operation: &ToolOperationName,
    arguments_text: impl Into<String>,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(tool_call_scope(call_id)),
        EventKind::ToolCallRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "tool_call_recorded",
            "call_id": call_id.get(),
            "source_session_id": source_session_id.get(),
            "provider_id": provider_id.get(),
            "operation": operation.as_str(),
            "arguments_text": arguments_text.into(),
        }),
    )
}

pub fn record_tool_call_route_bound(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(tool_call_route_scope(call_id, route_id)),
        EventKind::ToolCallRouteBound,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "tool_call_route_bound",
            "call_id": call_id.get(),
            "route_id": route_id.get(),
        }),
    )
}

pub fn replay_tool_call_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolCallAuditRecord>, String> {
    let mut calls = BTreeMap::<ToolCallId, ToolCallAuditRecord>::new();
    let mut route_owner = BTreeMap::<RouteId, ToolCallId>::new();

    for (index, event) in events.iter().enumerate() {
        match event.kind {
            EventKind::ToolCallRecorded => {
                let value = typed_payload(event, "tool_call_recorded")?;
                let call_id = ToolCallId::new(required_u64(&value, "call_id")?);
                let source_session_id = SessionId::new(required_u64(&value, "source_session_id")?);
                let provider_id = ToolProviderId::new(required_u64(&value, "provider_id")?);
                let operation =
                    ToolOperationName::new(required_string(&value, "operation")?.to_owned())
                        .map_err(|error| {
                            format!(
                                "tool call {} at sequence {} has invalid operation name: {error:?}",
                                call_id.get(),
                                event.sequence
                            )
                        })?;
                let arguments_text = required_string(&value, "arguments_text")?.to_owned();
                validate_scope(event, &tool_call_scope(call_id))?;

                if calls.contains_key(&call_id) {
                    return Err(format!(
                        "duplicate tool call {} at sequence {}",
                        call_id.get(),
                        event.sequence
                    ));
                }

                let prior = &events[..index];
                let session = replay_session_audit(prior)?
                    .into_iter()
                    .find(|record| record.session_id == source_session_id)
                    .ok_or_else(|| {
                        format!(
                            "tool call {} at sequence {} references unregistered source session {}",
                            call_id.get(),
                            event.sequence,
                            source_session_id.get()
                        )
                    })?;
                if session.registered_sequence >= event.sequence {
                    return Err(format!(
                        "tool call {} at sequence {} precedes source session registration",
                        call_id.get(),
                        event.sequence
                    ));
                }

                let provider = replay_tool_provider_audit(prior)?
                    .into_iter()
                    .find(|record| record.provider_id == provider_id)
                    .ok_or_else(|| {
                        format!(
                            "tool call {} at sequence {} references unregistered provider {}",
                            call_id.get(),
                            event.sequence,
                            provider_id.get()
                        )
                    })?;
                if provider.registered_sequence >= event.sequence {
                    return Err(format!(
                        "tool call {} at sequence {} precedes provider registration",
                        call_id.get(),
                        event.sequence
                    ));
                }

                calls.insert(
                    call_id,
                    ToolCallAuditRecord {
                        call_id,
                        source_session_id,
                        provider_id,
                        operation,
                        arguments_text,
                        recorded_sequence: event.sequence,
                        route_id: None,
                        route_bound_sequence: None,
                    },
                );
            }
            EventKind::ToolCallRouteBound => {
                let value = typed_payload(event, "tool_call_route_bound")?;
                let call_id = ToolCallId::new(required_u64(&value, "call_id")?);
                let route_id = RouteId::new(required_u64(&value, "route_id")?);
                validate_scope(event, &tool_call_route_scope(call_id, route_id))?;

                let call = calls.get_mut(&call_id).ok_or_else(|| {
                    format!(
                        "tool call route binding at sequence {} references missing call {}",
                        event.sequence,
                        call_id.get()
                    )
                })?;
                if let Some(existing) = call.route_id {
                    return Err(format!(
                        "tool call {} is already bound to route {}; cannot also bind route {} at sequence {}",
                        call_id.get(),
                        existing.get(),
                        route_id.get(),
                        event.sequence
                    ));
                }
                if let Some(existing_call) = route_owner.get(&route_id) {
                    return Err(format!(
                        "tool route {} already belongs to call {}; cannot also bind call {} at sequence {}",
                        route_id.get(),
                        existing_call.get(),
                        call_id.get(),
                        event.sequence
                    ));
                }

                let prior = &events[..index];
                let route = replay_routing_audit(prior)?
                    .into_iter()
                    .find(|record| record.request.id == route_id)
                    .ok_or_else(|| {
                        format!(
                            "tool call {} route binding at sequence {} references missing route {}",
                            call_id.get(),
                            event.sequence,
                            route_id.get()
                        )
                    })?;
                if call.recorded_sequence >= route.proposed_sequence {
                    return Err(format!(
                        "tool route {} was proposed at sequence {} before immutable call {} was recorded at sequence {}",
                        route_id.get(),
                        route.proposed_sequence,
                        call_id.get(),
                        call.recorded_sequence
                    ));
                }
                if route.request.class != RouteClass::ToolCall {
                    return Err(format!(
                        "tool call {} is bound to non-tool route {}",
                        call_id.get(),
                        route_id.get()
                    ));
                }
                if route.initial_policy != RoutePolicy::RequireApproval {
                    return Err(format!(
                        "tool route {} must begin under RequireApproval policy",
                        route_id.get()
                    ));
                }
                if route.gate_state != RouteGateState::PendingApproval
                    || route.latest_user_decision.is_some()
                    || route.dispatch_sequence.is_some()
                {
                    return Err(format!(
                        "tool route {} must still be pending explicit approval when call {} is bound",
                        route_id.get(),
                        call_id.get()
                    ));
                }

                let sessions = replay_session_audit(prior)?;
                let source_session = sessions
                    .iter()
                    .find(|record| record.session_id == call.source_session_id)
                    .ok_or_else(|| {
                        format!(
                            "tool call {} source session {} disappeared before route binding",
                            call_id.get(),
                            call.source_session_id.get()
                        )
                    })?;
                let source_endpoint = source_session.endpoint_binding.ok_or_else(|| {
                    format!(
                        "tool call {} source session {} has no routing endpoint",
                        call_id.get(),
                        call.source_session_id.get()
                    )
                })?;
                let source_endpoint_sequence =
                    source_session.endpoint_bound_sequence.ok_or_else(|| {
                        format!(
                            "tool call {} source session {} has no endpoint bind sequence",
                            call_id.get(),
                            call.source_session_id.get()
                        )
                    })?;
                if source_endpoint_sequence >= route.proposed_sequence {
                    return Err(format!(
                        "tool route {} was proposed before source endpoint binding",
                        route_id.get()
                    ));
                }
                if route.request.source != source_endpoint.endpoint_id() {
                    return Err(format!(
                        "tool route {} source endpoint {} does not match call {} source session {} endpoint {}",
                        route_id.get(),
                        route.request.source.get(),
                        call_id.get(),
                        call.source_session_id.get(),
                        source_endpoint.endpoint_id().get()
                    ));
                }

                let provider = replay_tool_provider_audit(prior)?
                    .into_iter()
                    .find(|record| record.provider_id == call.provider_id)
                    .ok_or_else(|| {
                        format!(
                            "tool call {} provider {} disappeared before route binding",
                            call_id.get(),
                            call.provider_id.get()
                        )
                    })?;
                let provider_endpoint = provider.endpoint_binding.ok_or_else(|| {
                    format!(
                        "tool call {} provider {} has no routing endpoint",
                        call_id.get(),
                        call.provider_id.get()
                    )
                })?;
                let provider_endpoint_sequence =
                    provider.endpoint_bound_sequence.ok_or_else(|| {
                        format!(
                            "tool call {} provider {} has no endpoint bind sequence",
                            call_id.get(),
                            call.provider_id.get()
                        )
                    })?;
                if provider_endpoint_sequence >= route.proposed_sequence {
                    return Err(format!(
                        "tool route {} was proposed before provider endpoint binding",
                        route_id.get()
                    ));
                }
                if route.request.destination != provider_endpoint.endpoint_id() {
                    return Err(format!(
                        "tool route {} destination endpoint {} does not match provider {} endpoint {}",
                        route_id.get(),
                        route.request.destination.get(),
                        call.provider_id.get(),
                        provider_endpoint.endpoint_id().get()
                    ));
                }

                call.route_id = Some(route_id);
                call.route_bound_sequence = Some(event.sequence);
                route_owner.insert(route_id, call_id);
            }
            _ => {}
        }
    }

    let mut records = calls.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.recorded_sequence);
    Ok(records)
}

#[must_use]
pub fn bound_tool_route_ids(records: &[ToolCallAuditRecord]) -> BTreeSet<RouteId> {
    records
        .iter()
        .filter_map(|record| record.route_id)
        .collect()
}

#[must_use]
pub fn tool_call_scope(call_id: ToolCallId) -> String {
    format!("tool-call:{}", call_id.get())
}

#[must_use]
pub fn tool_call_route_scope(call_id: ToolCallId, route_id: RouteId) -> String {
    format!("tool-call-route:{}:{}", call_id.get(), route_id.get())
}

fn append_typed(
    store: &mut impl EventStore,
    scope: Option<String>,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(scope, kind, encoded)
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed tool call payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "tool call event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "tool call event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "tool call event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "tool call event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed tool call payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed tool call payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::routing_audit::{
        RouteUserDecision, record_route_proposed, record_route_user_decision,
    };
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use chatarium_core::routing::{RouteEndpointId, RouteRequest};
    use chatarium_core::session::{SessionEndpointBinding, SessionId};
    use chatarium_core::tool::{ToolProviderEndpointBinding, ToolProviderName};

    const SESSION: SessionId = SessionId::new(1);
    const SOURCE: RouteEndpointId = RouteEndpointId::new(10);
    const DESTINATION: RouteEndpointId = RouteEndpointId::new(20);
    const PROVIDER: ToolProviderId = ToolProviderId::new(1);
    const CALL: ToolCallId = ToolCallId::new(1);
    const ROUTE: RouteId = RouteId::new(1);

    fn setup_addressable(store: &mut impl EventStore) {
        record_local_session_registered(store, SESSION).unwrap();
        record_session_endpoint_bound(store, SessionEndpointBinding::new(SESSION, SOURCE)).unwrap();
        record_tool_provider_registered(
            store,
            PROVIDER,
            &ToolProviderName::new("local-files").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            store,
            ToolProviderEndpointBinding::new(PROVIDER, DESTINATION),
        )
        .unwrap();
    }

    fn record_call(store: &mut impl EventStore) {
        record_tool_call(
            store,
            CALL,
            SESSION,
            PROVIDER,
            &ToolOperationName::new("read_file").unwrap(),
            " exact args\n ",
        )
        .unwrap();
    }

    fn propose(store: &mut impl EventStore, class: RouteClass, policy: RoutePolicy) {
        record_route_proposed(
            store,
            RouteRequest {
                id: ROUTE,
                source: SOURCE,
                destination: DESTINATION,
                class,
            },
            policy,
        )
        .unwrap();
    }

    #[test]
    fn immutable_call_and_pending_route_binding_replay() {
        let mut store = MemoryEventStore::default();
        setup_addressable(&mut store);
        record_call(&mut store);
        propose(
            &mut store,
            RouteClass::ToolCall,
            RoutePolicy::RequireApproval,
        );
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();

        let records = replay_tool_call_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].call_id, CALL);
        assert_eq!(records[0].source_session_id, SESSION);
        assert_eq!(records[0].provider_id, PROVIDER);
        assert_eq!(records[0].operation.as_str(), "read_file");
        assert_eq!(records[0].arguments_text, " exact args\n ");
        assert_eq!(records[0].route_id, Some(ROUTE));

        let route = replay_routing_audit(store.events()).unwrap().remove(0);
        assert_eq!(route.gate_state, RouteGateState::PendingApproval);
    }

    #[test]
    fn call_requires_registered_source_and_provider() {
        let mut missing_source = MemoryEventStore::default();
        record_tool_provider_registered(
            &mut missing_source,
            PROVIDER,
            &ToolProviderName::new("provider").unwrap(),
        )
        .unwrap();
        record_call(&mut missing_source);
        assert!(
            replay_tool_call_audit(missing_source.events())
                .unwrap_err()
                .contains("unregistered source session")
        );

        let mut missing_provider = MemoryEventStore::default();
        record_local_session_registered(&mut missing_provider, SESSION).unwrap();
        record_tool_call(
            &mut missing_provider,
            CALL,
            SESSION,
            PROVIDER,
            &ToolOperationName::new("op").unwrap(),
            "",
        )
        .unwrap();
        assert!(
            replay_tool_call_audit(missing_provider.events())
                .unwrap_err()
                .contains("unregistered provider")
        );
    }

    #[test]
    fn route_binding_requires_tool_class_and_require_approval() {
        for (class, policy, expected) in [
            (
                RouteClass::SessionMessage,
                RoutePolicy::RequireApproval,
                "non-tool route",
            ),
            (RouteClass::ToolCall, RoutePolicy::Allow, "RequireApproval"),
        ] {
            let mut store = MemoryEventStore::default();
            setup_addressable(&mut store);
            record_call(&mut store);
            propose(&mut store, class, policy);
            record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();
            assert!(
                replay_tool_call_audit(store.events())
                    .unwrap_err()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn route_binding_rejects_wrong_endpoint_or_preexisting_decision() {
        let mut wrong_destination = MemoryEventStore::default();
        setup_addressable(&mut wrong_destination);
        record_call(&mut wrong_destination);
        record_route_proposed(
            &mut wrong_destination,
            RouteRequest {
                id: ROUTE,
                source: SOURCE,
                destination: RouteEndpointId::new(99),
                class: RouteClass::ToolCall,
            },
            RoutePolicy::RequireApproval,
        )
        .unwrap();
        record_tool_call_route_bound(&mut wrong_destination, CALL, ROUTE).unwrap();
        assert!(
            replay_tool_call_audit(wrong_destination.events())
                .unwrap_err()
                .contains("does not match provider")
        );

        let mut decided = MemoryEventStore::default();
        setup_addressable(&mut decided);
        record_call(&mut decided);
        propose(
            &mut decided,
            RouteClass::ToolCall,
            RoutePolicy::RequireApproval,
        );
        record_route_user_decision(&mut decided, ROUTE, RouteUserDecision::Allow).unwrap();
        record_tool_call_route_bound(&mut decided, CALL, ROUTE).unwrap();
        assert!(
            replay_tool_call_audit(decided.events())
                .unwrap_err()
                .contains("pending explicit approval")
        );
    }

    #[test]
    fn route_must_be_proposed_after_immutable_call() {
        let mut store = MemoryEventStore::default();
        setup_addressable(&mut store);
        propose(
            &mut store,
            RouteClass::ToolCall,
            RoutePolicy::RequireApproval,
        );
        record_call(&mut store);
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();

        assert!(
            replay_tool_call_audit(store.events())
                .unwrap_err()
                .contains("before immutable call")
        );
    }

    #[test]
    fn call_and_route_correlations_are_one_to_one() {
        let mut store = MemoryEventStore::default();
        setup_addressable(&mut store);
        record_call(&mut store);
        propose(
            &mut store,
            RouteClass::ToolCall,
            RoutePolicy::RequireApproval,
        );
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();
        record_tool_call_route_bound(&mut store, CALL, RouteId::new(2)).unwrap();
        assert!(
            replay_tool_call_audit(store.events())
                .unwrap_err()
                .contains("already bound to route")
        );
    }
}
