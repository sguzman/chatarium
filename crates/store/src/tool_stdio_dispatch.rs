//! Durable reservation of user-approved external MCP stdio calls.
//!
//! A reservation records one RouteDispatched event *before* any possible
//! external side effect. No subprocess is spawned here. A crash after this
//! durable boundary must remain unresolved and must never auto-retry a call.

use crate::routing_audit::{RouteUserDecision, record_route_dispatched, replay_routing_audit};
use crate::tool_outcome_audit::replay_tool_call_outcome_audit;
use crate::tool_stdio_executable_inspection::inspect_stdio_executable;
use crate::tool_stdio_preflight::{
    ActivatedStdioToolInvocationPreview, preview_activated_stdio_tool_invocation,
};
use crate::tool_transport_config_audit::replay_tool_transport_config_audit;
use crate::EventStore;
use chatarium_core::routing::{
    DecisionAuthority, RouteGate, RouteGateState, RoutePolicy,
};
use chatarium_core::tool::ToolCallId;

/// Move-only proof that the one-shot route authority was durably consumed.
/// This is not proof of a launched process, a completed operation, or a
/// sandboxed executable. Never retry it after crash or interruption.
#[derive(Debug, PartialEq, Eq)]
pub struct ReservedStdioToolDispatch {
    invocation: ActivatedStdioToolInvocationPreview,
    dispatch_sequence: u64,
}

impl ReservedStdioToolDispatch {
    #[must_use]
    pub fn invocation(&self) -> &ActivatedStdioToolInvocationPreview {
        &self.invocation
    }

    #[must_use]
    pub const fn dispatch_sequence(&self) -> u64 {
        self.dispatch_sequence
    }
}

