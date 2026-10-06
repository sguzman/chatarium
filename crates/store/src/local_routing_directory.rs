//! Read-only directory of locally addressable conversation execution leaves.
//!
//! The directory joins already-durable identity edges. It creates no route,
//! permission, payload transfer, worker binding, or controller authority.

use crate::EventEnvelope;
use crate::local_conversation_chat_container_audit::replay_local_conversation_topologies;
use crate::session_audit::replay_session_audit;
use chatarium_core::LocalConversationId;
use chatarium_core::chat_container::{ChatContainerId, SessionLifecyclePhase};
use chatarium_core::routing::RouteEndpointId;
use chatarium_core::session::SessionId;
use std::collections::BTreeMap;

/// One currently addressable local conversation leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalRoutingDirectoryEntry {
    pub conversation_id: LocalConversationId,
    pub container_id: ChatContainerId,
    pub current_session_id: SessionId,
    pub endpoint_id: RouteEndpointId,
    pub current_session_phase: SessionLifecyclePhase,
    pub session_count: usize,
    pub topology_bound_sequence: u64,
    pub endpoint_bound_sequence: u64,
}

/// Reconstruct the current local routing directory from authoritative journal state.
///
/// Only the current session leaf of a local conversation is eligible. A topology
/// without a current-session endpoint is intentionally absent from the returned
/// directory. Endpoint binding alone never creates a route.
pub fn replay_local_routing_directory(
    events: &[EventEnvelope],
) -> Result<Vec<LocalRoutingDirectoryEntry>, String> {
    let sessions = replay_session_audit(events)?
        .into_iter()
        .map(|record| (record.session_id, record))
        .collect::<BTreeMap<_, _>>();

    let mut entries = Vec::new();
    for topology in replay_local_conversation_topologies(events)? {
        let session = sessions.get(&topology.current_session_id).ok_or_else(|| {
            format!(
                "local routing directory cannot resolve current session {} for conversation {}",
                topology.current_session_id.get(),
                topology.conversation_id
            )
        })?;
        let Some(binding) = session.endpoint_binding else {
            continue;
        };
        let endpoint_bound_sequence = session.endpoint_bound_sequence.ok_or_else(|| {
            format!(
                "session {} has endpoint binding without durable binding sequence",
                session.session_id.get()
            )
        })?;

        entries.push(LocalRoutingDirectoryEntry {
            conversation_id: topology.conversation_id,
            container_id: topology.container_id,
            current_session_id: topology.current_session_id,
            endpoint_id: binding.endpoint_id(),
            current_session_phase: topology.current_session_phase,
            session_count: topology.session_count,
            topology_bound_sequence: topology.bound_sequence,
            endpoint_bound_sequence,
        });
    }

    entries.sort_by_key(|entry| entry.topology_bound_sequence);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventStore;
    use crate::MemoryEventStore;
    use crate::chat_container_audit::{
        record_chat_container_created, record_chat_session_lifecycle_transition,
        record_chat_session_successor_bound,
    };
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use chatarium_core::chat_container::{
        ContextHandoffId, SessionLifecycleTransition, SessionSuccessorBinding,
    };
    use chatarium_core::session::SessionEndpointBinding;

    const C1: ChatContainerId = ChatContainerId::new(10);
    const S1: SessionId = SessionId::new(1);
    const S2: SessionId = SessionId::new(2);
    const E1: RouteEndpointId = RouteEndpointId::new(100);
    const E2: RouteEndpointId = RouteEndpointId::new(200);

    fn topology(store: &mut impl EventStore, conversation_id: LocalConversationId) {
        record_local_session_registered(store, S1).unwrap();
        record_chat_container_created(store, C1, S1).unwrap();
        record_local_conversation_chat_container_bound(store, conversation_id, C1).unwrap();
    }

    #[test]
    fn topology_without_current_endpoint_is_not_addressable() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        topology(&mut store, conversation);

        assert!(
            replay_local_routing_directory(store.events())
                .unwrap()
                .is_empty()
        );

        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S1, E1)).unwrap();

        let directory = replay_local_routing_directory(store.events()).unwrap();
        assert_eq!(directory.len(), 1);
        assert_eq!(directory[0].conversation_id, conversation);
        assert_eq!(directory[0].container_id, C1);
        assert_eq!(directory[0].current_session_id, S1);
        assert_eq!(directory[0].endpoint_id, E1);
        assert_eq!(
            directory[0].current_session_phase,
            SessionLifecyclePhase::Healthy
        );
    }

    #[test]
    fn rollover_never_inherits_predecessor_endpoint() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        topology(&mut store, conversation);
        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(S1, E1),
        )
        .unwrap();

        record_chat_session_lifecycle_transition(
            &mut store,
            C1,
            SessionLifecycleTransition::new(
                S1,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .unwrap(),
        )
        .unwrap();
        record_local_session_registered(&mut store, S2).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(C1, S1, S2, ContextHandoffId::new(1)).unwrap(),
        )
        .unwrap();

        assert!(replay_local_routing_directory(store.events()).unwrap().is_empty());

        record_session_endpoint_bound(&mut store, SessionEndpointBinding::new(S2, E2)).unwrap();
        let directory = replay_local_routing_directory(store.events()).unwrap();
        assert_eq!(directory.len(), 1);
        assert_eq!(directory[0].current_session_id, S2);
        assert_eq!(directory[0].endpoint_id, E2);
        assert_eq!(directory[0].session_count, 2);
    }
}
