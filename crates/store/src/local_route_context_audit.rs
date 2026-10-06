//! Durable user decisions controlling whether one delivered routed message is eligible for context.
//!
//! Delivery and context use are separate. This audit never mutates payload text,
//! transcript history, or Context Composer directly.

use crate::EventEnvelope;
use crate::EventStore;
use crate::local_routed_inbox::replay_local_routed_inbox;
use chatarium_core::EventKind;
use chatarium_core::LocalConversationId;
use chatarium_core::routing::{RouteId, RoutePayloadId};
use chatarium_core::session::SessionId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-route-context-audit";
const VERSION: u64 = 1;

/// Explicit user decision for one delivered routed inbox item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRouteContextDecision {
    Admit,
    Exclude,
}

/// Current replayed context eligibility for one delivered routed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalRouteContextRecord {
    pub route_id: RouteId,
    pub payload_id: RoutePayloadId,
    pub source_conversation_id: LocalConversationId,
    pub destination_conversation_id: LocalConversationId,
    pub source_session_id: SessionId,
    pub destination_session_id: SessionId,
    pub delivered_sequence: u64,
    pub decision: LocalRouteContextDecision,
    pub first_decision_sequence: u64,
    pub last_decision_sequence: u64,
}

impl LocalRouteContextRecord {
    #[must_use]
    pub const fn is_admitted(self) -> bool {
        matches!(self.decision, LocalRouteContextDecision::Admit)
    }
}

/// Append one explicit context eligibility decision.
///
/// The event references an already-delivered route/payload. Text is not duplicated.
pub fn record_local_route_context_decision(
    store: &mut impl EventStore,
    route_id: RouteId,
    payload_id: RoutePayloadId,
    destination_conversation_id: LocalConversationId,
    decision: LocalRouteContextDecision,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "local_route_context_decision",
        "route_id": route_id.get(),
        "payload_id": payload_id.get(),
        "destination_conversation_id": destination_conversation_id.to_string(),
        "decision": decision_name(decision),
    });
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(context_scope(destination_conversation_id, route_id)),
        EventKind::LocalRouteContextDecisionRecorded,
        encoded,
    )
}

/// Reconstruct current user context decisions from authoritative journal history.
///
/// Each decision is validated against the routed inbox as it existed immediately
/// before that decision event. Later rollover cannot rewrite historical delivery
/// provenance.
pub fn replay_local_route_context_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalRouteContextRecord>, String> {
    let mut records = BTreeMap::<RouteId, LocalRouteContextRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::LocalRouteContextDecisionRecorded {
            continue;
        }

        let value = typed_payload(event)?;
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let payload_id = RoutePayloadId::new(required_u64(&value, "payload_id")?);
        let destination_conversation_id = LocalConversationId::from_str(required_string(
            &value,
            "destination_conversation_id",
        )?)
        .map_err(|error| {
            format!(
                "local route context decision at sequence {} has invalid destination conversation id: {error}",
                event.sequence
            )
        })?;
        let decision = parse_decision(required_string(&value, "decision")?)?;
        validate_scope(event, destination_conversation_id, route_id)?;

        let prior = &events[..index];
        let inbox_item = replay_local_routed_inbox(prior)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "local route context decision at sequence {} references route {} before successful delivery",
                    event.sequence,
                    route_id.get()
                )
            })?;

        if inbox_item.payload_id != payload_id {
            return Err(format!(
                "local route context decision at sequence {} references payload {}, delivered route {} uses payload {}",
                event.sequence,
                payload_id.get(),
                route_id.get(),
                inbox_item.payload_id.get()
            ));
        }
        if inbox_item.destination_conversation_id != destination_conversation_id {
            return Err(format!(
                "local route context decision at sequence {} targets conversation {}, but route {} was delivered to {}",
                event.sequence,
                destination_conversation_id,
                route_id.get(),
                inbox_item.destination_conversation_id
            ));
        }

        match records.get_mut(&route_id) {
            Some(record) => {
                if record.payload_id != payload_id
                    || record.destination_conversation_id != destination_conversation_id
                {
                    return Err(format!(
                        "local route context decision at sequence {} conflicts with earlier route {} provenance",
                        event.sequence,
                        route_id.get()
                    ));
                }
                record.decision = decision;
                record.last_decision_sequence = event.sequence;
            }
            None => {
                records.insert(
                    route_id,
                    LocalRouteContextRecord {
                        route_id,
                        payload_id,
                        source_conversation_id: inbox_item.source_conversation_id,
                        destination_conversation_id,
                        source_session_id: inbox_item.source_session_id,
                        destination_session_id: inbox_item.destination_session_id,
                        delivered_sequence: inbox_item.delivered_sequence,
                        decision,
                        first_decision_sequence: event.sequence,
                        last_decision_sequence: event.sequence,
                    },
                );
            }
        }
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.first_decision_sequence);
    Ok(values)
}

