//! Read-only inbox projection for successfully delivered local session-message routes.
//!
//! The inbox joins immutable route payloads with durable delivery provenance.
//! It creates no transcript message, inference context item, acknowledgement,
//! lifecycle transition, or new journal fact.

use crate::EventEnvelope;
use crate::local_route_delivery_audit::replay_local_route_delivery_audit;
use crate::local_route_payload_audit::replay_local_route_payload_audit;
use chatarium_core::LocalConversationId;
use chatarium_core::routing::{RouteId, RoutePayloadId};
use chatarium_core::session::SessionId;
use std::collections::BTreeMap;

/// One successfully delivered routed message as visible to its destination.
///
/// Text remains the exact immutable payload text. The item is not user-authored
/// transcript content and is not automatically admitted to inference context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRoutedInboxItem {
    pub route_id: RouteId,
    pub payload_id: RoutePayloadId,
    pub source_conversation_id: LocalConversationId,
    pub destination_conversation_id: LocalConversationId,
    pub source_session_id: SessionId,
    pub destination_session_id: SessionId,
    pub text: String,
    pub payload_attached_sequence: u64,
    pub dispatch_sequence: u64,
    pub delivered_sequence: u64,
}

/// Reconstruct all successfully delivered local routed messages.
///
/// Delivery audit is authoritative for successful arrival. Payload audit is
/// authoritative for immutable text. This projection fails closed if those
/// already-validated histories disagree.
pub fn replay_local_routed_inbox(
    events: &[EventEnvelope],
) -> Result<Vec<LocalRoutedInboxItem>, String> {
    let payloads = replay_local_route_payload_audit(events)?
        .into_iter()
        .map(|payload| (payload.route_id, payload))
        .collect::<BTreeMap<_, _>>();

    let mut items = Vec::new();
    for delivery in replay_local_route_delivery_audit(events)? {
        let payload = payloads.get(&delivery.route_id).ok_or_else(|| {
            format!(
                "delivered local route {} has no replayed immutable payload",
                delivery.route_id.get()
            )
        })?;
        if payload.payload_id != delivery.payload_id {
            return Err(format!(
                "delivered local route {} payload identity disagrees between payload and delivery audit",
                delivery.route_id.get()
            ));
        }
        if payload.source_conversation_id != delivery.source_conversation_id
            || payload.destination_conversation_id != delivery.destination_conversation_id
        {
            return Err(format!(
                "delivered local route {} conversation provenance disagrees between payload and delivery audit",
                delivery.route_id.get()
            ));
        }
        if payload.attached_sequence >= delivery.dispatch_sequence {
            return Err(format!(
                "delivered local route {} payload attachment does not precede dispatch",
                delivery.route_id.get()
            ));
        }
        if delivery.dispatch_sequence >= delivery.delivered_sequence {
            return Err(format!(
                "delivered local route {} dispatch does not precede delivery",
                delivery.route_id.get()
            ));
        }

        items.push(LocalRoutedInboxItem {
            route_id: delivery.route_id,
            payload_id: delivery.payload_id,
            source_conversation_id: delivery.source_conversation_id,
            destination_conversation_id: delivery.destination_conversation_id,
            source_session_id: delivery.source_session_id,
            destination_session_id: delivery.destination_session_id,
            text: payload.text.clone(),
            payload_attached_sequence: payload.attached_sequence,
            dispatch_sequence: delivery.dispatch_sequence,
            delivered_sequence: delivery.delivered_sequence,
        });
    }

    items.sort_by_key(|item| item.delivered_sequence);
    Ok(items)
}

/// Reconstruct only routed messages delivered to one local conversation.
pub fn replay_local_routed_inbox_for_conversation(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<LocalRoutedInboxItem>, String> {
    Ok(replay_local_routed_inbox(events)?
        .into_iter()
        .filter(|item| item.destination_conversation_id == conversation_id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, MemoryEventStore};
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

    fn addressable(
        store: &mut impl crate::EventStore,
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

    #[test]
    fn delivered_payload_projects_into_destination_inbox_only() {
        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let unrelated = LocalConversationId::new();
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

        let request = RouteRequest {
            id: RouteId::new(1),
            source: RouteEndpointId::new(1),
            destination: RouteEndpointId::new(2),
            class: RouteClass::SessionMessage,
        };
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(1),
            request.id,
            source,
            destination,
            " exact routed text ",
        )
        .unwrap();

        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(request.id).unwrap();
        let dispatch_sequence = record_route_dispatched(&mut store, permit).unwrap();
        record_local_route_delivered(
            &mut store,
            request.id,
            RoutePayloadId::new(1),
            source,
            destination,
            SessionId::new(1),
            SessionId::new(2),
            dispatch_sequence,
        )
        .unwrap();

        let all = replay_local_routed_inbox(store.events()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, " exact routed text ");
        assert_eq!(all[0].source_conversation_id, source);
        assert_eq!(all[0].destination_conversation_id, destination);

        let destination_inbox =
            replay_local_routed_inbox_for_conversation(store.events(), destination).unwrap();
        assert_eq!(destination_inbox.len(), 1);
        assert_eq!(destination_inbox[0].route_id, RouteId::new(1));

        assert!(
            replay_local_routed_inbox_for_conversation(store.events(), source)
                .unwrap()
                .is_empty()
        );
        assert!(
            replay_local_routed_inbox_for_conversation(store.events(), unrelated)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn payload_dispatch_delivery_sequences_remain_explicit() {
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

        let request = RouteRequest {
            id: RouteId::new(5),
            source: RouteEndpointId::new(1),
            destination: RouteEndpointId::new(2),
            class: RouteClass::SessionMessage,
        };
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        let payload_sequence = record_local_route_payload_attached(
            &mut store,
            RoutePayloadId::new(7),
            request.id,
            source,
            destination,
            "hello",
        )
        .unwrap();
        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(request.id).unwrap();
        let dispatch_sequence = record_route_dispatched(&mut store, permit).unwrap();
        let delivered_sequence = record_local_route_delivered(
            &mut store,
            request.id,
            RoutePayloadId::new(7),
            source,
            destination,
            SessionId::new(1),
            SessionId::new(2),
            dispatch_sequence,
        )
        .unwrap();

        let item = replay_local_routed_inbox(store.events())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(item.payload_attached_sequence, payload_sequence);
        assert_eq!(item.dispatch_sequence, dispatch_sequence);
        assert_eq!(item.delivered_sequence, delivered_sequence);
        assert!(item.payload_attached_sequence < item.dispatch_sequence);
        assert!(item.dispatch_sequence < item.delivered_sequence);
    }
}
