//! Durable immutable payload correlation for local session-message routes.
//!
//! Route identity/policy remains owned by routing_audit. This module stores the
//! exact routed text separately and proves which local conversations owned the
//! route endpoints when that payload was attached.

use crate::local_routing_directory::replay_local_routing_directory;
use crate::routing_audit::replay_routing_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::routing::{RouteClass, RouteId, RoutePayloadId};
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-route-payload-audit";
const VERSION: u64 = 1;

/// One immutable text payload attached to one local session-message route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRoutePayloadRecord {
    pub payload_id: RoutePayloadId,
    pub route_id: RouteId,
    pub source_conversation_id: LocalConversationId,
    pub destination_conversation_id: LocalConversationId,
    pub text: String,
    pub attached_sequence: u64,
}

/// Append one exact text payload correlated to an already-proposed local route.
///
/// Higher-level callers should validate policy/product constraints before append.
/// Replay independently validates historical route and endpoint provenance.
pub fn record_local_route_payload_attached(
    store: &mut impl EventStore,
    payload_id: RoutePayloadId,
    route_id: RouteId,
    source_conversation_id: LocalConversationId,
    destination_conversation_id: LocalConversationId,
    text: impl Into<String>,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(payload_scope(route_id, payload_id)),
        EventKind::RoutePayloadAttached,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "route_payload_attached",
            "payload_id": payload_id.get(),
            "route_id": route_id.get(),
            "source_conversation_id": source_conversation_id.to_string(),
            "destination_conversation_id": destination_conversation_id.to_string(),
            "text": text.into(),
        }),
    )
}

/// Reconstruct immutable local route payloads from authoritative journal events.
///
/// Validation is point-in-time: each payload is checked against the journal
/// prefix that existed immediately before its attachment event. Later session
/// rollover therefore cannot invalidate historical endpoint provenance.
pub fn replay_local_route_payload_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalRoutePayloadRecord>, String> {
    let mut by_route = BTreeMap::<RouteId, LocalRoutePayloadRecord>::new();
    let mut payload_owner = BTreeMap::<RoutePayloadId, RouteId>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::RoutePayloadAttached {
            continue;
        }

        let value = typed_payload(event)?;
        let payload_id = RoutePayloadId::new(required_u64(&value, "payload_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let source_conversation_id =
            parse_conversation_id(&value, "source_conversation_id", event.sequence)?;
        let destination_conversation_id =
            parse_conversation_id(&value, "destination_conversation_id", event.sequence)?;
        let text = required_string(&value, "text")?.to_owned();
        validate_scope(event, route_id, payload_id)?;

        if text.trim().is_empty() {
            return Err(format!(
                "route payload {} at sequence {} has empty text",
                payload_id.get(),
                event.sequence
            ));
        }
        if source_conversation_id == destination_conversation_id {
            return Err(format!(
                "route payload {} at sequence {} names the same local conversation on both ends",
                payload_id.get(),
                event.sequence
            ));
        }
        if let Some(existing) = by_route.get(&route_id) {
            return Err(format!(
                "route {} already has payload {}; cannot also attach payload {} at sequence {}",
                route_id.get(),
                existing.payload_id.get(),
                payload_id.get(),
                event.sequence
            ));
        }
        if let Some(existing_route) = payload_owner.get(&payload_id) {
            return Err(format!(
                "payload {} already belongs to route {}; cannot also attach it to route {} at sequence {}",
                payload_id.get(),
                existing_route.get(),
                route_id.get(),
                event.sequence
            ));
        }

        let prior = &events[..index];
        let route = replay_routing_audit(prior)?
            .into_iter()
            .find(|record| record.request.id == route_id)
            .ok_or_else(|| {
                format!(
                    "route payload {} at sequence {} references route {} before proposal",
                    payload_id.get(),
                    event.sequence,
                    route_id.get()
                )
            })?;
        if route.request.class != RouteClass::SessionMessage {
            return Err(format!(
                "route payload {} at sequence {} references non-session-message route {}",
                payload_id.get(),
                event.sequence,
                route_id.get()
            ));
        }
        if route.gate_state.is_dispatched() {
            return Err(format!(
                "route payload {} at sequence {} was attached after route {} dispatched",
                payload_id.get(),
                event.sequence,
                route_id.get()
            ));
        }

        let directory = replay_local_routing_directory(prior)?;
        let source = directory
            .iter()
            .find(|entry| entry.endpoint_id == route.request.source)
            .ok_or_else(|| {
                format!(
                    "route payload {} at sequence {} cannot resolve source endpoint {} in the historical local routing directory",
                    payload_id.get(),
                    event.sequence,
                    route.request.source.get()
                )
            })?;
        let destination = directory
            .iter()
            .find(|entry| entry.endpoint_id == route.request.destination)
            .ok_or_else(|| {
                format!(
                    "route payload {} at sequence {} cannot resolve destination endpoint {} in the historical local routing directory",
                    payload_id.get(),
                    event.sequence,
                    route.request.destination.get()
                )
            })?;

        if source.conversation_id != source_conversation_id {
            return Err(format!(
                "route payload {} source conversation disagrees with historical endpoint ownership at sequence {}",
                payload_id.get(),
                event.sequence
            ));
        }
        if destination.conversation_id != destination_conversation_id {
            return Err(format!(
                "route payload {} destination conversation disagrees with historical endpoint ownership at sequence {}",
                payload_id.get(),
                event.sequence
            ));
        }
        if !source.current_session_phase.accepts_ordinary_turns()
            || !destination.current_session_phase.accepts_ordinary_turns()
        {
            return Err(format!(
                "route payload {} at sequence {} was attached while one endpoint could not accept ordinary turns",
                payload_id.get(),
                event.sequence
            ));
        }

        let record = LocalRoutePayloadRecord {
            payload_id,
            route_id,
            source_conversation_id,
            destination_conversation_id,
            text,
            attached_sequence: event.sequence,
        };
        by_route.insert(route_id, record);
        payload_owner.insert(payload_id, route_id);
    }

    let mut records = by_route.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.attached_sequence);
    Ok(records)
}

