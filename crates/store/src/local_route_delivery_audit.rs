//! Durable local delivery provenance for dispatched session-message routes.
//!
//! Generic route policy/dispatch remains owned by routing_audit. This module
//! records the separate fact that one already-dispatched immutable payload
//! reached its intended current local conversation/session leaf.

use crate::local_route_payload_audit::replay_local_route_payload_audit;
use crate::local_routing_directory::replay_local_routing_directory;
use crate::routing_audit::replay_routing_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::routing::{RouteId, RoutePayloadId};
use chatarium_core::session::SessionId;
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-route-delivery-audit";
const VERSION: u64 = 1;

/// One durable successful delivery of a previously dispatched local route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalRouteDeliveryRecord {
    pub route_id: RouteId,
    pub payload_id: RoutePayloadId,
    pub source_conversation_id: LocalConversationId,
    pub destination_conversation_id: LocalConversationId,
    pub source_session_id: SessionId,
    pub destination_session_id: SessionId,
    pub dispatch_sequence: u64,
    pub delivered_sequence: u64,
}

/// Append one local delivery fact after dispatch authorization has already been
/// durably consumed.
pub fn record_local_route_delivered(
    store: &mut impl EventStore,
    route_id: RouteId,
    payload_id: RoutePayloadId,
    source_conversation_id: LocalConversationId,
    destination_conversation_id: LocalConversationId,
    source_session_id: SessionId,
    destination_session_id: SessionId,
    dispatch_sequence: u64,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(delivery_scope(route_id)),
        EventKind::LocalRouteDelivered,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_route_delivered",
            "route_id": route_id.get(),
            "payload_id": payload_id.get(),
            "source_conversation_id": source_conversation_id.to_string(),
            "destination_conversation_id": destination_conversation_id.to_string(),
            "source_session_id": source_session_id.get(),
            "destination_session_id": destination_session_id.get(),
            "dispatch_sequence": dispatch_sequence,
        }),
    )
}