/// Replay only currently-admitted routed context for one destination conversation.
pub fn replay_admitted_local_route_context(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<LocalRouteContextRecord>, String> {
    Ok(replay_local_route_context_audit(events)?
        .into_iter()
        .filter(|record| {
            record.destination_conversation_id == conversation_id && record.is_admitted()
        })
        .collect())
}

#[must_use]
pub fn context_scope(
    destination_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> String {
    format!(
        "local-route-context:{destination_conversation_id}:{}",
        route_id.get()
    )
}

const fn decision_name(decision: LocalRouteContextDecision) -> &'static str {
    match decision {
        LocalRouteContextDecision::Admit => "admit",
        LocalRouteContextDecision::Exclude => "exclude",
    }
}

fn parse_decision(value: &str) -> Result<LocalRouteContextDecision, String> {
    match value {
        "admit" => Ok(LocalRouteContextDecision::Admit),
        "exclude" => Ok(LocalRouteContextDecision::Exclude),
        _ => Err(format!(
            "unsupported local route context decision '{value}'"
        )),
    }
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed local route context decision at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local route context decision at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local route context decision at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_route_context_decision") {
        return Err(format!(
            "local route context decision at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    destination_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<(), String> {
    let expected = context_scope(destination_conversation_id, route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local route context decision at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        format!("typed local route context decision is missing integer field '{field}'")
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed local route context decision is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::chat_container_audit::record_chat_container_created;
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::local_route_delivery_audit::record_local_route_delivered;
    use crate::local_route_payload_audit::record_local_route_payload_attached;
    use crate::routing_audit::{record_route_dispatched, record_route_proposed};
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use chatarium_core::chat_container::ChatContainerId;
    use chatarium_core::routing::{
        RouteClass, RouteEndpointId, RouteGate, RoutePolicy, RouteRequest,
    };
    use chatarium_core::session::SessionEndpointBinding;

    fn delivered_route(
        store: &mut impl EventStore,
    ) -> (
        LocalConversationId,
        LocalConversationId,
        RouteId,
        RoutePayloadId,
    ) {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let source_session = SessionId::new(1);
        let destination_session = SessionId::new(2);
        let source_endpoint = RouteEndpointId::new(1);
        let destination_endpoint = RouteEndpointId::new(2);

        for (conversation, container, session, endpoint) in [
            (
                source,
                ChatContainerId::new(1),
                source_session,
                source_endpoint,
            ),
            (
                destination,
                ChatContainerId::new(2),
                destination_session,
                destination_endpoint,
            ),
        ] {
            record_local_session_registered(store, session).unwrap();
            record_chat_container_created(store, container, session).unwrap();
            record_local_conversation_chat_container_bound(store, conversation, container).unwrap();
            record_session_endpoint_bound(store, SessionEndpointBinding::new(session, endpoint))
                .unwrap();
        }

        let route_id = RouteId::new(1);
        let payload_id = RoutePayloadId::new(1);
        let request = RouteRequest {
            id: route_id,
            source: source_endpoint,
            destination: destination_endpoint,
            class: RouteClass::SessionMessage,
        };
        record_route_proposed(store, request, RoutePolicy::Allow).unwrap();
        record_local_route_payload_attached(
            store,
            payload_id,
            route_id,
            source,
            destination,
            "routed text",
        )
        .unwrap();
        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(route_id).unwrap();
        let dispatch_sequence = record_route_dispatched(store, permit).unwrap();
        record_local_route_delivered(
            store,
            route_id,
            payload_id,
            source,
            destination,
            source_session,
            destination_session,
            dispatch_sequence,
        )
        .unwrap();

        (source, destination, route_id, payload_id)
    }

    #[test]
    fn admit_and_exclude_are_reversible_without_changing_delivery() {
        let mut store = MemoryEventStore::default();
        let (_, destination, route_id, payload_id) = delivered_route(&mut store);

        record_local_route_context_decision(
            &mut store,
            route_id,
            payload_id,
            destination,
            LocalRouteContextDecision::Admit,
        )
        .unwrap();
        let admitted = replay_admitted_local_route_context(store.events(), destination).unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].route_id, route_id);

        record_local_route_context_decision(
            &mut store,
            route_id,
            payload_id,
            destination,
            LocalRouteContextDecision::Exclude,
        )
        .unwrap();
        assert!(
            replay_admitted_local_route_context(store.events(), destination)
                .unwrap()
                .is_empty()
        );
        let records = replay_local_route_context_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].decision, LocalRouteContextDecision::Exclude);
        assert!(records[0].first_decision_sequence < records[0].last_decision_sequence);
    }

    #[test]
    fn decision_requires_prior_delivery_and_correct_destination() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let mut store = MemoryEventStore::default();

        record_local_route_context_decision(
            &mut store,
            RouteId::new(1),
            RoutePayloadId::new(1),
            destination,
            LocalRouteContextDecision::Admit,
        )
        .unwrap();
        assert!(
            replay_local_route_context_audit(store.events())
                .unwrap_err()
                .contains("before successful delivery")
        );

        let mut store = MemoryEventStore::default();
        let (_, actual_destination, route_id, payload_id) = delivered_route(&mut store);
        record_local_route_context_decision(
            &mut store,
            route_id,
            payload_id,
            source,
            LocalRouteContextDecision::Admit,
        )
        .unwrap();
        let error = replay_local_route_context_audit(store.events()).unwrap_err();
        assert!(error.contains("was delivered to"));
        assert_ne!(source, actual_destination);
    }

    #[test]
    fn payload_identity_must_match_delivery() {
        let mut store = MemoryEventStore::default();
        let (_, destination, route_id, _) = delivered_route(&mut store);

        record_local_route_context_decision(
            &mut store,
            route_id,
            RoutePayloadId::new(99),
            destination,
            LocalRouteContextDecision::Admit,
        )
        .unwrap();
        assert!(
            replay_local_route_context_audit(store.events())
                .unwrap_err()
                .contains("uses payload")
        );
    }
}