/// Stable scope for one route-payload correlation.
#[must_use]
pub fn payload_scope(route_id: RouteId, payload_id: RoutePayloadId) -> String {
    format!("route-payload:{}:{}", route_id.get(), payload_id.get())
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

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed local route payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "route payload event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "route payload event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("route_payload_attached") {
        return Err(format!(
            "route payload event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    route_id: RouteId,
    payload_id: RoutePayloadId,
) -> Result<(), String> {
    let expected = payload_scope(route_id, payload_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "route payload event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn parse_conversation_id(
    value: &Value,
    field: &str,
    sequence: u64,
) -> Result<LocalConversationId, String> {
    LocalConversationId::from_str(required_string(value, field)?).map_err(|error| {
        format!("route payload event at sequence {sequence} has invalid {field}: {error}")
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed route payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed route payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::chat_container_audit::{
        record_chat_container_created, record_chat_session_lifecycle_transition,
        record_chat_session_successor_bound,
    };
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::routing_audit::{record_route_dispatched, record_route_proposed};
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use chatarium_core::chat_container::{
        ChatContainerId, ContextHandoffId, SessionLifecyclePhase, SessionLifecycleTransition,
        SessionSuccessorBinding,
    };
    use chatarium_core::routing::{RouteEndpointId, RouteGate, RoutePolicy, RouteRequest};
    use chatarium_core::session::{SessionEndpointBinding, SessionId};

    fn addressable(
        store: &mut impl EventStore,
        conversation_id: LocalConversationId,
        container_id: ChatContainerId,
        session_id: SessionId,
        endpoint_id: RouteEndpointId,
    ) {
        record_local_session_registered(store, session_id).unwrap();
        record_chat_container_created(store, container_id, session_id).unwrap();
        record_local_conversation_chat_container_bound(store, conversation_id, container_id)
            .unwrap();
        record_session_endpoint_bound(store, SessionEndpointBinding::new(session_id, endpoint_id))
            .unwrap();
    }

    fn proposed_route(
        store: &mut impl EventStore,
        route_id: RouteId,
        source: RouteEndpointId,
        destination: RouteEndpointId,
        policy: RoutePolicy,
    ) -> RouteRequest {
        let request = RouteRequest {
            id: route_id,
            source,
            destination,
            class: RouteClass::SessionMessage,
        };
        record_route_proposed(store, request, policy).unwrap();
        request
    }

    #[test]
    fn payload_preserves_exact_text_and_point_in_time_conversation_provenance() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );
        proposed_route(
            &mut store,
            RouteId::new(1),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );

        let exact = " exact routed text\nwith spacing ";
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            exact,
        )
        .unwrap();

        let records = replay_local_route_payload_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].text, exact);
        assert_eq!(records[0].source_conversation_id, source);
        assert_eq!(records[0].destination_conversation_id, destination);
    }

    #[test]
    fn later_rollover_does_not_invalidate_historical_payload_provenance() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );
        proposed_route(
            &mut store,
            RouteId::new(1),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            "handoff-safe",
        )
        .unwrap();

        record_chat_session_lifecycle_transition(
            &mut store,
            ChatContainerId::new(1),
            SessionLifecycleTransition::new(
                SessionId::new(1),
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .unwrap(),
        )
        .unwrap();
        record_local_session_registered(&mut store, SessionId::new(3)).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(
                ChatContainerId::new(1),
                SessionId::new(1),
                SessionId::new(3),
                ContextHandoffId::new(1),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            replay_local_route_payload_audit(store.events())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn route_and_payload_are_one_to_one() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );
        proposed_route(
            &mut store,
            RouteId::new(1),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );
        proposed_route(
            &mut store,
            RouteId::new(2),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );

        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            "one",
        )
        .unwrap();
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(2),
            RouteId::new(1),
            source,
            destination,
            "two",
        )
        .unwrap();
        assert!(
            replay_local_route_payload_audit(store.events())
                .unwrap_err()
                .contains("already has payload")
        );

        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );
        proposed_route(
            &mut store,
            RouteId::new(1),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );
        proposed_route(
            &mut store,
            RouteId::new(2),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::RequireApproval,
        );
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            "one",
        )
        .unwrap();
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(2),
            source,
            destination,
            "two",
        )
        .unwrap();
        assert!(
            replay_local_route_payload_audit(store.events())
                .unwrap_err()
                .contains("already belongs to route")
        );
    }

    #[test]
    fn payload_requires_prior_route_and_cannot_be_attached_after_dispatch() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );

        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            "too early",
        )
        .unwrap();
        assert!(
            replay_local_route_payload_audit(store.events())
                .unwrap_err()
                .contains("before proposal")
        );

        let mut store = MemoryEventStore::default();
        addressable(
            &mut store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            RouteEndpointId::new(1),
        );
        addressable(
            &mut store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            RouteEndpointId::new(2),
        );
        let request = proposed_route(
            &mut store,
            RouteId::new(1),
            RouteEndpointId::new(1),
            RouteEndpointId::new(2),
            RoutePolicy::Allow,
        );
        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(RouteId::new(1)).unwrap();
        record_route_dispatched(&mut store, permit).unwrap();
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            source,
            destination,
            "too late",
        )
        .unwrap();
        assert!(
            replay_local_route_payload_audit(store.events())
                .unwrap_err()
                .contains("after route 1 dispatched")
        );
    }
}
