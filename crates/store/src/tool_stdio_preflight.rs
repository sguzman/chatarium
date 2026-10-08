//! Inert pre-dispatch validation of external stdio MCP tool calls.
//!
//! This prepares *only* an inspectable wire request. It never starts a process,
//! consumes a DispatchPermit, records delivery, or grants permission. A future
//! executable adapter must independently validate activation and recheck these
//! journal conditions immediately before its one-shot dispatch boundary.

use crate::EventEnvelope;
use crate::local_routing_directory::replay_local_routing_directory;
use crate::routing_audit::{RouteUserDecision, replay_routing_audit};
use crate::session_audit::replay_session_audit;
use crate::tool_call_audit::replay_tool_call_audit;
use crate::tool_provider_activation_audit::replay_tool_provider_activation_audit;
use crate::tool_provider_audit::replay_tool_provider_audit;
use crate::tool_transport_config_audit::replay_tool_transport_config_audit;
use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteGateState, RouteId, RoutePolicy,
};
use chatarium_core::session::SessionId;
use chatarium_core::tool::{ToolCallId, ToolProviderId};
use chatarium_protocol::mcp_wire::{encode_stdio_frame, tools_call_request};
use chatarium_protocol::tool_envelope::parse_legacy_tool_call;
use serde_json::Value;

/// A zero-side-effect preview of a single future MCP tools/call request.
///
/// A successfully prepared preview is NOT authority to execute. Its route may
/// be denied or become stale immediately afterward. Never cache a preview as
/// a dispatch permit, subprocess handle, or tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioToolInvocationPreview {
    pub call_id: ToolCallId,
    pub route_id: RouteId,
    pub provider_id: ToolProviderId,
    pub source_session_id: SessionId,
    pub executable: String,
    pub argv: Vec<String>,
    pub operation: String,
    pub configured_sequence: u64,
    pub approved_sequence: u64,
    /// One bounded, line-delimited MCP 2026-07-28 JSON-RPC request.
    pub request_frame: String,
}