/// Reconstruct successful local deliveries from authoritative journal history.
///
/// Each delivery is validated against the journal prefix immediately before the
/// delivery event, preserving point-in-time endpoint/session provenance across
/// later rollover.
pub fn replay_local_route_delivery_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalRouteDeliveryRecord>, String> {
    let mut deliveries = BTreeMap::<RouteId, LocalRouteDeliveryRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::LocalRouteDelivered {
            continue;
        }

        let value = typed_payload(event)?;
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let payload_id = RoutePayloadId::new(required_u64(&value, "payload_id")?);
        let source_conversation_id =
            parse_conversation_id(&value, "source_conversation_id", event.sequence)?;
        let destination_conversation_id =
            parse_conversation_id(&value, "destination_conversation_id", event.sequence)?;
        let source_session_id = SessionId::new(required_u64(&value, "source_session_id")?);
        let destination_session_id =
            SessionId::new(required_u64(&value, "destination_session_id")?);
        let dispatch_sequence = required_u64(&value, "dispatch_sequence")?;
        validate_scope(event, route_id)?;

        if deliveries.contains_key(&route_id) {
            return Err(format!(
                "route {} has more than one local delivery event; duplicate at sequence {}",
                route_id.get(),
                event.sequence
            ));
        }
        if source_conversation_id == destination_conversation_id {
            return Err(format!(
                "local route delivery {} at sequence {} names the same conversation on both ends",
                route_id.get(),
                event.sequence
            ));
        }
        if source_session_id == destination_session_id {
            return Err(format!(
                "local route delivery {} at sequence {} names the same session on both ends",
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
                    "local delivery at sequence {} references unknown route {}",
                    event.sequence,
                    route_id.get()
                )
            })?;
        let actual_dispatch_sequence = route.dispatch_sequence.ok_or_else(|| {
            format!(
                "local delivery for route {} at sequence {} exists before durable dispatch",
                route_id.get(),
                event.sequence
            )
        })?;
        if actual_dispatch_sequence != dispatch_sequence {
            return Err(format!(
                "local delivery for route {} at sequence {} claims dispatch sequence {}, actual is {}",
                route_id.get(),
                event.sequence,
                dispatch_sequence,
                actual_dispatch_sequence
            ));
        }
        if dispatch_sequence >= event.sequence {
            return Err(format!(
                "local delivery for route {} at sequence {} does not follow dispatch sequence {}",
                route_id.get(),
                event.sequence,
                dispatch_sequence
            ));
        }

        let payload = replay_local_route_payload_audit(prior)?
            .into_iter()
            .find(|payload| payload.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "local delivery for route {} at sequence {} has no durable payload",
                    route_id.get(),
                    event.sequence
                )
            })?;
        if payload.payload_id != payload_id {
            return Err(format!(
                "local delivery for route {} references payload {}, durable route payload is {}",
                route_id.get(),
                payload_id.get(),
                payload.payload_id.get()
            ));
        }
        if payload.source_conversation_id != source_conversation_id
            || payload.destination_conversation_id != destination_conversation_id
        {
            return Err(format!(
                "local delivery for route {} disagrees with durable payload conversation provenance",
                route_id.get()
            ));
        }

        let directory = replay_local_routing_directory(prior)?;
        let source = directory
            .iter()
            .find(|entry| entry.endpoint_id == route.request.source)
            .ok_or_else(|| {
                format!(
                    "local delivery for route {} cannot resolve source endpoint {} at delivery time",
                    route_id.get(),
                    route.request.source.get()
                )
            })?;
        let destination = directory
            .iter()
            .find(|entry| entry.endpoint_id == route.request.destination)
            .ok_or_else(|| {
                format!(
                    "local delivery for route {} cannot resolve destination endpoint {} at delivery time",
                    route_id.get(),
                    route.request.destination.get()
                )
            })?;

        if source.conversation_id != source_conversation_id
            || destination.conversation_id != destination_conversation_id
        {
            return Err(format!(
                "local delivery for route {} disagrees with endpoint conversation ownership at delivery time",
                route_id.get()
            ));
        }
        if source.current_session_id != source_session_id
            || destination.current_session_id != destination_session_id
        {
            return Err(format!(
                "local delivery for route {} disagrees with current session leaves at delivery time",
                route_id.get()
            ));
        }
        if !source.current_session_phase.accepts_ordinary_turns()
            || !destination.current_session_phase.accepts_ordinary_turns()
        {
            return Err(format!(
                "local delivery for route {} occurred while one endpoint could not accept ordinary turns",
                route_id.get()
            ));
        }

        deliveries.insert(
            route_id,
            LocalRouteDeliveryRecord {
                route_id,
                payload_id,
                source_conversation_id,
                destination_conversation_id,
                source_session_id,
                destination_session_id,
                dispatch_sequence,
                delivered_sequence: event.sequence,
            },
        );
    }

    let mut records = deliveries.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.delivered_sequence);
    Ok(records)
}