/// Revalidate current activation, immutable call, source session, endpoint,
/// exact MCP frame, executable metadata and user-approved RouteGate. Then
/// durably consume exactly one dispatch permit. Only a future separately
/// hardened runner may accept this move-only reservation.
///
/// Metadata inspection is an advisory preflight and does not close filesystem
/// replacement races. A runner still needs its own race-aware launch policy.
pub fn reserve_activated_stdio_tool_dispatch(
    store: &mut impl EventStore,
    call_id: ToolCallId,
) -> Result<ReservedStdioToolDispatch, String> {
    let invocation = preview_activated_stdio_tool_invocation(store.events(), call_id)?;
    let configured = replay_tool_transport_config_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == invocation.invocation.provider_id)
        .ok_or_else(|| "external provider configuration disappeared".to_owned())?;
    if configured.configured_sequence != invocation.invocation.configured_sequence {
        return Err("external provider configuration changed during reservation".to_owned());
    }
    inspect_stdio_executable(&configured.config).map_err(|error| {
        format!("Linux stdio executable failed reservation inspection: {error:?}")
    })?;

    let route_id = invocation.invocation.route_id;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| "approved external tool route disappeared".to_owned())?;
    if route.initial_policy != RoutePolicy::RequireApproval
        || route.latest_user_decision != Some(RouteUserDecision::Allow)
        || route.gate_state
            != (RouteGateState::Allowed { by: DecisionAuthority::User })
    {
        return Err("tool route no longer has unconsumed user approval".to_owned());
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|record| record.call_id == call_id || record.route_id == route_id)
    {
        return Err("tool call already has a terminal outcome".to_owned());
    }

    let mut gate = RouteGate::new(route.request, route.initial_policy);
    gate.user_allow().map_err(|error| {
        format!("cannot reconstruct approved tool route: {error:?}")
    })?;
    let permit = gate.authorize_dispatch(route_id).map_err(|error| {
        format!("cannot consume approved tool route: {error:?}")
    })?;
    let next_sequence = u64::try_from(store.events().len())
        .map_err(|error| error.to_string())?
        .checked_add(1)
        .ok_or_else(|| "stdio dispatch sequence exhausted".to_owned())?;
    record_route_dispatched(store, permit).map_err(|error| error.to_string())?;
    let recorded = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| "reserved tool route did not replay".to_owned())?;
    if recorded.dispatch_sequence != Some(next_sequence)
        || !recorded.gate_state.is_dispatched()
    {
        return Err("reserved tool route dispatch replay disagrees with journal".to_owned());
    }
    Ok(ReservedStdioToolDispatch {
        invocation,
        dispatch_sequence: next_sequence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::chat_container_audit::record_chat_container_created;
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::routing_audit::{
        record_route_proposed, record_route_user_decision,
    };
    use crate::session_audit::{
        record_local_session_registered, record_session_endpoint_bound,
    };
    use crate::tool_call_audit::{
        record_tool_call, record_tool_call_route_bound,
    };
    use crate::tool_provider_activation_audit::{
        ProviderActivationDecision, append_tool_provider_activation_decision_checked,
    };
    use crate::tool_provider_audit::{
        record_tool_provider_registered, record_tool_provider_endpoint_bound,
    };
    use crate::tool_transport_config_audit::append_tool_transport_config_checked;
    use chatarium_core::LocalConversationId;
    use chatarium_core::chat_container::ChatContainerId;
    use chatarium_core::routing::{
        RouteClass, RouteEndpointId, RouteId, RouteRequest,
    };
    use chatarium_core::session::{SessionEndpointBinding, SessionId};
    use chatarium_core::tool::{
        StdioToolProviderConfig, ToolOperationName, ToolProviderEndpointBinding,
        ToolProviderId, ToolProviderName,
    };

    const SESSION: SessionId = SessionId::new(1);
    const SOURCE_ENDPOINT: RouteEndpointId = RouteEndpointId::new(11);
    const DEST_ENDPOINT: RouteEndpointId = RouteEndpointId::new(22);
    const PROVIDER: ToolProviderId = ToolProviderId::new(3);
    const CALL: ToolCallId = ToolCallId::new(4);
    const ROUTE: RouteId = RouteId::new(5);

    fn fixture(executable: &str, approved: bool) -> MemoryEventStore {
        let mut store = MemoryEventStore::default();
        record_local_session_registered(&mut store, SESSION).unwrap();
        record_chat_container_created(&mut store, ChatContainerId::new(1), SESSION).unwrap();
        record_local_conversation_chat_container_bound(
            &mut store, LocalConversationId::new(), ChatContainerId::new(1),
        ).unwrap();
        record_session_endpoint_bound(
            &mut store, SessionEndpointBinding::new(SESSION, SOURCE_ENDPOINT),
        ).unwrap();
        record_tool_provider_registered(
            &mut store, PROVIDER, &ToolProviderName::new("external").unwrap(),
        ).unwrap();
        record_tool_provider_endpoint_bound(
            &mut store, ToolProviderEndpointBinding::new(PROVIDER, DEST_ENDPOINT),
        ).unwrap();
        let config = StdioToolProviderConfig::new(
            executable, Vec::new(), vec![ToolOperationName::new("hello").unwrap()],
        ).unwrap();
        let config_event =
            append_tool_transport_config_checked(&mut store, PROVIDER, &config).unwrap();
        append_tool_provider_activation_decision_checked(
            &mut store, PROVIDER, config_event.sequence, ProviderActivationDecision::Activate,
        ).unwrap();
        record_tool_call(
            &mut store, CALL, SESSION, PROVIDER,
            &ToolOperationName::new("hello").unwrap(), "{}",
        ).unwrap();
        record_route_proposed(
            &mut store,
            RouteRequest {
                id: ROUTE,
                source: SOURCE_ENDPOINT,
                destination: DEST_ENDPOINT,
                class: RouteClass::ToolCall,
            },
            RoutePolicy::RequireApproval,
        ).unwrap();
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();
        if approved {
            record_route_user_decision(
                &mut store, ROUTE, RouteUserDecision::Allow,
            ).unwrap();
        }
        store
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dispatched_reservation_is_durable_and_cannot_be_reused_after_crash() {
        let mut store = fixture("/usr/bin/true", true);
        let before = store.events().len();
        let reserved = reserve_activated_stdio_tool_dispatch(&mut store, CALL).unwrap();
        assert_eq!(reserved.invocation().invocation.call_id, CALL);
        assert_eq!(reserved.invocation().invocation.route_id, ROUTE);
        assert_eq!(reserved.dispatch_sequence(), before as u64 + 1);
        assert_eq!(store.events().len(), before + 1);
        assert_eq!(
            store.events().last().unwrap().kind,
            chatarium_core::EventKind::RouteDispatched,
        );
        // No outcome was invented merely because authority was consumed.
        assert!(replay_tool_call_outcome_audit(store.events()).unwrap().is_empty());
        let snapshot = store.events().to_vec();
        assert!(reserve_activated_stdio_tool_dispatch(&mut store, CALL).is_err());
        assert_eq!(store.events(), snapshot.as_slice());
    }

    #[test]
    fn inactive_or_unapproved_routes_never_consume_a_permit() {
        let mut store = fixture("/usr/bin/true", false);
        let before = store.events().len();
        assert!(reserve_activated_stdio_tool_dispatch(&mut store, CALL).is_err());
        assert_eq!(store.events().len(), before);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_executable_does_not_consume_approval() {
        let mut store = fixture(
            "/chatarium-nonexistent-stdio-path-937242/not-found", true,
        );
        let before = store.events().len();
        assert!(reserve_activated_stdio_tool_dispatch(&mut store, CALL)
            .unwrap_err().contains("inspection"));
        assert_eq!(store.events().len(), before);
    }
}