/// Revalidate the entire currently-approved stdio call before any execution.
///
/// The source session must be the current, ordinary-turn-capable leaf of a
/// local conversation. The provider must have a distinct endpoint, and the
/// transport config must predate the immutable call. Only an explicit user
/// Allow under RequireApproval is accepted. Dispatch must remain unconsumed.
pub fn preview_stdio_tool_invocation(
    events: &[EventEnvelope],
    call_id: ToolCallId,
) -> Result<StdioToolInvocationPreview, String> {
    let call = replay_tool_call_audit(events)?
        .into_iter()
        .find(|record| record.call_id == call_id)
        .ok_or_else(|| format!("missing immutable tool call {}", call_id.get()))?;
    let route_id = call
        .route_id
        .ok_or_else(|| format!("tool call {} has no correlated route", call_id.get()))?;

    let route = replay_routing_audit(events)?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("missing tool route {}", route_id.get()))?;
    if route.request.class != RouteClass::ToolCall
        || route.initial_policy != RoutePolicy::RequireApproval
        || route.latest_user_decision != Some(RouteUserDecision::Allow)
        || route.gate_state
            != (RouteGateState::Allowed {
                by: DecisionAuthority::User,
            })
        || route.dispatch_sequence.is_some()
    {
        return Err(format!(
            "tool route {} is not explicitly user-approved and undispatched",
            route_id.get()
        ));
    }

    let approved_sequence = route.last_sequence;
    if call
        .route_bound_sequence
        .is_none_or(|sequence| sequence >= approved_sequence)
    {
        return Err(format!(
            "tool call {} route correlation does not predate approval",
            call_id.get()
        ));
    }

    let source = replay_local_routing_directory(events)?
        .into_iter()
        .find(|entry| entry.current_session_id == call.source_session_id)
        .ok_or_else(|| {
            format!(
                "tool call {} source session {} is not a current local conversation leaf",
                call_id.get(),
                call.source_session_id.get()
            )
        })?;
    if !source.current_session_phase.accepts_ordinary_turns()
        || source.endpoint_id != route.request.source
    {
        return Err(format!(
            "tool route {} source endpoint/phase is stale",
            route_id.get()
        ));
    }
    let session = replay_session_audit(events)?
        .into_iter()
        .find(|record| record.session_id == call.source_session_id)
        .ok_or_else(|| "tool source session disappeared".to_owned())?;
    if session
        .endpoint_binding
        .is_none_or(|binding| binding.endpoint_id() != route.request.source)
    {
        return Err("tool source session routing endpoint changed".to_owned());
    }

    let provider = replay_tool_provider_audit(events)?
        .into_iter()
        .find(|record| record.provider_id == call.provider_id)
        .ok_or_else(|| format!("missing provider {}", call.provider_id.get()))?;
    if provider
        .endpoint_binding
        .is_none_or(|binding| binding.endpoint_id() != route.request.destination)
    {
        return Err(format!(
            "tool route {} destination does not match provider {}",
            route_id.get(),
            call.provider_id.get()
        ));
    }

    let configured = replay_tool_transport_config_audit(events)?
        .into_iter()
        .find(|record| record.provider_id == call.provider_id)
        .ok_or_else(|| {
            format!(
                "tool provider {} has no external stdio configuration",
                call.provider_id.get()
            )
        })?;
    if configured.configured_sequence >= call.recorded_sequence {
        return Err(format!(
            "tool provider {} transport configuration does not predate call {}",
            call.provider_id.get(),
            call_id.get()
        ));
    }
    if !configured.config.allows(&call.operation) {
        return Err(format!(
            "operation {} is not on provider {} stdio allowlist",
            call.operation,
            call.provider_id.get()
        ));
    }

    let args = parse_exact_arguments(&call.arguments_text, call.operation.as_str())?;
    let request =
        tools_call_request(call_id.get(), call.operation.as_str(), &args).map_err(|error| {
            format!(
                "tool call {} cannot encode MCP request: {error:?}",
                call_id.get()
            )
        })?;
    let request_frame = encode_stdio_frame(&request).map_err(|error| {
        format!(
            "tool call {} cannot frame MCP request: {error:?}",
            call_id.get()
        )
    })?;

    Ok(StdioToolInvocationPreview {
        call_id,
        route_id,
        provider_id: call.provider_id,
        source_session_id: call.source_session_id,
        executable: configured.config.executable().to_owned(),
        argv: configured.config.args().to_vec(),
        operation: call.operation.as_str().to_owned(),
        configured_sequence: configured.configured_sequence,
        approved_sequence,
        request_frame,
    })
}

/// A validated activation gate in addition to an inert request preview.
/// This is still not a permit to execute a process or dispatch a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivatedStdioToolInvocationPreview {
    pub invocation: StdioToolInvocationPreview,
    pub activation_sequence: u64,
}

/// A future runner must recheck this immediately before dispatch and must
/// independently enforce race-aware executable validation and the one-shot
/// route permit. Re-activation never revives a call recorded before it.
pub fn preview_activated_stdio_tool_invocation(
    events: &[EventEnvelope],
    call_id: ToolCallId,
) -> Result<ActivatedStdioToolInvocationPreview, String> {
    let invocation = preview_stdio_tool_invocation(events, call_id)?;
    let activation = replay_tool_provider_activation_audit(events)?
        .into_iter()
        .find(|record| record.provider_id == invocation.provider_id)
        .ok_or_else(|| "external tool provider has no explicit activation decision".to_owned())?;
    if !activation.active {
        return Err("external tool provider is deactivated".to_owned());
    }
    if activation.configured_sequence != invocation.configured_sequence {
        return Err(
            "external tool provider activation is for a different configuration".to_owned(),
        );
    }
    let call = replay_tool_call_audit(events)?
        .into_iter()
        .find(|record| record.call_id == call_id)
        .ok_or_else(|| "activated tool call disappeared".to_owned())?;
    let activation_sequence = activation
        .activated_sequence
        .ok_or_else(|| "external tool provider has no active activation sequence".to_owned())?;
    if activation_sequence >= call.recorded_sequence {
        return Err("external tool provider was not active when this call was recorded".to_owned());
    }
    Ok(ActivatedStdioToolInvocationPreview {
        invocation,
        activation_sequence,
    })
}