/// Stable scope for one local route delivery.
#[must_use]
pub fn delivery_scope(route_id: RouteId) -> String {
    format!("local-route-delivery:{}", route_id.get())
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
            "malformed local route delivery payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local route delivery event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local route delivery event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_route_delivered") {
        return Err(format!(
            "local route delivery event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, route_id: RouteId) -> Result<(), String> {
    let expected = delivery_scope(route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local route delivery event at sequence {} has scope {:?}, expected {:?}",
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
        format!("local route delivery event at sequence {sequence} has invalid {field}: {error}")
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed local route delivery is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed local route delivery is missing string field '{field}'"))
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
    use crate::local_route_payload_audit::record_local_route_payload_attached;
    use crate::routing_audit::{record_route_dispatched, record_route_proposed};
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use chatarium_core::chat_container::{
        ChatContainerId, ContextHandoffId, SessionLifecyclePhase, SessionLifecycleTransition,
        SessionSuccessorBinding,
    };
    use chatarium_core::routing::{
        RouteClass, RouteEndpointId, RouteGate, RoutePolicy, RouteRequest,
    };
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

    fn routed_pair(
        store: &mut impl EventStore,
    ) -> (
        LocalConversationId,
        LocalConversationId,
        RouteRequest,
        RoutePayloadId,
    ) {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        addressable(
            store,
            source,
            ChatContainerId::new(1),
            SessionId::new(1),
            chatarium_core::routing::RouteEndpointId::new(1),
        );
        addressable(
            store,
            destination,
            ChatContainerId::new(2),
            SessionId::new(2),
            chatarium_core::routing::RouteEndpointId::new(2),
        );
        let request = RouteRequest {
            id: RouteId::new(1),
            source: chatarium_core::routing::RouteEndpointId::new(1),
            destination: chatarium_core::routing::RouteEndpointId::new(2),
            class: RouteClass::SessionMessage,
        };
        record_route_proposed(store, request, RoutePolicy::Allow).unwrap();
        let payload_id = RoutePayloadId::new(1);
        record_local_route_payload_attached(
            store,
            payload_id,
            request.id,
            source,
            destination,
            "deliver me",
        )
        .unwrap();
        (source, destination, request, payload_id)
    }

    fn dispatch(store: &mut impl EventStore, request: RouteRequest) -> u64 {
        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(request.id).unwrap();
        record_route_dispatched(store, permit).unwrap()
    }

    #[test]
    fn delivery_requires_durable_dispatch_and_payload() {
        let mut store = MemoryEventStore::default();
        let (source, destination, request, payload_id) = routed_pair(&mut store);

        record_local_route_delivered(
            &mut store,
            request.id,
            payload_id,
            source,
            destination,
            SessionId::new(1),
            SessionId::new(2),
            999,
        )
        .unwrap();
        assert!(
            replay_local_route_delivery_audit(store.events())
                .unwrap_err()
                .contains("before durable dispatch")
        );

        let mut store = MemoryEventStore::default();
        let (source, destination, request, payload_id) = routed_pair(&mut store);
        let dispatch_sequence = dispatch(&mut store, request);
        record_local_route_delivered(
            &mut store,
            request.id,
            payload_id,
            source,
            destination,
            SessionId::new(1),
            SessionId::new(2),
            dispatch_sequence,
        )
        .unwrap();

        let deliveries = replay_local_route_delivery_audit(store.events()).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].route_id, request.id);
        assert_eq!(deliveries[0].payload_id, payload_id);
        assert_eq!(deliveries[0].dispatch_sequence, dispatch_sequence);
    }

    #[test]
    fn duplicate_delivery_is_rejected() {
        let mut store = MemoryEventStore::default();
        let (source, destination, request, payload_id) = routed_pair(&mut store);
        let dispatch_sequence = dispatch(&mut store, request);

        for _ in 0..2 {
            record_local_route_delivered(
                &mut store,
                request.id,
                payload_id,
                source,
                destination,
                SessionId::new(1),
                SessionId::new(2),
                dispatch_sequence,
            )
            .unwrap();
        }

        assert!(
            replay_local_route_delivery_audit(store.events())
                .unwrap_err()
                .contains("more than one local delivery")
        );
    }

    #[test]
    fn later_rollover_does_not_invalidate_completed_delivery() {
        let mut store = MemoryEventStore::default();
        let (source, destination, request, payload_id) = routed_pair(&mut store);
        let dispatch_sequence = dispatch(&mut store, request);
        record_local_route_delivered(
            &mut store,
            request.id,
            payload_id,
            source,
            destination,
            SessionId::new(1),
            SessionId::new(2),
            dispatch_sequence,
        )
        .unwrap();

        record_chat_session_lifecycle_transition(
            &mut store,
            ChatContainerId::new(2),
            SessionLifecycleTransition::new(
                SessionId::new(2),
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
                ChatContainerId::new(2),
                SessionId::new(2),
                SessionId::new(3),
                ContextHandoffId::new(1),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            replay_local_route_delivery_audit(store.events())
                .unwrap()
                .len(),
            1
        );
    }
}