fn parse_exact_arguments(text: &str, operation: &str) -> Result<Value, String> {
    // Only an explicitly bounded legacy envelope or an exact JSON object is
    // accepted. Never execute prose, script snippets, or implicit shell text.
    let arguments = if text.trim_start().starts_with("<tool_call") {
        let legacy = parse_legacy_tool_call(text)
            .map_err(|error| format!("invalid legacy tool-call envelope: {error:?}"))?;
        if legacy.name != operation {
            return Err(format!(
                "legacy tool name {} differs from immutable operation {}",
                legacy.name, operation
            ));
        }
        legacy.arguments
    } else {
        serde_json::from_str::<Value>(text)
            .map_err(|error| format!("tool arguments are not JSON: {error}"))?
    };
    if !arguments.is_object() {
        return Err("MCP tools/call requires a JSON object argument".to_owned());
    }
    Ok(arguments)
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
    use crate::routing_audit::{record_route_proposed, record_route_user_decision};
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use crate::tool_call_audit::{record_tool_call, record_tool_call_route_bound};
    use crate::tool_provider_activation_audit::{
        ProviderActivationDecision, append_tool_provider_activation_decision_checked,
    };
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use crate::tool_transport_config_audit::append_tool_transport_config_checked;
    use chatarium_core::LocalConversationId;
    use chatarium_core::chat_container::{
        ChatContainerId, ContextHandoffId, SessionLifecyclePhase, SessionLifecycleTransition,
        SessionSuccessorBinding,
    };
    use chatarium_core::routing::{RouteEndpointId, RouteRequest};
    use chatarium_core::session::SessionEndpointBinding;
    use chatarium_core::tool::{
        StdioToolProviderConfig, ToolOperationName, ToolProviderEndpointBinding, ToolProviderName,
    };
    use serde_json::json;

    const SOURCE: SessionId = SessionId::new(1);
    const SOURCE_ENDPOINT: RouteEndpointId = RouteEndpointId::new(10);
    const PROVIDER_ENDPOINT: RouteEndpointId = RouteEndpointId::new(20);
    const PROVIDER: ToolProviderId = ToolProviderId::new(3);
    const CALL: ToolCallId = ToolCallId::new(4);
    const ROUTE: RouteId = RouteId::new(5);

    fn configured_call(
        operation: &str,
        allow: &str,
        arguments: &str,
        user_approved: bool,
    ) -> MemoryEventStore {
        configured_call_with_activation(operation, allow, arguments, user_approved, false)
    }

    fn configured_call_with_activation(
        operation: &str,
        allow: &str,
        arguments: &str,
        user_approved: bool,
        active_before_call: bool,
    ) -> MemoryEventStore {
        let mut store = MemoryEventStore::default();
        record_local_session_registered(&mut store, SOURCE).unwrap();
        record_chat_container_created(&mut store, ChatContainerId::new(1), SOURCE).unwrap();
        record_local_conversation_chat_container_bound(
            &mut store,
            LocalConversationId::new(),
            ChatContainerId::new(1),
        )
        .unwrap();
        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(SOURCE, SOURCE_ENDPOINT),
        )
        .unwrap();
        record_tool_provider_registered(
            &mut store,
            PROVIDER,
            &ToolProviderName::new("local.example").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(PROVIDER, PROVIDER_ENDPOINT),
        )
        .unwrap();
        let config = StdioToolProviderConfig::new(
            "/usr/bin/example-mcp",
            vec!["--stdio".to_owned()],
            vec![ToolOperationName::new(allow).unwrap()],
        )
        .unwrap();
        let configured =
            append_tool_transport_config_checked(&mut store, PROVIDER, &config).unwrap();
        if active_before_call {
            append_tool_provider_activation_decision_checked(
                &mut store,
                PROVIDER,
                configured.sequence,
                ProviderActivationDecision::Activate,
            )
            .unwrap();
        }
        record_tool_call(
            &mut store,
            CALL,
            SOURCE,
            PROVIDER,
            &ToolOperationName::new(operation).unwrap(),
            arguments,
        )
        .unwrap();
        record_route_proposed(
            &mut store,
            RouteRequest {
                id: ROUTE,
                source: SOURCE_ENDPOINT,
                destination: PROVIDER_ENDPOINT,
                class: RouteClass::ToolCall,
            },
            RoutePolicy::RequireApproval,
        )
        .unwrap();
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();
        if user_approved {
            record_route_user_decision(&mut store, ROUTE, RouteUserDecision::Allow).unwrap();
        }
        store
    }

    #[test]
    fn approved_call_prepares_exact_bounded_mcp_frame_without_side_effects() {
        let store = configured_call("hello", "hello", r#"{"text":"a\nb"}"#, true);
        let before = store.events().len();
        let preview = preview_stdio_tool_invocation(store.events(), CALL).unwrap();
        assert_eq!(preview.call_id, CALL);
        assert_eq!(preview.route_id, ROUTE);
        assert_eq!(preview.executable, "/usr/bin/example-mcp");
        assert_eq!(preview.argv, vec!["--stdio"]);
        let request: Value = serde_json::from_str(preview.request_frame.trim()).unwrap();
        assert_eq!(request["method"], "tools/call");
        assert_eq!(request["id"], CALL.get());
        assert_eq!(request["params"]["name"], "hello");
        assert_eq!(request["params"]["arguments"], json!({"text":"a\nb"}));
        assert!(preview.request_frame.ends_with('\n'));
        assert_eq!(preview.request_frame.matches('\n').count(), 1);
        assert_eq!(store.events().len(), before);
        assert!(
            replay_routing_audit(store.events()).unwrap()[0]
                .dispatch_sequence
                .is_none()
        );
    }

    #[test]
    fn requires_explicit_user_approval_and_exact_allowlist() {
        let not_approved = configured_call("hello", "hello", "{}", false);
        assert!(
            preview_stdio_tool_invocation(not_approved.events(), CALL)
                .unwrap_err()
                .contains("user-approved")
        );
        let excluded = configured_call("read", "hello", "{}", true);
        assert!(
            preview_stdio_tool_invocation(excluded.events(), CALL)
                .unwrap_err()
                .contains("allowlist")
        );
    }

    #[test]
    fn refuses_nonobject_args_and_mismatched_legacy_operation() {
        let array = configured_call("hello", "hello", "[1]", true);
        assert!(preview_stdio_tool_invocation(array.events(), CALL).is_err());

        let envelope = configured_call(
            "hello",
            "hello",
            r#"<tool_call name="read">{"x":1}</tool_call>"#,
            true,
        );
        assert!(
            preview_stdio_tool_invocation(envelope.events(), CALL)
                .unwrap_err()
                .contains("differs")
        );
    }

    #[test]
    fn legacy_wire_id_does_not_override_numeric_mcp_call_identity() {
        let store = configured_call(
            "hello",
            "hello",
            r#"<tool_call name="hello" id="legacy-external-987">{"text":"ok"}</tool_call>"#,
            true,
        );
        let preview = preview_stdio_tool_invocation(store.events(), CALL).unwrap();
        let value: Value = serde_json::from_str(preview.request_frame.trim()).unwrap();
        assert_eq!(value["id"], CALL.get());
        assert_eq!(value["params"]["name"], "hello");
        assert_eq!(value["params"]["arguments"], json!({"text":"ok"}));
        assert_eq!(
            store.events().last().unwrap().kind,
            chatarium_core::EventKind::RouteUserDecisionRecorded
        );
    }

    #[test]
    fn denied_and_already_dispatched_routes_fail_closed() {
        use crate::routing_audit::record_route_dispatched;
        use chatarium_core::routing::{RouteGate, RouteRequest};

        let mut denied = configured_call("hello", "hello", "{}", false);
        record_route_user_decision(&mut denied, ROUTE, RouteUserDecision::Deny).unwrap();
        assert!(preview_stdio_tool_invocation(denied.events(), CALL).is_err());

        let mut dispatched = configured_call("hello", "hello", "{}", true);
        let request = RouteRequest {
            id: ROUTE,
            source: SOURCE_ENDPOINT,
            destination: PROVIDER_ENDPOINT,
            class: RouteClass::ToolCall,
        };
        let mut gate = RouteGate::new(request, RoutePolicy::RequireApproval);
        gate.user_allow().unwrap();
        let permit = gate.authorize_dispatch(ROUTE).unwrap();
        record_route_dispatched(&mut dispatched, permit).unwrap();
        assert!(
            preview_stdio_tool_invocation(dispatched.events(), CALL)
                .unwrap_err()
                .contains("undispatched")
        );
    }

    #[test]
    fn activated_preflight_rejects_unconfigured_activation_and_retroactive_enable() {
        let mut store = configured_call("hello", "hello", "{}", true);
        assert!(
            preview_activated_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("no explicit activation")
        );
        let configured = replay_tool_transport_config_audit(store.events()).unwrap();
        append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            configured[0].configured_sequence,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        assert!(
            preview_activated_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("not active when this call was recorded")
        );
    }

    #[test]
    fn activated_preflight_passes_only_after_separate_prior_provider_activation() {
        let store = configured_call_with_activation("hello", "hello", "{}", true, true);
        let before = store.events().len();
        let preview = preview_activated_stdio_tool_invocation(store.events(), CALL).unwrap();
        assert_eq!(preview.invocation.call_id, CALL);
        assert_eq!(preview.invocation.route_id, ROUTE);
        assert!(
            preview.activation_sequence
                < replay_tool_call_audit(store.events()).unwrap()[0].recorded_sequence
        );
        assert_eq!(store.events().len(), before);
        assert!(
            replay_routing_audit(store.events()).unwrap()[0]
                .dispatch_sequence
                .is_none()
        );
    }

    #[test]
    fn revocation_blocks_pending_calls_and_reactivation_does_not_retroactively_revive_them() {
        let mut store = configured_call_with_activation("hello", "hello", "{}", true, true);
        let configured_seq =
            replay_tool_transport_config_audit(store.events()).unwrap()[0].configured_sequence;
        append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            configured_seq,
            ProviderActivationDecision::Deactivate,
        )
        .unwrap();
        assert!(
            preview_activated_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("deactivated")
        );
        append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            configured_seq,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        assert!(
            preview_activated_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("not active when this call was recorded")
        );
    }

    #[test]
    fn activation_cannot_override_an_unapproved_tool_route() {
        let store = configured_call_with_activation("hello", "hello", "{}", false, true);
        assert!(
            preview_activated_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("user-approved")
        );
    }

    #[test]
    fn stale_source_session_is_not_invocable_after_rollover() {
        let mut store = configured_call("hello", "hello", "{}", true);
        record_chat_session_lifecycle_transition(
            &mut store,
            ChatContainerId::new(1),
            SessionLifecycleTransition::new(
                SOURCE,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .unwrap(),
        )
        .unwrap();
        record_local_session_registered(&mut store, SessionId::new(2)).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(
                ChatContainerId::new(1),
                SOURCE,
                SessionId::new(2),
                ContextHandoffId::new(1),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            preview_stdio_tool_invocation(store.events(), CALL)
                .unwrap_err()
                .contains("not a current local conversation leaf")
        );
    }
}
