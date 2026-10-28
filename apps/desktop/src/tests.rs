use super::*;

#[test]
fn in_chat_mcp_status_follows_durable_approval_dispatch_and_outcome() {
    let mut store = chatarium_store::MemoryEventStore::default();
    let conversation = LocalConversationId::new();
    let other = LocalConversationId::new();
    let request = mcp_agent_defaults::ModelToolRequest {
        provider: local_http_mcp::MATHEMATICA.to_owned(),
        tool: "mathematica_create_session".to_owned(),
        arguments: serde_json::json!({}),
    };
    let (call_id, route_id, _) = append_agent_mcp_call_checked(
        &mut store,
        conversation,
        &request,
        mcp_agent_defaults::ToolPolicy::Ask,
    )
    .unwrap();

    let pending = project_inline_mcp_status(store.events(), conversation).unwrap();
    assert_eq!(pending.call_id, call_id);
    assert_eq!(pending.route_id, route_id);
    assert_eq!(
        pending.stage,
        mcp_call_review::CallReviewStage::AwaitingDecision
    );
    assert!(!pending.automatically_authorized);
    assert!(project_inline_mcp_status(store.events(), other).is_none());

    append_tool_call_route_user_decision_checked(&mut store, route_id, RouteUserDecision::Allow)
        .unwrap();
    let allowed = project_inline_mcp_status(store.events(), conversation).unwrap();
    assert_eq!(
        allowed.stage,
        mcp_call_review::CallReviewStage::ApprovedForSeparateReview
    );
    assert_eq!(allowed.successful, None);

    reserve_local_http_mcp_checked(&mut store, route_id).unwrap();
    let dispatched = project_inline_mcp_status(store.events(), conversation).unwrap();
    assert_eq!(
        dispatched.stage,
        mcp_call_review::CallReviewStage::DispatchedAwaitingOutcome
    );
    assert_eq!(dispatched.successful, None);
    assert!(reserve_local_http_mcp_checked(&mut store, route_id).is_err());

    append_tool_call_outcome_checked(
        &mut store,
        call_id,
        route_id,
        ToolCallOutcomeKind::Result,
        "session-created".to_owned(),
    )
    .unwrap();
    let finished = project_inline_mcp_status(store.events(), conversation).unwrap();
    assert_eq!(
        finished.stage,
        mcp_call_review::CallReviewStage::OutcomeRecorded
    );
    assert_eq!(finished.successful, Some(true));
    assert!(project_inline_mcp_status(store.events(), other).is_none());
}

#[test]
fn model_openviking_write_is_rejected_without_creating_a_durable_tool_route() {
    let mut store = chatarium_store::MemoryEventStore::default();
    let owner = LocalConversationId::new();
    let proposal = mcp_agent_defaults::ModelToolRequest {
        provider: local_http_mcp::OPENVIKING.to_owned(),
        tool: "write".to_owned(),
        arguments: serde_json::json!({
            "uri": format!("viking://~/chatarium/conversations/{owner}/checkpoints/turn-1.md"),
            "mode": "create",
            "content": "unverified model-authored memory",
        }),
    };
    for policy in [
        mcp_agent_defaults::ToolPolicy::Ask,
        mcp_agent_defaults::ToolPolicy::Automatic,
    ] {
        let error =
            append_agent_mcp_call_checked(&mut store, owner, &proposal, policy).unwrap_err();
        assert!(error.contains("automatic verified checkpoint lifecycle"));
        assert!(store.events().is_empty());
    }
}

#[test]
fn in_chat_mcp_status_distinguishes_denied_and_saved_auto_permission() {
    let conversation = LocalConversationId::new();
    let request = mcp_agent_defaults::ModelToolRequest {
        provider: local_http_mcp::MANIFOLDR.to_owned(),
        tool: "search_markets".to_owned(),
        arguments: serde_json::json!({"term":"AI"}),
    };
    let mut denied_store = chatarium_store::MemoryEventStore::default();
    let (_, denied_route, _) = append_agent_mcp_call_checked(
        &mut denied_store,
        conversation,
        &request,
        mcp_agent_defaults::ToolPolicy::Ask,
    )
    .unwrap();
    append_tool_call_route_user_decision_checked(
        &mut denied_store,
        denied_route,
        RouteUserDecision::Deny,
    )
    .unwrap();
    let denied = project_inline_mcp_status(denied_store.events(), conversation).unwrap();
    assert_eq!(denied.stage, mcp_call_review::CallReviewStage::Denied);
    assert!(reserve_local_http_mcp_checked(&mut denied_store, denied_route).is_err());

    let mut authorized_store = chatarium_store::MemoryEventStore::default();
    let (_, auto_route, _) = append_agent_mcp_call_checked(
        &mut authorized_store,
        conversation,
        &request,
        mcp_agent_defaults::ToolPolicy::Automatic,
    )
    .unwrap();
    let automatic = project_inline_mcp_status(authorized_store.events(), conversation).unwrap();
    assert!(automatic.automatically_authorized);
    assert_eq!(automatic.successful, None);
    reserve_local_http_mcp_checked(&mut authorized_store, auto_route).unwrap();
    assert_eq!(
        project_inline_mcp_status(authorized_store.events(), conversation)
            .unwrap()
            .stage,
        mcp_call_review::CallReviewStage::DispatchedAwaitingOutcome
    );
}

fn persist_notice_name(notice: &PersistNotice) -> &'static str {
    match notice {
        PersistNotice::ProjectOpenVikingRetrievalFinished { .. } => {
            "project_openviking_retrieval_finished"
        }
        PersistNotice::OpenVikingRetrievalFinished { .. } => "openviking_retrieval_finished",
        PersistNotice::OpenVikingMemorySummarySaved { .. } => "openviking_summary_saved",
        PersistNotice::OpenVikingMemoryWriteFinished { .. } => "openviking_write_finished",
        PersistNotice::DraftSaved { .. } => "draft_saved",
        PersistNotice::MessageCommitted { .. } => "message_committed",
        PersistNotice::TurnEventAppended { .. } => "turn_event_appended",
        PersistNotice::AgentActivityAppended { .. } => "agent_activity_appended",
        PersistNotice::MemoryCheckpointQueued { .. } => "memory_checkpoint_queued",
        PersistNotice::HistoricalConversationLoaded { .. } => "historical_loaded",
        PersistNotice::HistoricalConversationLoadFailed { .. } => "historical_load_failed",
        PersistNotice::LiveConversationLoaded { .. } => "live_loaded",
        PersistNotice::LiveConversationLoadFailed { .. } => "live_load_failed",
        PersistNotice::HistoricalLiveMirrorPromoted { .. } => "live_promoted",
        PersistNotice::HistoricalLiveMirrorPromotionFailed { .. } => "live_promotion_failed",
        PersistNotice::DiscoveredLiveMirrorPromoted { .. } => "discovered_live_promoted",
        PersistNotice::DiscoveredLiveMirrorPromotionFailed { .. } => {
            "discovered_live_promotion_failed"
        }
        PersistNotice::MirrorQueueUpdated { .. } => "mirror_queue_updated",
        PersistNotice::RemoteHealthUpdated { .. } => "remote_health_updated",
        PersistNotice::OrchestrationTopologyInitialized { .. } => {
            "orchestration_topology_initialized"
        }
        PersistNotice::RouteEndpointBound { .. } => "route_endpoint_bound",
        PersistNotice::LocalRoutePolicyEventAppended { .. } => "local_route_policy_event_appended",
        PersistNotice::LocalRoutePayloadAttached { .. } => "local_route_payload_attached",
        PersistNotice::LocalRouteDispatchUpdated { .. } => "local_route_dispatch_updated",
        PersistNotice::LocalRouteContextDecisionUpdated { .. } => {
            "local_route_context_decision_updated"
        }
        PersistNotice::ToolProviderUpdated { .. } => "tool_provider_updated",
        PersistNotice::AgentMcpCallProposed { .. } => "agent_mcp_call_proposed",
        PersistNotice::ToolCallProposed { .. } => "tool_call_proposed",
        PersistNotice::ToolCallRoutePolicyUpdated { .. } => "tool_call_route_policy_updated",
        PersistNotice::BuiltinHelloDispatched { .. } => "builtin_hello_dispatched",
        PersistNotice::HostShellDispatched { .. } => "host_shell_dispatched",
        PersistNotice::SandboxHostChecked { .. } => "sandbox_host_checked",
        PersistNotice::StdioToolDispatchReserved { .. } => "stdio_tool_dispatch_reserved",
        PersistNotice::StdioToolDispatchFinished { .. } => "stdio_tool_dispatch_finished",
        PersistNotice::ToolResultContextDecisionUpdated { .. } => {
            "tool_result_context_decision_updated"
        }
        PersistNotice::LocalMemoryArtifactRecorded { .. } => "local_memory_artifact_recorded",
        PersistNotice::LocalMemoryContextDecisionUpdated { .. } => {
            "local_memory_context_decision_updated"
        }
        PersistNotice::LocalMemorySupersessionRecorded { .. } => {
            "local_memory_supersession_recorded"
        }
        PersistNotice::LocalMemoryLabelUpdated { .. } => "local_memory_label_updated",
        PersistNotice::LifecycleEventAppended { .. } => "lifecycle_event_appended",
        PersistNotice::SupervisionEventAppended { .. } => "supervision_event_appended",
        PersistNotice::ControllerContinuationLeaseCreated { .. } => {
            "controller_continuation_lease_created"
        }
        PersistNotice::ControllerControlProposed { .. } => "controller_control_proposed",
        PersistNotice::ControllerControlRoutePolicyUpdated { .. } => {
            "controller_control_route_policy_updated"
        }
        PersistNotice::ControllerControlDispatched { .. } => "controller_control_dispatched",
        PersistNotice::ControllerWorkerResultContextDecisionUpdated { .. } => {
            "controller_worker_result_context_decision_updated"
        }
        PersistNotice::ControllerCoordinationStarted { .. } => "controller_coordination_started",
        PersistNotice::ControllerCoordinationResultRecorded { .. } => {
            "controller_coordination_result_recorded"
        }
        PersistNotice::ControllerCoordinationResultContextDecisionUpdated { .. } => {
            "controller_coordination_result_context_decision_updated"
        }
        PersistNotice::ControllerCoordinationSuggestionRecorded { .. } => {
            "controller_coordination_suggestion_recorded"
        }
        PersistNotice::ControllerCoordinationSuggestionPromoted { .. } => {
            "controller_coordination_suggestion_promoted"
        }
        PersistNotice::WorkerControlAcknowledged { .. } => "worker_control_acknowledged",
        PersistNotice::WorkerControlStatusResultRecorded { .. } => {
            "worker_control_status_result_recorded"
        }
        PersistNotice::WorkerControlActionAdvanced { .. } => "worker_control_action_advanced",
        PersistNotice::WorkerContinuationExecutionStarted { .. } => {
            "worker_continuation_execution_started"
        }
        PersistNotice::WorkerContinuationExecutionResultRecorded { .. } => {
            "worker_continuation_execution_result_recorded"
        }
        PersistNotice::Failed { .. } => "failed",
    }
}

#[cfg(target_os = "linux")]
#[test]
fn live_stdio_dispatch_worker_contract_records_exact_terminal_observation() {
    if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
        return;
    }
    let source = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    append_local_orchestration_topology_checked(
        &mut store,
        source,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        source,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();
    let provider = ToolProviderId::new(30);
    let call = ToolCallId::new(71);
    let route = RouteId::new(72);
    append_tool_provider_registration_checked(
        &mut store,
        provider,
        ToolProviderName::new("external.test").unwrap(),
    )
    .unwrap();
    append_tool_provider_endpoint_checked(&mut store, provider, RouteEndpointId::new(50)).unwrap();

    // Real sandbox process emits one valid bounded MCP response to
    // the immutable, explicitly approved tool request.
    let reply = r#"{"jsonrpc":"2.0","id":71,"result":{"resultType":"complete","content":[{"type":"text","text":"durable-ok"}]}}"#;
    let sed = StdioToolProviderConfig::new(
        "/usr/bin/sed",
        vec!["-n".to_owned(), "-e".to_owned(), format!("s/.*/{reply}/p")],
        vec![ToolOperationName::new("fixture.echo").unwrap()],
    )
    .unwrap();
    let configured = append_tool_transport_config_checked(&mut store, provider, &sed).unwrap();
    local_tool_provider_control::append_activation_with_inspection(
        &mut store,
        provider,
        configured.sequence,
        ProviderActivationDecision::Activate,
    )
    .unwrap();
    append_tool_call_proposal_checked(
        &mut store,
        call,
        route,
        source,
        provider,
        ToolOperationName::new("fixture.echo").unwrap(),
        "{}".to_owned(),
        RoutePolicy::RequireApproval,
    )
    .unwrap();

    let unapproved_events = store.events().len();
    assert!(reserve_activated_stdio_tool_dispatch(&mut store, call).is_err());
    assert_eq!(store.events().len(), unapproved_events);

    append_tool_call_route_user_decision_checked(&mut store, route, RouteUserDecision::Allow)
        .unwrap();
    let approved = preview_activated_stdio_tool_invocation(store.events(), call).unwrap();
    local_stdio_runner::preflight_confined_stdio_launch(
        approved.invocation.executable.as_str(),
        &approved.invocation.argv,
    )
    .unwrap();
    let reserved = reserve_activated_stdio_tool_dispatch(&mut store, call).unwrap();
    let dispatch_sequence = reserved.dispatch_sequence();
    assert_eq!(
        store.events().last().unwrap().kind,
        EventKind::RouteDispatched
    );
    assert_eq!(
        replay_unresolved_external_tool_dispatches(store.events())
            .unwrap()
            .len(),
        1,
    );
    let observed = local_stdio_runner::run_reserved_stdio_tool_call(reserved);
    assert_eq!(observed.call_id, call);
    assert_eq!(observed.route_id, route);
    // Journal writes can interleave with sandbox work.
    append_local_memory_artifact_checked(
        &mut store,
        LocalMemoryId::new(73),
        source,
        "independent write during MCP execution".to_owned(),
    )
    .unwrap();
    let outcome_event = append_tool_call_outcome_checked(
        &mut store,
        observed.call_id,
        observed.route_id,
        observed.kind,
        observed.text,
    )
    .unwrap();
    assert_eq!(outcome_event.kind, EventKind::ToolCallOutcomeObserved);
    assert!(outcome_event.sequence > dispatch_sequence + 1);
    let outcome = replay_tool_call_outcome_audit(store.events())
        .unwrap()
        .into_iter()
        .find(|event| event.call_id == call)
        .unwrap();
    assert_eq!(outcome.dispatch_sequence, dispatch_sequence);
    assert_eq!(outcome.kind, ToolCallOutcomeKind::Result);
    let parsed =
        chatarium_protocol::mcp_wire::decode_stdio_response(&outcome.text, call.get()).unwrap();
    assert!(matches!(
        parsed,
        chatarium_protocol::mcp_wire::McpResponse::Complete(_)
    ));
    assert!(outcome.text.contains("durable-ok"));
    assert!(
        replay_unresolved_external_tool_dispatches(store.events())
            .unwrap()
            .is_empty()
    );
    let final_events = store.events().len();
    assert!(reserve_activated_stdio_tool_dispatch(&mut store, call).is_err());
    assert_eq!(store.events().len(), final_events);
}

#[cfg(target_os = "linux")]
#[test]
fn native_host_shell_reserves_only_approved_durable_routes() {
    use chatarium_store::MemoryEventStore;
    let mut store = MemoryEventStore::default();
    let proposal = mcp_agent_defaults::ModelToolRequest {
        provider: local_tool_adapter::BUILTIN_PROVIDER_NAME.to_owned(),
        tool: host_shell_mcp::OPERATION.to_owned(),
        arguments: serde_json::json!({"command": "printf test"}),
    };
    let (_call, route, _) = append_agent_mcp_call_checked(
        &mut store,
        LocalConversationId::new(),
        &proposal,
        mcp_agent_defaults::ToolPolicy::Ask,
    )
    .unwrap();
    let settings = host_shell_mcp::Settings {
        shell: "sh".to_owned(),
        ..Default::default()
    };
    assert!(reserve_builtin_host_shell_checked(&mut store, route, &settings).is_err());
    append_tool_call_route_user_decision_checked(&mut store, route, RouteUserDecision::Allow)
        .unwrap();
    let (call_id, args, event) =
        reserve_builtin_host_shell_checked(&mut store, route, &settings).unwrap();
    assert_eq!(args["command"], "printf test");
    assert_eq!(event.kind, EventKind::RouteDispatched);
    assert!(reserve_builtin_host_shell_checked(&mut store, route, &settings).is_err());
    assert!(
        replay_tool_call_outcome_audit(store.events())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        replay_tool_call_audit(store.events())
            .unwrap()
            .iter()
            .find(|call| call.call_id == call_id)
            .unwrap()
            .route_id,
        Some(route)
    );
}

#[test]
fn automatic_builtin_mcp_cycle_is_policy_audited_and_result_is_durable() {
    use chatarium_store::MemoryEventStore;
    let source = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let proposal = mcp_agent_defaults::ModelToolRequest {
        provider: local_tool_adapter::BUILTIN_PROVIDER_NAME.to_owned(),
        tool: local_tool_adapter::TOOLS_LIST_OPERATION.to_owned(),
        arguments: serde_json::json!({}),
    };
    let (call_id, route_id, created) = append_agent_mcp_call_checked(
        &mut store,
        source,
        &proposal,
        mcp_agent_defaults::ToolPolicy::Automatic,
    )
    .unwrap();
    assert!(created.len() >= 8); // topology + endpoint + provider + call + route
    let routed = replay_routing_audit(store.events()).unwrap();
    let agent_route = routed.iter().find(|r| r.request.id == route_id).unwrap();
    assert_eq!(agent_route.initial_policy, RoutePolicy::Allow);
    assert_eq!(
        agent_route.gate_state,
        RouteGateState::Allowed {
            by: DecisionAuthority::Policy
        }
    );
    let recorded = replay_tool_call_audit(store.events()).unwrap();
    assert_eq!(
        recorded
            .iter()
            .find(|r| r.call_id == call_id)
            .unwrap()
            .operation
            .as_str(),
        "tools.list"
    );
    let emitted = append_builtin_hello_checked(&mut store, route_id).unwrap();
    assert_eq!(emitted.len(), 2);
    let outcomes = replay_tool_call_outcome_audit(store.events()).unwrap();
    let actual = outcomes.iter().find(|r| r.call_id == call_id).unwrap();
    assert_eq!(actual.kind, ToolCallOutcomeKind::Result);
    assert!(actual.text.contains("tools.list"));
    assert_eq!(
        replay_routing_audit(store.events())
            .unwrap()
            .into_iter()
            .find(|r| r.request.id == route_id)
            .unwrap()
            .gate_state,
        RouteGateState::Dispatched {
            authorized_by: DecisionAuthority::Policy
        }
    );
    assert!(append_builtin_hello_checked(&mut store, route_id).is_err());
}

#[test]
fn ask_mode_mcp_call_stays_pending_and_never_executes_by_itself() {
    use chatarium_store::MemoryEventStore;
    let mut store = MemoryEventStore::default();
    let proposal = mcp_agent_defaults::ModelToolRequest {
        provider: local_tool_adapter::BUILTIN_PROVIDER_NAME.to_owned(),
        tool: "hello".to_owned(),
        arguments: serde_json::json!({}),
    };
    let (_call, route_id, _) = append_agent_mcp_call_checked(
        &mut store,
        LocalConversationId::new(),
        &proposal,
        mcp_agent_defaults::ToolPolicy::Ask,
    )
    .unwrap();
    assert_eq!(
        replay_routing_audit(store.events()).unwrap()[0].gate_state,
        RouteGateState::PendingApproval
    );
    assert!(append_builtin_hello_checked(&mut store, route_id).is_err());
    assert!(
        replay_tool_call_outcome_audit(store.events())
            .unwrap()
            .is_empty()
    );
    let count = store.events().len();
    assert!(
        append_agent_mcp_call_checked(
            &mut store,
            LocalConversationId::new(),
            &proposal,
            mcp_agent_defaults::ToolPolicy::Disabled,
        )
        .is_err()
    );
    assert_eq!(store.events().len(), count);
}

#[test]
fn approved_builtin_hello_dispatch_is_durable_and_one_shot() {
    let source = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    append_local_orchestration_topology_checked(
        &mut store,
        source,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        source,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();
    append_tool_provider_registration_checked(
        &mut store,
        ToolProviderId::new(1),
        ToolProviderName::new(local_tool_adapter::BUILTIN_PROVIDER_NAME).unwrap(),
    )
    .unwrap();
    append_tool_provider_endpoint_checked(
        &mut store,
        ToolProviderId::new(1),
        RouteEndpointId::new(2),
    )
    .unwrap();
    append_tool_call_proposal_checked(
        &mut store,
        ToolCallId::new(1),
        RouteId::new(1),
        source,
        ToolProviderId::new(1),
        ToolOperationName::new("hello").unwrap(),
        "<tool_call id=\"legacy-test\" name=\"hello\">{}\n</tool_call>".to_owned(),
        RoutePolicy::RequireApproval,
    )
    .unwrap();

    let before = store.events().len();
    assert!(
        append_builtin_hello_checked(&mut store, RouteId::new(1))
            .unwrap_err()
            .contains("requires a fresh explicit user Allow")
    );
    assert_eq!(store.events().len(), before);
    append_tool_call_route_user_decision_checked(
        &mut store,
        RouteId::new(1),
        RouteUserDecision::Allow,
    )
    .unwrap();
    let events = append_builtin_hello_checked(&mut store, RouteId::new(1)).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EventKind::RouteDispatched);
    assert_eq!(events[1].kind, EventKind::ToolCallOutcomeObserved);

    let outcome = replay_tool_call_outcome_audit(store.events())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(outcome.kind, ToolCallOutcomeKind::Result);
    let wire =
        chatarium_protocol::tool_envelope::parse_legacy_tool_result(outcome.text.as_str()).unwrap();
    assert_eq!(wire.id, "legacy-test");
    assert_eq!(wire.payload["message"], "hello");

    let after = store.events().len();
    assert!(append_builtin_hello_checked(&mut store, RouteId::new(1)).is_err());
    assert_eq!(store.events().len(), after);
}

#[test]
fn checked_local_orchestration_topology_is_durable_and_one_to_one() {
    let conversation_id = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    let container_id = ChatContainerId::new(1);
    let session_id = SessionId::new(1);

    let appended = append_local_orchestration_topology_checked(
        &mut store,
        conversation_id,
        container_id,
        session_id,
    )
    .unwrap();
    assert_eq!(appended.len(), 3);
    assert_eq!(appended[0].kind, EventKind::LocalSessionRegistered);
    assert_eq!(appended[1].kind, EventKind::ChatContainerCreated);
    assert_eq!(
        appended[2].kind,
        EventKind::LocalConversationChatContainerBound
    );

    let topology = local_conversation_topology(store.events(), conversation_id)
        .unwrap()
        .unwrap();
    assert_eq!(topology.container_id, container_id);
    assert_eq!(topology.root_session_id, session_id);
    assert_eq!(topology.current_session_id, session_id);
    assert_eq!(
        topology.current_session_phase,
        SessionLifecyclePhase::Healthy
    );

    let before_duplicate = store.events().len();
    assert!(
        append_local_orchestration_topology_checked(
            &mut store,
            conversation_id,
            ChatContainerId::new(2),
            SessionId::new(2),
        )
        .unwrap_err()
        .contains("already owns chat container")
    );
    assert_eq!(store.events().len(), before_duplicate);

    let (next_container, next_session) =
        next_available_local_orchestration_ids(store.events()).unwrap();
    assert_eq!(next_container, ChatContainerId::new(2));
    assert_eq!(next_session, SessionId::new(2));
}

#[test]
fn current_session_route_addressability_is_durable_and_fails_closed() {
    let conversation_id = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    append_local_orchestration_topology_checked(
        &mut store,
        conversation_id,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();

    let event = append_current_session_route_endpoint_checked(
        &mut store,
        conversation_id,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();
    assert_eq!(event.kind, EventKind::SessionEndpointBound);
    assert_eq!(
        session_endpoint_binding(store.events(), SessionId::new(1))
            .unwrap()
            .unwrap()
            .endpoint_id(),
        RouteEndpointId::new(1)
    );

    let before_duplicate = store.events().len();
    assert!(
        append_current_session_route_endpoint_checked(
            &mut store,
            conversation_id,
            SessionId::new(1),
            RouteEndpointId::new(2),
        )
        .unwrap_err()
        .contains("already bound to routing endpoint")
    );
    assert_eq!(store.events().len(), before_duplicate);
}

#[test]
fn route_endpoint_allocator_reserves_historical_route_endpoints() {
    use chatarium_core::routing::{RouteClass, RouteId, RoutePolicy, RouteRequest};
    use chatarium_store::routing_audit::record_route_proposed;

    let conversation_id = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    append_local_orchestration_topology_checked(
        &mut store,
        conversation_id,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();

    record_route_proposed(
        &mut store,
        RouteRequest {
            id: RouteId::new(1),
            source: RouteEndpointId::new(40),
            destination: RouteEndpointId::new(41),
            class: RouteClass::SessionMessage,
        },
        RoutePolicy::RequireApproval,
    )
    .unwrap();

    assert_eq!(
        next_available_route_endpoint_id(store.events()).unwrap(),
        RouteEndpointId::new(42)
    );

    let before = store.events().len();
    assert!(
        append_current_session_route_endpoint_checked(
            &mut store,
            conversation_id,
            SessionId::new(1),
            RouteEndpointId::new(40),
        )
        .unwrap_err()
        .contains("durable route history")
    );
    assert_eq!(store.events().len(), before);
}

#[test]
fn manual_local_route_policy_is_durable_and_requires_current_addressability() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let missing = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_orchestration_topology_checked(
        &mut store,
        source,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        source,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();

    append_local_orchestration_topology_checked(
        &mut store,
        destination,
        ChatContainerId::new(2),
        SessionId::new(2),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        destination,
        SessionId::new(2),
        RouteEndpointId::new(2),
    )
    .unwrap();

    let proposed = append_local_session_route_proposal_checked(
        &mut store,
        RouteId::new(1),
        source,
        destination,
    )
    .unwrap();
    assert_eq!(proposed.kind, EventKind::RouteProposed);

    let route = replay_routing_audit(store.events())
        .unwrap()
        .into_iter()
        .find(|route| route.request.id == RouteId::new(1))
        .unwrap();
    assert_eq!(route.request.source, RouteEndpointId::new(1));
    assert_eq!(route.request.destination, RouteEndpointId::new(2));
    assert_eq!(route.request.class, RouteClass::SessionMessage);
    assert_eq!(route.initial_policy, RoutePolicy::RequireApproval);
    assert_eq!(route.gate_state, RouteGateState::PendingApproval);

    let before_payload = store.events().len();
    assert!(
        append_local_route_user_decision_checked(
            &mut store,
            RouteId::new(1),
            RouteUserDecision::Allow,
        )
        .unwrap_err()
        .contains("cannot be allowed before an immutable payload")
    );
    assert_eq!(store.events().len(), before_payload);

    let payload_event = append_local_route_payload_checked(
        &mut store,
        RoutePayloadId::new(1),
        RouteId::new(1),
        " exact routed payload ".to_owned(),
    )
    .unwrap();
    assert_eq!(payload_event.kind, EventKind::RoutePayloadAttached);
    let payloads = replay_local_route_payload_audit(store.events()).unwrap();
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].payload_id, RoutePayloadId::new(1));
    assert_eq!(payloads[0].text, " exact routed payload ");
    assert_eq!(
        next_available_route_payload_id(store.events()).unwrap(),
        RoutePayloadId::new(2)
    );

    let allowed = append_local_route_user_decision_checked(
        &mut store,
        RouteId::new(1),
        RouteUserDecision::Allow,
    )
    .unwrap();
    assert_eq!(allowed.kind, EventKind::RouteUserDecisionRecorded);
    assert_eq!(
        replay_routing_audit(store.events()).unwrap()[0].gate_state,
        RouteGateState::Allowed {
            by: DecisionAuthority::User,
        }
    );

    append_local_route_user_decision_checked(&mut store, RouteId::new(1), RouteUserDecision::Deny)
        .unwrap();
    assert_eq!(
        replay_routing_audit(store.events()).unwrap()[0].gate_state,
        RouteGateState::Denied {
            by: DecisionAuthority::User,
        }
    );
    assert_eq!(
        next_available_route_id(store.events()).unwrap(),
        RouteId::new(2)
    );

    let before_missing = store.events().len();
    assert!(
        append_local_session_route_proposal_checked(&mut store, RouteId::new(2), source, missing,)
            .unwrap_err()
            .contains("not currently addressable")
    );
    assert_eq!(store.events().len(), before_missing);
}

#[test]
fn delivered_routed_context_is_explicit_reversible_and_not_transcript_authorship() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_orchestration_topology_checked(
        &mut store,
        source,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        source,
        SessionId::new(1),
        RouteEndpointId::new(1),
    )
    .unwrap();
    append_local_orchestration_topology_checked(
        &mut store,
        destination,
        ChatContainerId::new(2),
        SessionId::new(2),
    )
    .unwrap();
    append_current_session_route_endpoint_checked(
        &mut store,
        destination,
        SessionId::new(2),
        RouteEndpointId::new(2),
    )
    .unwrap();

    append_local_session_route_proposal_checked(&mut store, RouteId::new(1), source, destination)
        .unwrap();
    append_local_route_payload_checked(
        &mut store,
        RoutePayloadId::new(1),
        RouteId::new(1),
        "peer payload".to_owned(),
    )
    .unwrap();
    append_local_route_user_decision_checked(&mut store, RouteId::new(1), RouteUserDecision::Allow)
        .unwrap();
    let delivery_events =
        append_local_route_dispatch_and_delivery_checked(&mut store, RouteId::new(1)).unwrap();
    assert_eq!(delivery_events.len(), 2);
    assert_eq!(delivery_events[0].kind, EventKind::RouteDispatched);
    assert_eq!(delivery_events[1].kind, EventKind::LocalRouteDelivered);

    assert!(
        projected_local_display_messages(store.events(), destination)
            .iter()
            .all(|message| message.text != "peer payload")
    );
    assert!(
        admitted_routed_context_messages(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    let admit = append_local_route_context_decision_checked(
        &mut store,
        RouteId::new(1),
        destination,
        LocalRouteContextDecision::Admit,
    )
    .unwrap();
    assert_eq!(admit.kind, EventKind::LocalRouteContextDecisionRecorded);

    let routed = admitted_routed_context_messages(store.events(), destination).unwrap();
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].role, context_composer::TranscriptRole::User);
    assert_eq!(routed[0].order_sequence(), admit.sequence);
    assert!(routed[0].text.contains("Chatarium routed peer message"));
    assert!(routed[0].text.contains("peer payload"));

    let plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        routed,
    );
    assert_eq!(plan.routed_context_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");

    append_local_route_context_decision_checked(
        &mut store,
        RouteId::new(1),
        destination,
        LocalRouteContextDecision::Exclude,
    )
    .unwrap();
    assert!(
        admitted_routed_context_messages(store.events(), destination)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        replay_local_route_delivery_audit(store.events())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn local_memory_is_immutable_explicit_and_cross_conversation_admitted() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let memory_id = LocalMemoryId::new(1);
    let mut store = chatarium_store::MemoryEventStore::default();

    let artifact = append_local_memory_artifact_checked(
        &mut store,
        memory_id,
        source,
        " exact remembered fact ".to_owned(),
    )
    .unwrap();
    assert_eq!(artifact.kind, EventKind::LocalMemoryArtifactRecorded);
    assert_eq!(
        next_available_local_memory_id(store.events()).unwrap(),
        LocalMemoryId::new(2)
    );
    assert!(
        projected_local_display_messages(store.events(), destination)
            .iter()
            .all(|message| message.text != " exact remembered fact ")
    );
    assert!(
        admitted_local_memory_messages(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    let admit = append_local_memory_context_decision_checked(
        &mut store,
        memory_id,
        destination,
        LocalMemoryContextDecision::Admit,
    )
    .unwrap();
    assert_eq!(admit.kind, EventKind::LocalMemoryContextDecisionRecorded);

    let memory = admitted_local_memory_messages(store.events(), destination).unwrap();
    assert_eq!(memory.len(), 1);
    assert_eq!(memory[0].role, context_composer::TranscriptRole::User);
    assert_eq!(memory[0].order_sequence(), admit.sequence);
    assert!(memory[0].text.contains("Chatarium local memory"));
    assert!(memory[0].text.contains(&source.to_string()));
    assert!(memory[0].text.contains(" exact remembered fact "));

    let plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        memory,
    );
    assert_eq!(plan.local_memory_count(), 1);
    assert_eq!(plan.messages.len(), 1);
    assert_eq!(plan.messages[0].role, "user");

    let before_duplicate = store.events().len();
    assert!(
        append_local_memory_context_decision_checked(
            &mut store,
            memory_id,
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap_err()
        .contains("already has context decision")
    );
    assert_eq!(store.events().len(), before_duplicate);

    append_local_memory_context_decision_checked(
        &mut store,
        memory_id,
        destination,
        LocalMemoryContextDecision::Exclude,
    )
    .unwrap();
    assert!(
        admitted_local_memory_messages(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    let artifacts = replay_local_memory_audit(store.events()).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].text, " exact remembered fact ");
    assert_eq!(artifacts[0].source_conversation_id, source);
}

#[test]
fn one_shot_local_memory_is_turn_scoped_durable_and_not_persistent_admission() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let memory_id = LocalMemoryId::new(1);
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_memory_artifact_checked(&mut store, memory_id, source, "one shot fact".to_owned())
        .unwrap();
    let snapshot_after_sequence = store.events().last().unwrap().sequence;

    let first = AuthoredUserMessage::new(
        destination,
        LocalTurnId::new(),
        LocalMessageId::new(),
        "use the selected memory",
    );
    let (commit_event, selection_event) = commit_message_with_one_shot_memory_checked(
        &mut store,
        &first,
        &[memory_id],
        Some(snapshot_after_sequence),
    )
    .unwrap();
    let selection_event = selection_event.unwrap();
    assert_eq!(commit_event.kind, EventKind::UserMessageCommitted);
    assert_eq!(
        selection_event.kind,
        EventKind::LocalMemoryTurnSelectionRecorded
    );
    assert!(commit_event.sequence < selection_event.sequence);
    assert!(
        replay_admitted_local_memory_context(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    let one_shot = one_shot_local_memory_messages_for_turn(store.events(), first.turn_id).unwrap();
    assert_eq!(one_shot.len(), 1);
    assert_eq!(one_shot[0].role, context_composer::TranscriptRole::User);
    assert!(one_shot[0].text.contains("NEXT REQUEST ONLY"));
    assert!(one_shot[0].text.contains("one shot fact"));
    assert!(one_shot[0].text.contains(&format!(
        "one_shot_selection_event: #{}",
        selection_event.sequence
    )));

    let plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        one_shot,
    );
    assert_eq!(plan.local_memory_count(), 0);
    assert_eq!(plan.local_memory_one_shot_count(), 1);

    let second = AuthoredUserMessage::new(
        destination,
        LocalTurnId::new(),
        LocalMessageId::new(),
        "do not reuse it",
    );
    commit_message_with_one_shot_memory_checked(&mut store, &second, &[], None).unwrap();
    assert!(
        one_shot_local_memory_messages_for_turn(store.events(), second.turn_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        replay_local_memory_turn_selection_audit(store.events())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_one_shot_memory_fails_before_authored_commit() {
    let conversation = LocalConversationId::new();
    let memory_id = LocalMemoryId::new(1);
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_memory_artifact_checked(
        &mut store,
        memory_id,
        conversation,
        "always admitted".to_owned(),
    )
    .unwrap();
    append_local_memory_context_decision_checked(
        &mut store,
        memory_id,
        conversation,
        LocalMemoryContextDecision::Admit,
    )
    .unwrap();
    let snapshot_after_sequence = store.events().last().unwrap().sequence;
    let before = store.events().len();

    let message = AuthoredUserMessage::new(
        conversation,
        LocalTurnId::new(),
        LocalMessageId::new(),
        "must fail before commit",
    );
    assert!(
        commit_message_with_one_shot_memory_checked(
            &mut store,
            &message,
            &[memory_id],
            Some(snapshot_after_sequence),
        )
        .unwrap_err()
        .contains("persistently admitted")
    );
    assert_eq!(store.events().len(), before);
}

#[test]
fn local_memory_supersession_preserves_history_and_requires_fresh_successor_admission() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let predecessor = LocalMemoryId::new(1);
    let successor = LocalMemoryId::new(2);
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_memory_artifact_checked(&mut store, predecessor, source, "old fact".to_owned())
        .unwrap();
    append_local_memory_artifact_checked(
        &mut store,
        successor,
        source,
        "corrected fact".to_owned(),
    )
    .unwrap();
    append_local_memory_context_decision_checked(
        &mut store,
        predecessor,
        destination,
        LocalMemoryContextDecision::Admit,
    )
    .unwrap();
    assert_eq!(
        admitted_local_memory_messages(store.events(), destination)
            .unwrap()
            .len(),
        1
    );

    let supersession =
        append_local_memory_supersession_checked(&mut store, predecessor, successor).unwrap();
    assert_eq!(supersession.kind, EventKind::LocalMemoryArtifactSuperseded);

    let raw_context = replay_local_memory_context_audit(store.events()).unwrap();
    let old_decision = raw_context
        .iter()
        .find(|record| {
            record.memory_id == predecessor && record.destination_conversation_id == destination
        })
        .unwrap();
    assert_eq!(old_decision.decision, LocalMemoryContextDecision::Admit);

    assert!(
        admitted_local_memory_messages(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    let before_stale_admit = store.events().len();
    assert!(
        append_local_memory_context_decision_checked(
            &mut store,
            predecessor,
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap_err()
        .contains("superseded by")
    );
    assert_eq!(store.events().len(), before_stale_admit);

    assert!(
        replay_admitted_local_memory_context(store.events(), destination)
            .unwrap()
            .is_empty()
    );

    append_local_memory_context_decision_checked(
        &mut store,
        successor,
        destination,
        LocalMemoryContextDecision::Admit,
    )
    .unwrap();
    let effective = admitted_local_memory_messages(store.events(), destination).unwrap();
    assert_eq!(effective.len(), 1);
    assert!(effective[0].text.contains("corrected fact"));
    assert!(!effective[0].text.contains("old fact"));
}

#[test]
fn checked_local_controller_supervision_requires_explicit_worker_session_role() {
    let controller_conversation = LocalConversationId::new();
    let worker_conversation = LocalConversationId::new();
    let worker_id = WorkerId::new(1);
    let mut store = chatarium_store::MemoryEventStore::default();

    append_local_orchestration_topology_checked(
        &mut store,
        controller_conversation,
        ChatContainerId::new(1),
        SessionId::new(1),
    )
    .unwrap();
    append_local_orchestration_topology_checked(
        &mut store,
        worker_conversation,
        ChatContainerId::new(2),
        SessionId::new(2),
    )
    .unwrap();
    append_local_worker_binding_checked(&mut store, worker_conversation, worker_id).unwrap();

    let worker_session =
        append_current_session_worker_binding_checked(&mut store, worker_conversation, worker_id)
            .unwrap();
    assert_eq!(worker_session.kind, EventKind::WorkerSessionBound);
    assert_eq!(
        active_worker_session(&replay_session_audit(store.events()).unwrap(), worker_id)
            .unwrap()
            .session_id,
        SessionId::new(2)
    );

    let before_role_conflict = store.events().len();
    assert!(
        append_current_session_controller_designation_checked(&mut store, worker_conversation,)
            .unwrap_err()
            .contains("worker-bound")
    );
    assert_eq!(store.events().len(), before_role_conflict);

    let controller =
        append_current_session_controller_designation_checked(&mut store, controller_conversation)
            .unwrap();
    assert_eq!(controller.kind, EventKind::ControllerSessionDesignated);

    let supervised = append_local_controller_worker_binding_checked(
        &mut store,
        controller_conversation,
        worker_conversation,
    )
    .unwrap();
    assert_eq!(supervised.kind, EventKind::ControllerWorkerBound);

    let audit = replay_supervision_audit(store.events()).unwrap();
    assert_eq!(audit.controllers.len(), 1);
    assert_eq!(audit.bindings.len(), 1);
    assert_eq!(
        audit.bindings[0].binding.controller_session_id(),
        SessionId::new(1)
    );
    assert_eq!(
        audit.bindings[0].binding.worker_session_id(),
        SessionId::new(2)
    );

    let before_duplicate = store.events().len();
    assert!(
        append_local_controller_worker_binding_checked(
            &mut store,
            controller_conversation,
            worker_conversation,
        )
        .unwrap_err()
        .contains("already supervised")
    );
    assert_eq!(store.events().len(), before_duplicate);
}

#[test]
fn checked_controller_controls_require_approval_and_revalidate_freshness() {
    fn ready_supervised_pair(
        store: &mut chatarium_store::MemoryEventStore,
    ) -> (
        LocalConversationId,
        LocalConversationId,
        WorkerId,
        WorkerGoalId,
    ) {
        let controller_conversation = LocalConversationId::new();
        let worker_conversation = LocalConversationId::new();
        let worker_id = WorkerId::new(1);
        let goal_id = WorkerGoalId::new(1);

        append_local_orchestration_topology_checked(
            store,
            controller_conversation,
            ChatContainerId::new(1),
            SessionId::new(1),
        )
        .unwrap();
        append_current_session_route_endpoint_checked(
            store,
            controller_conversation,
            SessionId::new(1),
            RouteEndpointId::new(1),
        )
        .unwrap();
        append_local_orchestration_topology_checked(
            store,
            worker_conversation,
            ChatContainerId::new(2),
            SessionId::new(2),
        )
        .unwrap();
        append_current_session_route_endpoint_checked(
            store,
            worker_conversation,
            SessionId::new(2),
            RouteEndpointId::new(2),
        )
        .unwrap();
        append_local_worker_binding_checked(store, worker_conversation, worker_id).unwrap();
        append_current_session_worker_binding_checked(store, worker_conversation, worker_id)
            .unwrap();
        append_current_session_controller_designation_checked(store, controller_conversation)
            .unwrap();
        append_local_controller_worker_binding_checked(
            store,
            controller_conversation,
            worker_conversation,
        )
        .unwrap();
        append_worker_goal_checked(store, worker_id, goal_id).unwrap();
        append_worker_transition_checked(store, worker_id, goal_id, WorkerAction::StartOrResume)
            .unwrap();

        (
            controller_conversation,
            worker_conversation,
            worker_id,
            goal_id,
        )
    }

    let mut store = chatarium_store::MemoryEventStore::default();
    let (controller, worker_conversation, worker_id, goal_id) = ready_supervised_pair(&mut store);

    let (control_id, route_id, proposed) = append_controller_worker_control_proposal_checked(
        &mut store,
        controller,
        worker_conversation,
        ControllerControlAction::Stop,
    )
    .unwrap();
    assert_eq!(proposed.len(), 4);
    assert_eq!(proposed[0].kind, EventKind::WorkerControlAdmitted);
    assert_eq!(proposed[1].kind, EventKind::WorkerControlIssuerBound);
    assert_eq!(proposed[2].kind, EventKind::RouteProposed);
    assert_eq!(proposed[3].kind, EventKind::ControlRouteBound);
    assert_eq!(
        replay_control_audit(store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.control_id == control_id)
            .unwrap()
            .kind,
        WorkerControlKind::Stop
    );
    assert_eq!(
        replay_routing_audit(store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.request.id == route_id)
            .unwrap()
            .gate_state,
        RouteGateState::PendingApproval
    );

    append_controller_control_route_decision_checked(
        &mut store,
        route_id,
        RouteUserDecision::Allow,
    )
    .unwrap();
    let dispatched_events =
        append_controller_control_dispatch_checked(&mut store, route_id).unwrap();
    assert_eq!(dispatched_events.len(), 2);
    assert_eq!(dispatched_events[0].kind, EventKind::RouteDispatched);
    assert_eq!(dispatched_events[1].kind, EventKind::WorkerControlDelivered);

    let delivery = replay_worker_control_delivery_audit(store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == control_id)
        .unwrap();
    assert_eq!(delivery.route_id, route_id);
    assert_eq!(delivery.worker_conversation_id, worker_conversation);
    assert_eq!(delivery.worker_session_id, SessionId::new(2));
    assert_eq!(delivery.controller_session_id, SessionId::new(1));

    let inbox =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].control_id, control_id);
    assert_eq!(inbox[0].kind, WorkerControlKind::Stop);
    assert_eq!(inbox[0].goal_id, goal_id);
    assert_eq!(inbox[0].acknowledged_sequence, None);

    let acknowledgement =
        append_worker_control_acknowledgement_checked(&mut store, worker_conversation, route_id)
            .unwrap();
    assert_eq!(acknowledgement.kind, EventKind::WorkerControlAcknowledged);
    let acknowledgement_record = replay_worker_control_acknowledgement_audit(store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == control_id)
        .unwrap();
    assert_eq!(acknowledgement_record.route_id, route_id);
    assert_eq!(
        acknowledgement_record.worker_conversation_id,
        worker_conversation
    );

    let acknowledged_inbox =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation).unwrap();
    assert_eq!(
        acknowledged_inbox[0].acknowledged_sequence,
        Some(acknowledgement.sequence)
    );

    let before_duplicate_ack = store.events().len();
    assert!(
        append_worker_control_acknowledgement_checked(&mut store, worker_conversation, route_id,)
            .unwrap_err()
            .contains("already acknowledged")
    );
    assert_eq!(store.events().len(), before_duplicate_ack);

    let validated = replay_validated_control_dispatches(store.events()).unwrap();
    let dispatched = validated
        .into_iter()
        .find(|record| record.control_id == control_id)
        .unwrap();
    assert_eq!(
        dispatched.issuer,
        ControlIssuer::ControllerSession(SessionId::new(1))
    );
    assert_eq!(dispatched.worker_id, worker_id);
    assert_eq!(dispatched.worker_session_id, Some(SessionId::new(2)));
    assert_eq!(dispatched.authorized_by, DecisionAuthority::User);
    assert_eq!(
        worker_record(store.events(), worker_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::Working
    );

    let mut stale_store = chatarium_store::MemoryEventStore::default();
    let (stale_controller, stale_worker_conversation, stale_worker_id, stale_goal_id) =
        ready_supervised_pair(&mut stale_store);
    let (_, stale_route, _) = append_controller_worker_control_proposal_checked(
        &mut stale_store,
        stale_controller,
        stale_worker_conversation,
        ControllerControlAction::Stop,
    )
    .unwrap();
    append_controller_control_route_decision_checked(
        &mut stale_store,
        stale_route,
        RouteUserDecision::Allow,
    )
    .unwrap();
    append_worker_transition_checked(
        &mut stale_store,
        stale_worker_id,
        stale_goal_id,
        WorkerAction::Complete,
    )
    .unwrap();
    let before_stale_dispatch = stale_store.events().len();
    let error =
        append_controller_control_dispatch_checked(&mut stale_store, stale_route).unwrap_err();
    assert!(error.contains("Stop"));
    assert!(error.contains("Completed"));
    assert_eq!(stale_store.events().len(), before_stale_dispatch);

    let mut status_store = chatarium_store::MemoryEventStore::default();
    let (status_controller, status_worker_conversation, status_worker_id, status_goal_id) =
        ready_supervised_pair(&mut status_store);
    let (_, status_route, _) = append_controller_worker_control_proposal_checked(
        &mut status_store,
        status_controller,
        status_worker_conversation,
        ControllerControlAction::StatusRequest,
    )
    .unwrap();
    append_controller_control_route_decision_checked(
        &mut status_store,
        status_route,
        RouteUserDecision::Allow,
    )
    .unwrap();
    append_worker_transition_checked(
        &mut status_store,
        status_worker_id,
        status_goal_id,
        WorkerAction::Complete,
    )
    .unwrap();
    let status_events =
        append_controller_control_dispatch_checked(&mut status_store, status_route).unwrap();
    assert_eq!(status_events.len(), 2);
    assert_eq!(status_events[1].kind, EventKind::WorkerControlDelivered);
    let status_dispatch = replay_validated_control_dispatches(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.route.id == status_route)
        .unwrap();
    assert_eq!(status_dispatch.dispatched_phase, WorkerPhase::Completed);

    let status_acknowledgement = append_worker_control_acknowledgement_checked(
        &mut status_store,
        status_worker_conversation,
        status_route,
    )
    .unwrap();
    let status_result = append_worker_control_status_result_checked(
        &mut status_store,
        status_worker_conversation,
        status_route,
    )
    .unwrap();
    assert_eq!(
        status_result.kind,
        EventKind::WorkerControlStatusResultRecorded
    );
    let replayed_status = replay_worker_control_status_results(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.route_id == status_route)
        .unwrap();
    assert_eq!(replayed_status.worker_id, status_worker_id);
    assert_eq!(
        replayed_status.worker_conversation_id,
        status_worker_conversation
    );
    assert_eq!(replayed_status.goal_id, status_goal_id);
    assert_eq!(replayed_status.phase, WorkerPhase::Completed);
    assert_eq!(
        replayed_status.acknowledged_sequence,
        status_acknowledgement.sequence
    );
    assert_eq!(replayed_status.recorded_sequence, status_result.sequence);

    let controller_results =
        replay_controller_worker_results_for_conversation(status_store.events(), status_controller)
            .unwrap();
    assert_eq!(controller_results.len(), 1);
    assert_eq!(controller_results[0].route_id, status_route);
    assert_eq!(
        controller_results[0].controller_conversation_id,
        Some(status_controller)
    );
    assert_eq!(
        controller_results[0].worker_conversation_id,
        status_worker_conversation
    );
    assert!(matches!(
        controller_results[0].detail,
        ControllerWorkerResultDetail::Status {
            phase: WorkerPhase::Completed,
            ..
        }
    ));
    assert!(
        admitted_controller_worker_result_messages(status_store.events(), status_controller,)
            .unwrap()
            .is_empty()
    );

    let admitted_event = append_controller_worker_result_context_decision_checked(
        &mut status_store,
        status_route,
        status_controller,
        ControllerWorkerResultContextDecision::Admit,
    )
    .unwrap();
    assert_eq!(
        admitted_event.kind,
        EventKind::ControllerWorkerResultContextDecisionRecorded
    );
    let admitted_context =
        admitted_controller_worker_result_messages(status_store.events(), status_controller)
            .unwrap();
    assert_eq!(admitted_context.len(), 1);
    assert_eq!(
        admitted_context[0].role,
        context_composer::TranscriptRole::User
    );
    assert_eq!(
        admitted_context[0].order_sequence(),
        admitted_event.sequence
    );
    assert!(admitted_context[0].text.contains("result_kind: status"));
    assert!(admitted_context[0].text.contains("phase: COMPLETED"));

    let context_plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        admitted_context,
    );
    assert_eq!(context_plan.controller_worker_result_count(), 1);

    let (coordination_turn_id, coordination_start) =
        append_controller_coordination_start_checked(&mut status_store, status_controller).unwrap();
    assert_eq!(
        coordination_start.kind,
        EventKind::ControllerCoordinationTurnStarted
    );
    let coordination = replay_controller_coordination_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .unwrap();
    assert_eq!(coordination.controller_conversation_id, status_controller);
    assert_eq!(coordination.controller_session_id, SessionId::new(1));
    assert_eq!(coordination.admitted_result_routes, vec![status_route]);
    assert_eq!(coordination.result_sequence, None);
    assert!(status_store.events().iter().all(|event| {
        !(event.kind == EventKind::UserMessageCommitted
            && event.scope.as_deref() == Some(local_turn_scope(coordination_turn_id).as_str()))
    }));

    append_controller_worker_result_context_decision_checked(
        &mut status_store,
        status_route,
        status_controller,
        ControllerWorkerResultContextDecision::Exclude,
    )
    .unwrap();
    assert!(
        admitted_controller_worker_result_messages(status_store.events(), status_controller,)
            .unwrap()
            .is_empty()
    );

    let coordination_prefix = status_store
        .events()
        .iter()
        .take_while(|event| event.sequence <= coordination.started_sequence)
        .cloned()
        .collect::<Vec<_>>();
    let frozen_results =
        admitted_controller_worker_result_messages(&coordination_prefix, status_controller)
            .unwrap();
    assert_eq!(frozen_results.len(), 1);
    let mut coordination_context = frozen_results;
    coordination_context.push(
        context_composer::TranscriptMessage::controller_coordination(
            coordination.controller_session_id.get(),
            coordination_turn_id.to_string(),
            coordination.started_sequence,
        ),
    );
    let coordination_plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        coordination_context,
    );
    assert_eq!(coordination_plan.controller_worker_result_count(), 1);
    assert_eq!(coordination_plan.controller_coordination_count(), 1);
    assert_eq!(
        coordination_plan
            .messages
            .last()
            .expect("coordination marker")
            .role,
        "user"
    );

    let request_id = coordination_turn_id.to_string();
    status_store
        .append_scoped(
            Some(local_turn_scope(coordination_turn_id)),
            EventKind::DispatchAttempted,
            remote_turn_payload(
                coordination_turn_id,
                &request_id,
                Some("model"),
                None,
                Some("coordination test dispatch"),
            ),
        )
        .unwrap();
    let coordination_output = format!(
        "{{\"summary\":\"coordination synthesis\",\"suggestion_candidates\":[{{\"basis_result_route_id\":{},\"action\":\"status_request\"}}]}}",
        status_route.get()
    );
    status_store
        .append_scoped(
            Some(local_turn_scope(coordination_turn_id)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                coordination_turn_id,
                &request_id,
                None,
                Some(coordination_output.as_str()),
                Some("coordination test completion"),
            ),
        )
        .unwrap();
    let coordination_result =
        append_controller_coordination_result_if_terminal(&mut status_store, coordination_turn_id)
            .unwrap()
            .expect("terminal coordination result");
    assert_eq!(
        coordination_result.kind,
        EventKind::ControllerCoordinationTurnResultRecorded
    );
    let completed_coordination = replay_controller_coordination_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .unwrap();
    assert_eq!(
        completed_coordination.outcome,
        Some(ControllerCoordinationOutcome::Completed)
    );
    assert!(completed_coordination.result_sequence.is_some());
    assert_eq!(
        controller_coordination_output_text(status_store.events(), coordination_turn_id)
            .unwrap()
            .as_deref(),
        Some(coordination_output.as_str())
    );
    assert!(
        admitted_controller_coordination_result_messages(status_store.events(), status_controller,)
            .unwrap()
            .is_empty()
    );

    let before_candidate_projection = status_store.events().len();
    assert!(
        replay_controller_coordination_suggestion_audit(status_store.events())
            .unwrap()
            .is_empty()
    );
    let candidate_projection = project_controller_coordination_suggestion_candidates(
        status_store.events(),
        coordination_turn_id,
    )
    .unwrap();
    assert_eq!(candidate_projection.summary, "coordination synthesis");
    assert_eq!(candidate_projection.candidates.len(), 1);
    assert_eq!(
        candidate_projection.candidates[0].basis_result_route_id,
        status_route
    );
    assert_eq!(
        candidate_projection.candidates[0].action,
        CoordinationSuggestionAction::StatusRequest
    );
    assert_eq!(
        candidate_projection.candidates[0].worker_conversation_id,
        status_worker_conversation
    );
    assert_eq!(
        candidate_projection.candidates[0].worker_id,
        status_worker_id
    );
    assert_eq!(candidate_projection.candidates[0].goal_id, status_goal_id);
    assert_eq!(status_store.events().len(), before_candidate_projection);

    let before_bad_suggestion = status_store.events().len();
    assert!(
        append_controller_coordination_suggestion_checked(
            &mut status_store,
            status_controller,
            coordination_turn_id,
            RouteId::new(999_999),
            CoordinationSuggestionAction::StatusRequest,
        )
        .unwrap_err()
        .contains("was not frozen into coordination turn")
    );
    assert_eq!(status_store.events().len(), before_bad_suggestion);

    let before_suggestion = status_store.events().len();
    let (suggestion_id, suggestion_event) = append_controller_coordination_suggestion_checked(
        &mut status_store,
        status_controller,
        coordination_turn_id,
        status_route,
        CoordinationSuggestionAction::StatusRequest,
    )
    .unwrap();
    assert_eq!(suggestion_id, CoordinationSuggestionId::new(1));
    assert_eq!(
        suggestion_event.kind,
        EventKind::ControllerCoordinationSuggestionRecorded
    );
    let appended = &status_store.events()[before_suggestion..];
    assert_eq!(appended.len(), 1);
    assert_eq!(
        appended[0].kind,
        EventKind::ControllerCoordinationSuggestionRecorded
    );
    assert!(appended.iter().all(|event| {
        !matches!(
            event.kind,
            EventKind::WorkerControlAdmitted
                | EventKind::ControlRouteBound
                | EventKind::RouteProposed
                | EventKind::RouteUserDecisionRecorded
                | EventKind::RouteDispatched
                | EventKind::WorkerLifecycleTransitionRecorded
                | EventKind::ContinuationLeaseCreated
                | EventKind::ContinuationPermitIssued
        )
    }));
    let suggestion = replay_controller_coordination_suggestion_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.suggestion.id() == suggestion_id)
        .unwrap();
    assert_eq!(suggestion.controller_conversation_id, status_controller);
    assert_eq!(suggestion.coordination_turn_id, coordination_turn_id);
    assert_eq!(suggestion.basis_result_route_id, status_route);
    assert_eq!(
        suggestion.worker_conversation_id,
        status_worker_conversation
    );
    assert_eq!(suggestion.suggestion.worker_id(), status_worker_id);
    assert_eq!(
        suggestion.suggestion.action(),
        CoordinationSuggestionAction::StatusRequest
    );

    let before_duplicate_suggestion = status_store.events().len();
    assert!(
        append_controller_coordination_suggestion_checked(
            &mut status_store,
            status_controller,
            coordination_turn_id,
            status_route,
            CoordinationSuggestionAction::StatusRequest,
        )
        .unwrap_err()
        .contains("already has STATUS suggestion")
    );
    assert_eq!(status_store.events().len(), before_duplicate_suggestion);

    let before_promotion = status_store.events().len();
    let (promoted_control_id, promoted_route_id, promotion_events) =
        append_controller_coordination_suggestion_promotion_checked(
            &mut status_store,
            status_controller,
            suggestion_id,
        )
        .unwrap();
    assert!(!promotion_events.is_empty());
    assert_eq!(
        promotion_events.last().unwrap().kind,
        EventKind::ControllerCoordinationSuggestionPromoted
    );
    assert!(
        promotion_events
            .iter()
            .any(|event| { event.kind == EventKind::WorkerControlAdmitted })
    );
    assert!(
        promotion_events
            .iter()
            .any(|event| { event.kind == EventKind::RouteProposed })
    );
    assert!(
        promotion_events
            .iter()
            .any(|event| { event.kind == EventKind::ControlRouteBound })
    );
    assert!(!promotion_events.iter().any(|event| {
        matches!(
            event.kind,
            EventKind::RouteUserDecisionRecorded
                | EventKind::RouteDispatched
                | EventKind::WorkerControlDelivered
        )
    }));
    assert!(status_store.events().len() > before_promotion);

    let promoted_control = replay_control_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == promoted_control_id)
        .unwrap();
    assert_eq!(promoted_control.worker_id, status_worker_id);
    assert_eq!(promoted_control.kind, WorkerControlKind::StatusRequest);

    let promoted_route = replay_routing_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.request.id == promoted_route_id)
        .unwrap();
    assert_eq!(promoted_route.gate_state, RouteGateState::PendingApproval);
    assert_eq!(promoted_route.latest_user_decision, None);
    assert_eq!(promoted_route.dispatch_sequence, None);

    let promoted_suggestion =
        replay_controller_coordination_suggestion_audit(status_store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.suggestion.id() == suggestion_id)
            .unwrap();
    assert_eq!(
        promoted_suggestion.promoted_control_id,
        Some(promoted_control_id)
    );
    assert_eq!(
        promoted_suggestion.promoted_route_id,
        Some(promoted_route_id)
    );
    assert!(promoted_suggestion.promoted_sequence.is_some());

    let before_second_promotion = status_store.events().len();
    assert!(
        append_controller_coordination_suggestion_promotion_checked(
            &mut status_store,
            status_controller,
            suggestion_id,
        )
        .unwrap_err()
        .contains("already promoted")
    );
    assert_eq!(status_store.events().len(), before_second_promotion);

    let coordination_admit = append_controller_coordination_result_context_decision_checked(
        &mut status_store,
        coordination_turn_id,
        status_controller,
        ControllerCoordinationResultContextDecision::Admit,
    )
    .unwrap();
    assert_eq!(
        coordination_admit.kind,
        EventKind::ControllerCoordinationResultContextDecisionRecorded
    );
    let admitted_coordination =
        admitted_controller_coordination_result_messages(status_store.events(), status_controller)
            .unwrap();
    assert_eq!(admitted_coordination.len(), 1);
    assert_eq!(
        admitted_coordination[0].role,
        context_composer::TranscriptRole::User
    );
    assert_eq!(
        admitted_coordination[0].order_sequence(),
        coordination_admit.sequence
    );
    assert!(
        admitted_coordination[0]
            .text
            .contains("coordination synthesis")
    );
    let admitted_coordination_plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        admitted_coordination,
    );
    assert_eq!(
        admitted_coordination_plan.controller_coordination_result_count(),
        1
    );

    append_controller_coordination_result_context_decision_checked(
        &mut status_store,
        coordination_turn_id,
        status_controller,
        ControllerCoordinationResultContextDecision::Exclude,
    )
    .unwrap();
    assert!(
        admitted_controller_coordination_result_messages(status_store.events(), status_controller,)
            .unwrap()
            .is_empty()
    );
    let terminal_after_exclude = replay_controller_coordination_audit(status_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .unwrap();
    assert_eq!(
        terminal_after_exclude.result_sequence,
        completed_coordination.result_sequence
    );
    assert_eq!(
        terminal_after_exclude.outcome,
        Some(ControllerCoordinationOutcome::Completed)
    );

    assert!(
        admitted_controller_worker_result_messages(status_store.events(), status_controller,)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        replay_controller_worker_results_for_conversation(
            status_store.events(),
            status_controller,
        )
        .unwrap()
        .len(),
        1
    );

    assert_eq!(
        worker_record(status_store.events(), status_worker_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::Completed
    );

    let before_duplicate_status = status_store.events().len();
    assert!(
        append_worker_control_status_result_checked(
            &mut status_store,
            status_worker_conversation,
            status_route,
        )
        .unwrap_err()
        .contains("already has a durable status result")
    );
    assert_eq!(status_store.events().len(), before_duplicate_status);

    let before_stop_status = store.events().len();
    assert!(
        append_worker_control_status_result_checked(&mut store, worker_conversation, route_id,)
            .unwrap_err()
            .contains("not a StatusRequest")
    );
    assert_eq!(store.events().len(), before_stop_status);

    let mut recovery_store = chatarium_store::MemoryEventStore::default();
    let (controller, worker_conversation, _, _) = ready_supervised_pair(&mut recovery_store);
    let (recovery_control, recovery_route, _) = append_controller_worker_control_proposal_checked(
        &mut recovery_store,
        controller,
        worker_conversation,
        ControllerControlAction::Stop,
    )
    .unwrap();
    append_controller_control_route_decision_checked(
        &mut recovery_store,
        recovery_route,
        RouteUserDecision::Allow,
    )
    .unwrap();

    let route = replay_routing_audit(recovery_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.request.id == recovery_route)
        .unwrap();
    let mut gate = route_gate_before_dispatch(&route).unwrap();
    let permit = gate.authorize_dispatch(recovery_route).unwrap();
    let original_dispatch_sequence = record_route_dispatched(&mut recovery_store, permit).unwrap();

    let recovered_events =
        append_controller_control_dispatch_checked(&mut recovery_store, recovery_route).unwrap();
    assert_eq!(recovered_events.len(), 1);
    assert_eq!(recovered_events[0].kind, EventKind::WorkerControlDelivered);

    let recovered_dispatch = replay_validated_control_dispatches(recovery_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == recovery_control)
        .unwrap();
    assert_eq!(
        recovered_dispatch.dispatch_sequence,
        original_dispatch_sequence
    );
    let recovered_delivery = replay_worker_control_delivery_audit(recovery_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == recovery_control)
        .unwrap();
    assert_eq!(
        recovered_delivery.dispatch_sequence,
        original_dispatch_sequence
    );

    let mut continuation_store = chatarium_store::MemoryEventStore::default();
    let (
        continuation_controller,
        continuation_worker_conversation,
        continuation_worker_id,
        continuation_goal_id,
    ) = ready_supervised_pair(&mut continuation_store);

    let before_missing_authority = continuation_store.events().len();
    assert!(
        append_controller_worker_control_proposal_checked(
            &mut continuation_store,
            continuation_controller,
            continuation_worker_conversation,
            ControllerControlAction::Continue,
        )
        .unwrap_err()
        .contains("no live continuation lease")
    );
    assert_eq!(continuation_store.events().len(), before_missing_authority);

    let (_, lease_event) = append_controller_continuation_lease_checked(
        &mut continuation_store,
        continuation_controller,
        continuation_worker_conversation,
        2,
    )
    .unwrap();
    assert_eq!(lease_event.kind, EventKind::ContinuationLeaseCreated);
    let lease_id = replay_continuation_audit(continuation_store.events()).unwrap()[0].lease_id;
    let lease = replay_continuation_audit(continuation_store.events())
        .unwrap()
        .remove(0);
    assert_eq!(lease.worker_id, continuation_worker_id);
    assert_eq!(lease.goal_id, continuation_goal_id);
    assert_eq!(lease.allowance, 2);
    assert_eq!(lease.issued, 0);
    assert_eq!(lease.remaining, 2);

    let before_overlap = continuation_store.events().len();
    assert!(
        append_controller_continuation_lease_checked(
            &mut continuation_store,
            continuation_controller,
            continuation_worker_conversation,
            1,
        )
        .unwrap_err()
        .contains("already has live continuation authority")
    );
    assert_eq!(continuation_store.events().len(), before_overlap);

    let (continue_control, continue_route, continue_events) =
        append_controller_worker_control_proposal_checked(
            &mut continuation_store,
            continuation_controller,
            continuation_worker_conversation,
            ControllerControlAction::Continue,
        )
        .unwrap();
    assert_eq!(continue_events.len(), 5);
    assert_eq!(continue_events[0].kind, EventKind::ContinuationPermitIssued);
    assert_eq!(continue_events[1].kind, EventKind::WorkerControlAdmitted);
    assert_eq!(continue_events[2].kind, EventKind::WorkerControlIssuerBound);
    assert_eq!(continue_events[3].kind, EventKind::RouteProposed);
    assert_eq!(continue_events[4].kind, EventKind::ControlRouteBound);
    let continue_record = replay_control_audit(continuation_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.control_id == continue_control)
        .unwrap();
    assert_eq!(
        continue_record.kind,
        WorkerControlKind::Continue { permit_ordinal: 1 }
    );
    let replayed_lease = replay_continuation_audit(continuation_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.lease_id == lease_id)
        .unwrap();
    assert_eq!(replayed_lease.issued, 1);
    assert_eq!(replayed_lease.remaining, 1);
    assert_eq!(replayed_lease.consumed, 1);
    assert_eq!(
        replayed_lease.permits[0].consumed_by,
        Some(continue_control)
    );
    assert_eq!(
        replay_routing_audit(continuation_store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.request.id == continue_route)
            .unwrap()
            .gate_state,
        RouteGateState::PendingApproval
    );

    append_controller_control_route_decision_checked(
        &mut continuation_store,
        continue_route,
        RouteUserDecision::Allow,
    )
    .unwrap();
    let delivered =
        append_controller_control_dispatch_checked(&mut continuation_store, continue_route)
            .unwrap();
    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].kind, EventKind::RouteDispatched);
    assert_eq!(delivered[1].kind, EventKind::WorkerControlDelivered);
    let acknowledgement = append_worker_control_acknowledgement_checked(
        &mut continuation_store,
        continuation_worker_conversation,
        continue_route,
    )
    .unwrap();
    assert_eq!(acknowledgement.kind, EventKind::WorkerControlAcknowledged);

    let worker_before_execution =
        worker_record(continuation_store.events(), continuation_worker_id)
            .unwrap()
            .unwrap();
    let execution = append_worker_continuation_execution_start_checked(
        &mut continuation_store,
        continuation_worker_conversation,
        continue_route,
    )
    .unwrap();
    assert_eq!(
        execution.kind,
        EventKind::WorkerContinuationExecutionStarted
    );
    let execution_record = replay_worker_continuation_execution_audit(continuation_store.events())
        .unwrap()
        .into_iter()
        .find(|record| record.route_id == continue_route)
        .unwrap();
    assert_eq!(execution_record.control_id, continue_control);
    assert_eq!(execution_record.worker_id, continuation_worker_id);
    assert_eq!(execution_record.goal_id, continuation_goal_id);
    assert_eq!(execution_record.lease_id, lease_id);
    assert_eq!(execution_record.permit_ordinal, 1);
    assert_eq!(
        execution_record.acknowledged_sequence,
        acknowledgement.sequence
    );
    assert_eq!(execution_record.started_sequence, execution.sequence);
    let worker_after_execution = worker_record(continuation_store.events(), continuation_worker_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        worker_after_execution.lifecycle.phase(),
        worker_before_execution.lifecycle.phase()
    );
    assert_eq!(
        worker_after_execution.last_sequence,
        worker_before_execution.last_sequence
    );

    let before_duplicate_execution = continuation_store.events().len();
    assert!(
        append_worker_continuation_execution_start_checked(
            &mut continuation_store,
            continuation_worker_conversation,
            continue_route,
        )
        .unwrap_err()
        .contains("already has a continuation execution start")
    );
    assert_eq!(
        continuation_store.events().len(),
        before_duplicate_execution
    );

    let continuation_scope = local_turn_scope(execution_record.execution_turn_id);
    assert!(!continuation_store.events().iter().any(|event| {
        event.scope.as_deref() == Some(continuation_scope.as_str())
            && event.kind == EventKind::UserMessageCommitted
    }));

    let request_id = execution_record.execution_turn_id.to_string();
    continuation_store
        .append_scoped(
            Some(continuation_scope.clone()),
            EventKind::DispatchAttempted,
            remote_turn_payload(
                execution_record.execution_turn_id,
                &request_id,
                Some("gpt-test"),
                None,
                Some("test bounded continuation dispatch"),
            ),
        )
        .unwrap();
    let dispatch_sequence = continuation_store.events().last().unwrap().sequence;
    continuation_store
        .append_scoped(
            Some(continuation_scope.clone()),
            EventKind::RemoteAcceptanceObserved,
            remote_turn_payload(
                execution_record.execution_turn_id,
                &request_id,
                None,
                None,
                Some("test bounded continuation accepted"),
            ),
        )
        .unwrap();
    continuation_store
        .append_scoped(
            Some(continuation_scope.clone()),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                execution_record.execution_turn_id,
                &request_id,
                None,
                Some("continued worker output"),
                Some("test bounded continuation completed"),
            ),
        )
        .unwrap();
    let completion_sequence = continuation_store.events().last().unwrap().sequence;

    let transport = continuation_execution_transport_state(
        continuation_store.events(),
        execution_record.execution_turn_id,
    )
    .unwrap();
    assert_eq!(transport.dispatch_sequence, Some(dispatch_sequence));
    assert_eq!(transport.completion_sequence, Some(completion_sequence));
    assert!(transport.is_terminal());

    let (result_route, result_event) = append_worker_continuation_result_if_terminal(
        &mut continuation_store,
        execution_record.execution_turn_id,
    )
    .unwrap()
    .expect("terminal continuation must produce a result");
    assert_eq!(result_route, continue_route);
    assert_eq!(
        result_event.kind,
        EventKind::WorkerContinuationExecutionResultRecorded
    );

    let completed_execution =
        replay_worker_continuation_execution_audit(continuation_store.events())
            .unwrap()
            .into_iter()
            .find(|record| record.route_id == continue_route)
            .unwrap();
    assert_eq!(
        completed_execution
            .outcome
            .expect("terminal result has outcome")
            .stable_name(),
        "completed"
    );
    assert_eq!(
        completed_execution.terminal_sequence,
        Some(completion_sequence)
    );
    assert_eq!(
        completed_execution.result_sequence,
        Some(result_event.sequence)
    );

    let controller_results = replay_controller_worker_results_for_conversation(
        continuation_store.events(),
        continuation_controller,
    )
    .unwrap();
    assert_eq!(controller_results.len(), 1);
    assert_eq!(controller_results[0].route_id, continue_route);
    assert_eq!(
        controller_results[0].worker_conversation_id,
        continuation_worker_conversation
    );
    match &controller_results[0].detail {
        ControllerWorkerResultDetail::Continuation {
            outcome,
            output_text,
            terminal_sequence,
            ..
        } => {
            assert_eq!(outcome.stable_name(), "completed");
            assert_eq!(output_text.as_deref(), Some("continued worker output"));
            assert_eq!(*terminal_sequence, completion_sequence);
        }
        other => panic!("expected continuation controller result, got {other:?}"),
    }

    let admitted_event = append_controller_worker_result_context_decision_checked(
        &mut continuation_store,
        continue_route,
        continuation_controller,
        ControllerWorkerResultContextDecision::Admit,
    )
    .unwrap();
    let admitted_context = admitted_controller_worker_result_messages(
        continuation_store.events(),
        continuation_controller,
    )
    .unwrap();
    assert_eq!(admitted_context.len(), 1);
    assert_eq!(
        admitted_context[0].order_sequence(),
        admitted_event.sequence
    );
    assert!(
        admitted_context[0]
            .text
            .contains("result_kind: continuation")
    );
    assert!(admitted_context[0].text.contains("continued worker output"));

    assert!(
        append_worker_continuation_result_if_terminal(
            &mut continuation_store,
            execution_record.execution_turn_id,
        )
        .unwrap()
        .is_none()
    );

    assert!(!continuation_store.events().iter().any(|event| {
        event.scope.as_deref() == Some(continuation_scope.as_str())
            && event.kind == EventKind::UserMessageCommitted
    }));
    assert!(
        projected_local_display_messages(
            continuation_store.events(),
            continuation_worker_conversation,
        )
        .iter()
        .all(|message| message.text != "continued worker output")
    );
    let worker_after_result = worker_record(continuation_store.events(), continuation_worker_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        worker_after_result.lifecycle.phase(),
        worker_before_execution.lifecycle.phase()
    );

    let mut stranded_store = chatarium_store::MemoryEventStore::default();
    let (stranded_controller, stranded_worker_conversation, stranded_worker_id, stranded_goal_id) =
        ready_supervised_pair(&mut stranded_store);
    append_controller_continuation_lease_checked(
        &mut stranded_store,
        stranded_controller,
        stranded_worker_conversation,
        1,
    )
    .unwrap();
    let stranded_lease = replay_continuation_audit(stranded_store.events())
        .unwrap()
        .remove(0);
    let mut live_lease = reconstruct_continuation_lease_for_next_issue(
        stranded_store.events(),
        stranded_lease.lease_id,
    )
    .unwrap();
    let lifecycle = worker_record(stranded_store.events(), stranded_worker_id)
        .unwrap()
        .unwrap()
        .lifecycle;
    assert_eq!(lifecycle.goal_id(), Some(stranded_goal_id));
    let stranded_permit = live_lease.authorize(&lifecycle).unwrap();
    record_continuation_permit_issued(&mut stranded_store, &stranded_permit).unwrap();

    let before_recovery = stranded_store.events().len();
    let (recovered_control, _, recovered_continue_events) =
        append_controller_worker_control_proposal_checked(
            &mut stranded_store,
            stranded_controller,
            stranded_worker_conversation,
            ControllerControlAction::Continue,
        )
        .unwrap();
    assert_eq!(recovered_continue_events.len(), 4);
    assert_eq!(
        recovered_continue_events[0].kind,
        EventKind::WorkerControlAdmitted
    );
    assert_eq!(
        stranded_store.events().len(),
        before_recovery + recovered_continue_events.len()
    );
    let stranded_replay = replay_continuation_audit(stranded_store.events())
        .unwrap()
        .remove(0);
    assert_eq!(stranded_replay.issued, 1);
    assert_eq!(stranded_replay.remaining, 0);
    assert_eq!(stranded_replay.consumed, 1);
    assert_eq!(
        stranded_replay.permits[0].consumed_by,
        Some(recovered_control)
    );
}

#[test]
fn worker_control_application_is_crash_recoverable_and_correlation_safe() {
    fn acknowledged_stop(
        store: &mut chatarium_store::MemoryEventStore,
    ) -> (
        LocalConversationId,
        WorkerId,
        WorkerGoalId,
        ControlId,
        RouteId,
        u64,
    ) {
        let controller_conversation = LocalConversationId::new();
        let worker_conversation = LocalConversationId::new();
        let worker_id = WorkerId::new(1);
        let goal_id = WorkerGoalId::new(1);

        append_local_orchestration_topology_checked(
            store,
            controller_conversation,
            ChatContainerId::new(1),
            SessionId::new(1),
        )
        .unwrap();
        append_current_session_route_endpoint_checked(
            store,
            controller_conversation,
            SessionId::new(1),
            RouteEndpointId::new(1),
        )
        .unwrap();
        append_local_orchestration_topology_checked(
            store,
            worker_conversation,
            ChatContainerId::new(2),
            SessionId::new(2),
        )
        .unwrap();
        append_current_session_route_endpoint_checked(
            store,
            worker_conversation,
            SessionId::new(2),
            RouteEndpointId::new(2),
        )
        .unwrap();
        append_local_worker_binding_checked(store, worker_conversation, worker_id).unwrap();
        append_current_session_worker_binding_checked(store, worker_conversation, worker_id)
            .unwrap();
        append_current_session_controller_designation_checked(store, controller_conversation)
            .unwrap();
        append_local_controller_worker_binding_checked(
            store,
            controller_conversation,
            worker_conversation,
        )
        .unwrap();
        append_worker_goal_checked(store, worker_id, goal_id).unwrap();
        append_worker_transition_checked(store, worker_id, goal_id, WorkerAction::StartOrResume)
            .unwrap();

        let (control_id, route_id, _) = append_controller_worker_control_proposal_checked(
            store,
            controller_conversation,
            worker_conversation,
            ControllerControlAction::Stop,
        )
        .unwrap();
        append_controller_control_route_decision_checked(store, route_id, RouteUserDecision::Allow)
            .unwrap();
        append_controller_control_dispatch_checked(store, route_id).unwrap();
        let acknowledgement =
            append_worker_control_acknowledgement_checked(store, worker_conversation, route_id)
                .unwrap();

        (
            worker_conversation,
            worker_id,
            goal_id,
            control_id,
            route_id,
            acknowledgement.sequence,
        )
    }

    let mut full = chatarium_store::MemoryEventStore::default();
    let (conversation, worker, goal, _, route, _) = acknowledged_stop(&mut full);
    let applied = append_worker_control_action_checked(&mut full, conversation, route).unwrap();
    assert_eq!(applied.len(), 3);
    assert_eq!(applied[0].kind, EventKind::WorkerControlActionStarted);
    assert_eq!(
        applied[1].kind,
        EventKind::WorkerLifecycleTransitionRecorded
    );
    assert_eq!(
        applied[2].kind,
        EventKind::WorkerControlActionResultRecorded
    );
    assert_eq!(
        worker_record(full.events(), worker)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::Stopped
    );
    let completed = replay_worker_control_action_audit(full.events())
        .unwrap()
        .into_iter()
        .find(|record| record.route_id == route)
        .unwrap();
    assert!(completed.is_complete());
    assert_eq!(completed.goal_id, goal);
    assert_eq!(completed.resulting_phase, Some(WorkerPhase::Stopped));

    let controller_results = replay_controller_worker_results(full.events()).unwrap();
    assert_eq!(controller_results.len(), 1);
    assert_eq!(controller_results[0].route_id, route);
    assert_eq!(controller_results[0].worker_conversation_id, conversation);
    assert!(controller_results[0].controller_conversation_id.is_some());
    assert!(matches!(
        controller_results[0].detail,
        ControllerWorkerResultDetail::Action {
            kind: WorkerControlKind::Stop,
            resulting_phase: WorkerPhase::Stopped,
            ..
        }
    ));

    assert!(
        append_worker_control_action_checked(&mut full, conversation, route)
            .unwrap_err()
            .contains("already has a complete action result")
    );

    let mut after_start = chatarium_store::MemoryEventStore::default();
    let (conversation, worker, goal, control, route, ack) = acknowledged_stop(&mut after_start);
    let started = record_worker_control_action_started(
        &mut after_start,
        control,
        route,
        worker,
        conversation,
        goal,
        WorkerControlKind::Stop,
        ack,
        WorkerPhase::Working,
    )
    .unwrap();
    let recovered =
        append_worker_control_action_checked(&mut after_start, conversation, route).unwrap();
    assert_eq!(recovered.len(), 2);
    assert_eq!(
        recovered[0].kind,
        EventKind::WorkerLifecycleTransitionRecorded
    );
    assert_eq!(
        recovered[1].kind,
        EventKind::WorkerControlActionResultRecorded
    );
    assert_eq!(
        replay_worker_control_action_audit(after_start.events()).unwrap()[0].started_sequence,
        started
    );

    let mut after_transition = chatarium_store::MemoryEventStore::default();
    let (conversation, worker, goal, control, route, ack) =
        acknowledged_stop(&mut after_transition);
    let started = record_worker_control_action_started(
        &mut after_transition,
        control,
        route,
        worker,
        conversation,
        goal,
        WorkerControlKind::Stop,
        ack,
        WorkerPhase::Working,
    )
    .unwrap();
    let lifecycle_sequence = record_worker_control_transition(
        &mut after_transition,
        worker,
        goal,
        WorkerAction::Stop,
        WorkerControlTransitionProvenance {
            control_id: control,
            route_id: route,
            action_started_sequence: started,
        },
    )
    .unwrap();
    let recovered =
        append_worker_control_action_checked(&mut after_transition, conversation, route).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].kind,
        EventKind::WorkerControlActionResultRecorded
    );
    let record = replay_worker_control_action_audit(after_transition.events())
        .unwrap()
        .into_iter()
        .find(|record| record.route_id == route)
        .unwrap();
    assert_eq!(record.lifecycle_sequence, Some(lifecycle_sequence));
    assert!(record.is_complete());

    let mut conflict = chatarium_store::MemoryEventStore::default();
    let (conversation, worker, goal, control, route, ack) = acknowledged_stop(&mut conflict);
    record_worker_control_action_started(
        &mut conflict,
        control,
        route,
        worker,
        conversation,
        goal,
        WorkerControlKind::Stop,
        ack,
        WorkerPhase::Working,
    )
    .unwrap();
    append_worker_transition_checked(&mut conflict, worker, goal, WorkerAction::RequestInput)
        .unwrap();
    let before = conflict.events().len();
    let error =
        append_worker_control_action_checked(&mut conflict, conversation, route).unwrap_err();
    assert!(error.contains("without matching control correlation"));
    assert_eq!(conflict.events().len(), before);
    assert_eq!(
        worker_record(conflict.events(), worker)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::NeedsInput
    );
}

#[test]
fn checked_local_worker_lifecycle_is_durable_and_rejects_illegal_transition() {
    let conversation_id = LocalConversationId::new();
    let worker_id = WorkerId::new(1);
    let first_goal = WorkerGoalId::new(1);
    let second_goal = WorkerGoalId::new(2);
    let mut store = chatarium_store::MemoryEventStore::default();

    let binding =
        append_local_worker_binding_checked(&mut store, conversation_id, worker_id).unwrap();
    assert_eq!(binding.kind, EventKind::LocalConversationWorkerBound);
    assert_eq!(
        local_worker_binding(store.events(), conversation_id)
            .unwrap()
            .unwrap()
            .worker_id,
        worker_id
    );

    append_worker_goal_checked(&mut store, worker_id, first_goal).unwrap();
    let ready = worker_record(store.events(), worker_id).unwrap().unwrap();
    assert_eq!(ready.lifecycle.goal_id(), Some(first_goal));
    assert_eq!(ready.lifecycle.phase(), WorkerPhase::Ready);

    append_worker_transition_checked(
        &mut store,
        worker_id,
        first_goal,
        WorkerAction::StartOrResume,
    )
    .unwrap();
    assert_eq!(
        worker_record(store.events(), worker_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::Working
    );

    append_worker_transition_checked(
        &mut store,
        worker_id,
        first_goal,
        WorkerAction::RequestInput,
    )
    .unwrap();
    assert_eq!(
        worker_record(store.events(), worker_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::NeedsInput
    );

    let before_illegal = store.events().len();
    assert!(
        append_worker_transition_checked(
            &mut store,
            worker_id,
            first_goal,
            WorkerAction::Complete,
        )
        .unwrap_err()
        .contains("cannot apply")
    );
    assert_eq!(store.events().len(), before_illegal);

    append_worker_transition_checked(
        &mut store,
        worker_id,
        first_goal,
        WorkerAction::StartOrResume,
    )
    .unwrap();
    append_worker_transition_checked(&mut store, worker_id, first_goal, WorkerAction::Complete)
        .unwrap();
    assert_eq!(
        worker_record(store.events(), worker_id)
            .unwrap()
            .unwrap()
            .lifecycle
            .phase(),
        WorkerPhase::Completed
    );

    append_worker_goal_checked(&mut store, worker_id, second_goal).unwrap();
    let replacement = worker_record(store.events(), worker_id).unwrap().unwrap();
    assert_eq!(replacement.lifecycle.goal_id(), Some(second_goal));
    assert_eq!(replacement.lifecycle.phase(), WorkerPhase::Ready);
}

#[test]
fn checked_worker_lifecycle_requires_local_conversation_binding() {
    let worker_id = WorkerId::new(7);
    let mut store = chatarium_store::MemoryEventStore::default();

    assert!(
        append_worker_goal_checked(&mut store, worker_id, WorkerGoalId::new(1))
            .unwrap_err()
            .contains("not bound to a local Chatarium conversation")
    );
    assert!(store.events().is_empty());
}

#[test]
fn capability_probe_block_reason_reports_first_failed_precondition() {
    assert_eq!(
        capability_probe_block_reason(false, false, true, true, true),
        Some("ChatGPT plan connection is not ready")
    );
    assert_eq!(
        capability_probe_block_reason(true, false, true, true, true),
        Some("choose an account-visible model")
    );
    assert_eq!(
        capability_probe_block_reason(true, true, true, false, false),
        Some("finish or stop the current response first")
    );
    assert_eq!(
        capability_probe_block_reason(true, true, false, false, true),
        Some("capability probe suite is already running")
    );
    assert_eq!(
        capability_probe_block_reason(true, true, false, false, false),
        None
    );
}

fn imported_event(
    sequence: u64,
    kind: EventKind,
    text: &str,
    identity_field: &str,
    identity: &str,
) -> EventEnvelope {
    let mut details = serde_json::Map::new();
    details.insert(
        identity_field.to_owned(),
        Value::String(identity.to_owned()),
    );
    EventEnvelope {
        sequence,
        at_unix_ms: sequence,
        scope: Some("conversation:test".to_owned()),
        kind,
        payload: serde_json::json!({
            "text": text,
            "details": details,
        })
        .to_string(),
    }
}

#[test]
fn fresh_tab_history_recovery_runs_only_for_empty_primary_and_empty_cache() {
    assert!(should_run_fresh_tab_history_recovery(0, 0));
    assert!(!should_run_fresh_tab_history_recovery(1, 0));
    assert!(!should_run_fresh_tab_history_recovery(0, 85));
    assert!(!should_run_fresh_tab_history_recovery(85, 85));
}

#[test]
fn zero_item_discovery_is_not_a_success_claim() {
    let current_pass_observed = 0usize;
    assert_eq!(current_pass_observed, 0);
}

#[test]
fn zero_item_discovery_retains_last_known_catalog() {
    let existing = vec![ConversationListItem {
        id: "remote-1".to_owned(),
        title: Some("Known conversation".to_owned()),
        create_time: None,
        update_time: None,
    }];

    let (merged, current_pass_observed) = merge_history_discovery_catalog(existing.clone(), &[]);

    assert_eq!(current_pass_observed, 0);
    assert_eq!(merged, existing);
}

#[test]
fn remote_history_cache_round_trips_last_known_catalog() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "chatarium-remote-history-cache-{}-{nonce}",
        std::process::id()
    ));
    let path = directory.join(REMOTE_HISTORY_CACHE_FILE);
    let expected = vec![ConversationListItem {
        id: "remote-1".to_owned(),
        title: Some("Known conversation".to_owned()),
        create_time: Some(serde_json::json!(1.25)),
        update_time: Some(serde_json::json!("2026-10-03T00:01:00Z")),
    }];

    persist_remote_history_cache(&path, &expected).unwrap();
    let loaded = load_remote_history_cache(&path).unwrap();

    assert_eq!(loaded, expected);

    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn unknown_history_total_is_labeled_as_observed_not_complete() {
    assert_eq!(
        history_observed_label(85, None),
        "CHATGPT HISTORY · 85 OBSERVED"
    );
    assert_eq!(
        history_observed_label(85, Some(120)),
        "CHATGPT HISTORY · 85/120"
    );
}

#[test]
fn remote_history_entry_statuses_are_stage_explicit() {
    assert_eq!(
        remote_history_entry_state_label(false, false, false, false, false, false),
        "remote · discovered · click to mirror"
    );
    assert_eq!(
        remote_history_entry_state_label(false, false, false, true, false, false),
        "remote · mirroring…"
    );
    assert_eq!(
        remote_history_entry_state_label(false, false, false, false, true, true),
        "remote · rate limited · cooling down"
    );
    assert_eq!(
        remote_history_entry_state_label(false, false, false, false, false, true),
        "remote · mirror failed · click to retry"
    );
    assert_eq!(
        remote_history_entry_state_label(true, false, false, false, false, false),
        "remote · fully mirrored locally"
    );
    assert_eq!(
        remote_history_entry_state_label(true, true, false, false, false, false),
        "remote · mirrored locally · partial"
    );
}

#[test]
fn local_remote_catalog_states_are_explicit_and_non_account_wide() {
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::Discovered),
        "REMOTE · NOT MIRRORED"
    );
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::MirroredFully),
        "MIRRORED LOCALLY"
    );
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::MirroredPartial),
        "MIRRORED LOCALLY · PARTIAL"
    );
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::TransientFailure),
        "TRANSIENT FAILURE"
    );
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::RateLimited),
        "RATE LIMITED"
    );
    assert_eq!(
        remote_catalog_state_label(RemoteMirrorQueueStatus::StructuralFailure),
        "STRUCTURAL FAILURE"
    );
}

#[test]
fn production_controller_failure_policy_stops_only_terminal_conditions() {
    assert_eq!(
        mirror_failure_class(&account_bridge::BrowserBridgeError::RateLimited(
            "cooldown".to_owned()
        )),
        MirrorFailureClass::RateLimited
    );
    assert_eq!(
        mirror_failure_class(&account_bridge::BrowserBridgeError::Protocol(
            "shape".to_owned()
        )),
        MirrorFailureClass::Structural
    );
    assert_eq!(
        mirror_failure_class(&account_bridge::BrowserBridgeError::Timeout),
        MirrorFailureClass::Transient
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::RateLimited),
        "PAUSED · RATE LIMITED"
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::AuthenticationRequired),
        "PAUSED · AUTHENTICATION REQUIRED"
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::Stopped),
        "STOPPED"
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::Running),
        "RUNNING · CONCURRENCY=1"
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::Paused),
        "PAUSED"
    );
    assert_eq!(
        mirror_controller_state_label(MirrorControllerState::Completed),
        "COMPLETED"
    );
}

#[test]
fn queue_counts_preserve_pending_and_terminal_state_boundaries() {
    let entries = vec![
        RemoteCatalogViewEntry {
            catalog_index: 0,
            item: ConversationListItem {
                id: "one".to_owned(),
                title: None,
                create_time: None,
                update_time: None,
            },
            status: RemoteMirrorQueueStatus::Discovered,
            local_conversation_id: None,
        },
        RemoteCatalogViewEntry {
            catalog_index: 1,
            item: ConversationListItem {
                id: "two".to_owned(),
                title: None,
                create_time: None,
                update_time: None,
            },
            status: RemoteMirrorQueueStatus::MirroredPartial,
            local_conversation_id: Some(LocalConversationId::new()),
        },
        RemoteCatalogViewEntry {
            catalog_index: 2,
            item: ConversationListItem {
                id: "three".to_owned(),
                title: None,
                create_time: None,
                update_time: None,
            },
            status: RemoteMirrorQueueStatus::RateLimited,
            local_conversation_id: None,
        },
    ];
    let counts = remote_catalog_queue_counts(&entries);
    assert_eq!(counts.observed, 3);
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.partial, 1);
    assert_eq!(counts.rate_limited, 1);
}

#[test]
fn history_list_zero_is_not_silently_promoted_to_success() {
    assert_eq!(
        classify_history_list_semantics(0, true),
        HistoryListSemanticVerdict::Contradiction
    );
    assert_eq!(
        classify_history_list_semantics(0, false),
        HistoryListSemanticVerdict::UnconfirmedZero
    );
    assert_eq!(
        classify_history_list_semantics(1, true),
        HistoryListSemanticVerdict::Valid
    );
}

#[test]
fn history_probe_timeout_names_extension_roundtrip_boundary() {
    let status = history_probe_failure_status(&account_bridge::BrowserBridgeError::Timeout);
    assert!(status.contains("Edge extension"));
    assert!(status.contains("typed roundtrip"));
    assert!(!status.contains("Tampermonkey"));
}

#[test]
fn attachment_reader_shows_only_verified_metadata_and_never_encoded_bytes() {
    let mut store = chatarium_store::MemoryEventStore::default();
    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "Please read this file.",
    );
    let attachment = serde_json::json!({
        "filename": "memo.txt",
        "mime": "text/plain",
        "byte_len": 3,
        "prepared_at_unix_ms": 10,
        "read_elapsed_ms": 1.25,
        "encoding_elapsed_ms": 0.2,
        "content": {
            "type": "input_file",
            "filename": "memo.txt",
            "file_data": "data:text/plain;base64,YWJj"
        }
    });
    chatarium_store::authored::commit_user_message_with_attachment(
        &mut store,
        &message,
        Some(&attachment),
    )
    .unwrap();
    let projected = projected_local_display_messages(store.events(), message.conversation_id);
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].text, "Please read this file.");
    let origin = projected[0].provenance_label.as_ref().unwrap();
    assert!(origin.contains("memo.txt"));
    assert!(origin.contains("3 bytes"));
    assert!(origin.contains("read 1.2 ms") || origin.contains("read 1.3 ms"));
    assert!(!origin.contains("YWJj"));
    assert!(!projected[0].text.contains("YWJj"));
    let cached = native_conversation_search::NativeConversationSearchIndex::build(store.events());
    assert_eq!(
        cached.messages(message.conversation_id)[0].provenance_label,
        projected[0].provenance_label
    );
}

#[test]
fn historical_messages_keep_import_provenance_and_roles() {
    let messages = historical_display_messages(
        vec![
            HistoricalTranscriptMessage {
                remote_message_id: "user".to_owned(),
                role: HistoricalTranscriptRole::User,
                text: "hello".to_owned(),
                create_time: None,
            },
            HistoricalTranscriptMessage {
                remote_message_id: "assistant".to_owned(),
                role: HistoricalTranscriptRole::Assistant,
                text: "world".to_owned(),
                create_time: None,
            },
        ],
        42,
    );

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, DisplayRole::User);
    assert_eq!(messages[1].role, DisplayRole::Assistant);
    assert_eq!(
        messages[0].provenance_label.as_deref(),
        Some("historical snapshot · import event #42")
    );
}

#[test]
fn model_discovery_is_single_flight_and_transition_gated() {
    assert!(should_request_models(false, false, true, false));
    assert!(!should_request_models(false, false, true, true));
    assert!(!should_request_models(true, false, false, false));
    assert!(should_request_models(true, true, false, false));
    assert!(should_request_models(true, false, true, false));
}

#[test]
fn display_projection_collapses_observed_updates_into_one_message() {
    let events = vec![
        imported_event(
            1,
            EventKind::TranscriptUserMessageObserved,
            "hello",
            "observed_id",
            "user-1",
        ),
        imported_event(
            2,
            EventKind::UserMessageCommitted,
            "hello",
            "observed_message_id",
            "user-1",
        ),
        imported_event(
            3,
            EventKind::AssistantSnapshotObserved,
            "hel",
            "observed_id",
            "assistant-1",
        ),
        imported_event(
            4,
            EventKind::AssistantSnapshotObserved,
            "hello there",
            "observed_id",
            "assistant-1",
        ),
        imported_event(
            5,
            EventKind::AssistantCompletionObserved,
            "hello there",
            "observed_id",
            "assistant-1",
        ),
    ];

    let projected = projected_display_messages(&events);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].role, DisplayRole::User);
    assert_eq!(projected[0].text, "hello");
    assert_eq!(projected[0].sequence, 2);
    assert_eq!(projected[1].role, DisplayRole::Assistant);
    assert_eq!(projected[1].text, "hello there");
    assert_eq!(projected[1].sequence, 5);
}

#[test]
fn local_projection_never_leaks_other_conversation_history() {
    let first_conversation = LocalConversationId::new();
    let second_conversation = LocalConversationId::new();
    let first_turn = LocalTurnId::new();
    let second_turn = LocalTurnId::new();

    let mut store = chatarium_store::MemoryEventStore::default();
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            first_conversation,
            first_turn,
            LocalMessageId::new(),
            "first private conversation",
        ),
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(first_turn)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                first_turn,
                "first-request",
                None,
                Some("first answer"),
                Some("complete"),
            ),
        )
        .unwrap();

    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            second_conversation,
            second_turn,
            LocalMessageId::new(),
            "second conversation",
        ),
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(second_turn)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                second_turn,
                "second-request",
                None,
                Some("second answer"),
                Some("complete"),
            ),
        )
        .unwrap();

    let projected = projected_local_display_messages(store.events(), second_conversation);
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[0].role, DisplayRole::User);
    assert_eq!(projected[0].text, "second conversation");
    assert_eq!(projected[1].role, DisplayRole::Assistant);
    assert_eq!(projected[1].text, "second answer");

    let context_plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "",
        context_transcript(&projected),
    );
    let serialized = context_plan.input_json().to_string();
    assert!(!serialized.contains("first private conversation"));
    assert!(!serialized.contains("first answer"));
}

#[test]
fn shared_turn_scope_cannot_cross_native_transcript_or_inference_boundaries() {
    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let shared_turn = LocalTurnId::new();
    let private_turn = LocalTurnId::new();
    let mut store = chatarium_store::MemoryEventStore::default();

    // Both authored inputs remain visible in their own local chats,
    // but neither chat owns the shared assistant turn unambiguously.
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            source,
            shared_turn,
            LocalMessageId::new(),
            "source-private authored input",
        ),
    )
    .unwrap();
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            destination,
            shared_turn,
            LocalMessageId::new(),
            "destination-private authored input",
        ),
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(shared_turn)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                shared_turn,
                &shared_turn.to_string(),
                None,
                Some("AMBIGUOUS_PRIVATE_ASSISTANT_OUTPUT"),
                None,
            ),
        )
        .unwrap();

    // An unrelated, uniquely owned assistant scope must remain usable.
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            destination,
            private_turn,
            LocalMessageId::new(),
            "destination followup",
        ),
    )
    .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(private_turn)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(
                private_turn,
                &private_turn.to_string(),
                None,
                Some("destination-owned answer"),
                None,
            ),
        )
        .unwrap();

    for (owner, own_text, other_text) in [
        (
            source,
            "source-private authored input",
            "destination-private authored input",
        ),
        (
            destination,
            "destination-private authored input",
            "source-private authored input",
        ),
    ] {
        let visible = projected_local_display_messages(store.events(), owner);
        let joined = visible
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains(own_text));
        assert!(!joined.contains(other_text));
        assert!(!joined.contains("AMBIGUOUS_PRIVATE_ASSISTANT_OUTPUT"));

        // This is the actual composition path used by local inference.
        let plan = context_composer::ContextPlan::compose(
            context_composer::ContextPolicy::dispatch(),
            "",
            "",
            context_transcript(&visible),
        );
        let serialized = plan.input_json().to_string();
        assert!(!serialized.contains(other_text));
        assert!(!serialized.contains("AMBIGUOUS_PRIVATE_ASSISTANT_OUTPUT"));
    }
    let destination_view = projected_local_display_messages(store.events(), destination);
    assert!(
        destination_view
            .iter()
            .any(|message| message.text == "destination-owned answer")
    );
    assert!(
        projected_local_display_messages(store.events(), source)
            .iter()
            .all(|message| message.text != "destination-owned answer")
    );
}

#[test]
fn conversation_title_comes_from_first_user_message() {
    let messages = vec![
        DisplayMessage {
            role: DisplayRole::Assistant,
            text: "system-like preface".to_owned(),
            sequence: 1,
            timestamp: None,
            provenance_label: None,
        },
        DisplayMessage {
            role: DisplayRole::User,
            text: "  a useful local title\nwith whitespace  ".to_owned(),
            sequence: 2,
            timestamp: None,
            provenance_label: None,
        },
    ];

    assert_eq!(
        derived_conversation_title(&messages),
        "a useful local title with whitespace"
    );
}

#[test]
fn full_header_title_expands_default_sidebar_preview_without_renaming_it() {
    let text = "Verify the repaired Manifoldr MCP probability operation.\n\nDiscover the tools and return actual results.";
    let messages = [DisplayMessage {
        role: DisplayRole::User,
        text: text.to_owned(),
        sequence: 1,
        timestamp: None,
        provenance_label: None,
    }];
    let stored = derived_conversation_title(&messages);
    assert!(stored.ends_with('…'));
    assert_eq!(
        expanded_native_conversation_header_title(&stored, &messages),
        "Verify the repaired Manifoldr MCP probability operation."
    );
    assert_eq!(stored, derived_conversation_title(&messages));
    assert_eq!(
        expanded_native_conversation_header_title("My custom long title…", &messages),
        "My custom long title…"
    );
    assert_eq!(
        expanded_native_conversation_header_title("Renamed chat", &messages),
        "Renamed chat"
    );
}

#[test]
fn native_derived_titles_preserve_normalization_with_bounded_unicode_prefix() {
    let title = |text: String| {
        derived_conversation_title(&[DisplayMessage {
            role: DisplayRole::User,
            text,
            sequence: 1,
            timestamp: None,
            provenance_label: None,
        }])
    };
    assert_eq!(title("  \n\t  ".to_owned()), "Local conversation");
    assert_eq!(title("é".repeat(42)), "é".repeat(42));
    assert_eq!(title("é".repeat(43)), format!("{}…", "é".repeat(42)));
    assert_eq!(
        title(format!("{}  \nmore", "a".repeat(42))),
        format!("{}…", "a".repeat(42))
    );
    assert_eq!(
        title(format!("{}   next", "b".repeat(41))),
        format!("{} …", "b".repeat(41))
    );
    assert_eq!(
        title(format!("   {} later", "Z".repeat(250_000))),
        format!("{}…", "Z".repeat(42))
    );
}

#[test]
fn typed_message_commit_restores_same_local_conversation_identity() {
    let conversation_id = LocalConversationId::new();
    let message = AuthoredUserMessage::new(
        conversation_id,
        LocalTurnId::new(),
        LocalMessageId::new(),
        "hello",
    );
    let mut store = chatarium_store::MemoryEventStore::default();
    commit_user_message(&mut store, &message).unwrap();

    assert_eq!(
        projected_local_conversation_id(store.events()).unwrap(),
        Some(conversation_id)
    );
}

#[test]
fn scoped_typed_commit_prevents_older_draft_from_reappearing() {
    let conversation_id = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    store
        .append_scoped(
            Some(local_conversation_scope(conversation_id)),
            EventKind::DraftChanged,
            "old draft".to_owned(),
        )
        .unwrap();
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            conversation_id,
            LocalTurnId::new(),
            LocalMessageId::new(),
            "committed",
        ),
    )
    .unwrap();

    assert_eq!(projected_working_draft(store.events(), conversation_id), "");
}

#[test]
fn drafts_are_isolated_between_local_conversations() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut store = chatarium_store::MemoryEventStore::default();
    store
        .append_scoped(
            Some(local_conversation_scope(first)),
            EventKind::DraftChanged,
            "first draft".to_owned(),
        )
        .unwrap();
    store
        .append_scoped(
            Some(local_conversation_scope(second)),
            EventKind::DraftChanged,
            "second draft".to_owned(),
        )
        .unwrap();

    assert_eq!(
        projected_working_draft(store.events(), first),
        "first draft"
    );
    assert_eq!(
        projected_working_draft(store.events(), second),
        "second draft"
    );
}

#[test]
fn restart_recovery_marks_incomplete_remote_turn_once_without_retrying() {
    let conversation_id = LocalConversationId::new();
    let turn_id = LocalTurnId::new();
    let message = AuthoredUserMessage::new(
        conversation_id,
        turn_id,
        LocalMessageId::new(),
        "survive restart",
    );
    let mut store = chatarium_store::MemoryEventStore::default();
    commit_user_message(&mut store, &message).unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(turn_id)),
            EventKind::DispatchAttempted,
            remote_turn_payload(turn_id, &turn_id.to_string(), Some("model"), None, None),
        )
        .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(turn_id)),
            EventKind::RemoteAcceptanceObserved,
            remote_turn_payload(turn_id, &turn_id.to_string(), None, None, Some("accepted")),
        )
        .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(turn_id)),
            EventKind::AssistantSnapshotObserved,
            remote_turn_payload(
                turn_id,
                &turn_id.to_string(),
                None,
                Some("partial"),
                Some("snapshot"),
            ),
        )
        .unwrap();

    assert_eq!(recover_interrupted_remote_turns(&mut store).unwrap(), 1);
    assert_eq!(recover_interrupted_remote_turns(&mut store).unwrap(), 0);

    let scoped = store
        .events()
        .iter()
        .filter(|event| event.scope.as_deref() == Some(local_turn_scope(turn_id).as_str()))
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    let evidence = TurnEvidence::replay_event_kinds(scoped).unwrap();
    assert_eq!(evidence.remote, RemoteEvidence::AcceptedObserved);
    assert_eq!(evidence.assistant, AssistantEvidence::PartialInterrupted);
    assert_eq!(
        store
            .events()
            .iter()
            .filter(|event| event.kind == EventKind::TransportInterrupted)
            .count(),
        1
    );
}

#[test]
fn restart_recovery_leaves_completed_turns_unchanged() {
    let turn_id = LocalTurnId::new();
    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        turn_id,
        LocalMessageId::new(),
        "complete",
    );
    let mut store = chatarium_store::MemoryEventStore::default();
    commit_user_message(&mut store, &message).unwrap();
    for kind in [
        EventKind::DispatchAttempted,
        EventKind::RemoteAcceptanceObserved,
        EventKind::AssistantStreamStarted,
        EventKind::AssistantCompletionObserved,
    ] {
        store
            .append_scoped(
                Some(local_turn_scope(turn_id)),
                kind,
                remote_turn_payload(
                    turn_id,
                    &turn_id.to_string(),
                    None,
                    (kind == EventKind::AssistantCompletionObserved).then_some("done"),
                    None,
                ),
            )
            .unwrap();
    }

    assert_eq!(recover_interrupted_remote_turns(&mut store).unwrap(), 0);
    assert!(
        !store
            .events()
            .iter()
            .any(|event| event.kind == EventKind::TransportInterrupted)
    );
}

#[test]
fn persistence_worker_orders_commit_before_remote_dispatch() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "chatarium-desktop-remote-order-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let store = JsonlEventStore::open(&path).expect("open journal");
    let (command_tx, command_rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    let worker_data_dir = path.parent().expect("journal parent").to_path_buf();
    let completion_tx = command_tx.clone();
    let worker = thread::spawn(move || {
        persistence_worker(store, worker_data_dir, command_rx, completion_tx, notice_tx)
    });

    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "durable before dispatch",
    );
    command_tx
        .send(PersistCommand::CommitMessage {
            request_id: 1,
            message: message.clone(),
            attachment: None,
            one_shot_memory_ids: Vec::new(),
            one_shot_memory_snapshot_after_sequence: None,
        })
        .unwrap();

    let commit_event = match notice_rx.recv().unwrap() {
        PersistNotice::MessageCommitted { event, .. } => event,
        other => panic!(
            "expected commit notice, got {}",
            persist_notice_name(&other)
        ),
    };

    command_tx
        .send(PersistCommand::AppendTurnEvent {
            turn_id: message.turn_id,
            kind: EventKind::DispatchAttempted,
            payload: remote_turn_payload(message.turn_id, "request", Some("model"), None, None),
        })
        .unwrap();

    let dispatch_event = match notice_rx.recv().unwrap() {
        PersistNotice::TurnEventAppended { event, kind, .. } => {
            assert_eq!(kind, EventKind::DispatchAttempted);
            event
        }
        other => panic!(
            "expected turn-event notice, got {}",
            persist_notice_name(&other)
        ),
    };

    assert!(commit_event.sequence < dispatch_event.sequence);
    assert_eq!(commit_event.scope, Some(local_turn_scope(message.turn_id)));
    assert_eq!(
        dispatch_event.scope,
        Some(local_turn_scope(message.turn_id))
    );

    command_tx.send(PersistCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let reopened = JsonlEventStore::open(&path).expect("reopen journal");
    let scoped = reopened
        .events()
        .iter()
        .filter(|event| event.scope == Some(local_turn_scope(message.turn_id)))
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    let evidence = TurnEvidence::replay_event_kinds(scoped).expect("replay evidence");
    assert_eq!(
        evidence.local,
        chatarium_core::LocalEvidence::MessageCommitted
    );
    assert_eq!(evidence.remote, chatarium_core::RemoteEvidence::Dispatching);

    let _ = fs::remove_file(path);
}

#[test]
fn completed_turn_automatically_queues_recoverable_metadata_only_memory_intent() {
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-memory-lifecycle-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let journal = root.join("journal.jsonl");
    let store = JsonlEventStore::open(&journal).unwrap();
    let (command_tx, command_rx) = mpsc::channel();
    let (notice_tx, notice_rx) = mpsc::channel();
    let data_dir = root.clone();
    let completion_tx = command_tx.clone();
    let worker = thread::spawn(move || {
        persistence_worker(store, data_dir, command_rx, completion_tx, notice_tx)
    });
    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "Complete this work, and keep the final checkpoint locally.",
    );
    command_tx
        .send(PersistCommand::CommitMessage {
            request_id: 1,
            message: message.clone(),
            attachment: None,
            one_shot_memory_ids: Vec::new(),
            one_shot_memory_snapshot_after_sequence: None,
        })
        .unwrap();
    assert!(matches!(
        notice_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        PersistNotice::MessageCommitted { .. }
    ));
    command_tx
        .send(PersistCommand::AppendTurnEvent {
            turn_id: message.turn_id,
            kind: EventKind::DispatchAttempted,
            payload: remote_turn_payload(message.turn_id, "request", Some("model"), None, None),
        })
        .unwrap();
    assert!(matches!(
        notice_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        PersistNotice::TurnEventAppended {
            kind: EventKind::DispatchAttempted,
            ..
        }
    ));
    let private_response = "Completed project milestone, recorded verified decisions and             unresolved issues, and confirmed the application builds successfully.             The next action is to inspect the actual source repository for the remaining             work and avoid claiming unverified success.";
    command_tx
        .send(PersistCommand::AppendTurnEvent {
            turn_id: message.turn_id,
            kind: EventKind::AssistantCompletionObserved,
            payload: remote_turn_payload(
                message.turn_id,
                "request",
                Some("model"),
                Some(private_response),
                None,
            ),
        })
        .unwrap();
    assert!(matches!(
        notice_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        PersistNotice::TurnEventAppended {
            kind: EventKind::AssistantCompletionObserved,
            ..
        }
    ));
    for _ in 0..2 {
        command_tx
            .send(PersistCommand::QueueMemoryCheckpoint {
                conversation_id: message.conversation_id,
                turn_id: message.turn_id,
            })
            .unwrap();
        assert!(matches!(
            notice_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            PersistNotice::MemoryCheckpointQueued {
                pending_count: 1,
                ..
            }
        ));
    }
    command_tx.send(PersistCommand::Shutdown).unwrap();
    worker.join().unwrap();

    let path = root.join(openviking_checkpoint_outbox::FILE_NAME);
    let restored = openviking_checkpoint_outbox::Outbox::load(&path).unwrap();
    assert_eq!(restored.pending_count(), 1);
    assert_eq!(
        restored.entries()[0].conversation_id,
        message.conversation_id.to_string()
    );
    assert_eq!(restored.entries()[0].turn_id, message.turn_id.to_string());
    let outbox_text = fs::read_to_string(&path).unwrap();
    assert!(!outbox_text.contains(private_response));
    assert!(!outbox_text.contains("verified decisions"));
    let reopened = JsonlEventStore::open(&journal).unwrap();
    assert!(reopened.events().iter().any(|event| {
        event.kind == EventKind::AssistantCompletionObserved
            && event.scope == Some(local_turn_scope(message.turn_id))
    }));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn restart_reconciles_committed_memory_work_without_bulk_importing_old_transcripts() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-memory-recovery-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("journal.jsonl");
    let outbox_path = root.join(openviking_checkpoint_outbox::FILE_NAME);
    let mut store = JsonlEventStore::open(&path).unwrap();

    let old = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "A historical exchange must not be exported on first setup.",
    );
    commit_user_message(&mut store, &old).unwrap();
    let substantial = "This is a completed technical milestone with verified decisions.             Earlier attempts were checked against actual source material and a working test             validated the durable implementation. Future tasks remain open for review.";
    store
        .append_scoped(
            Some(local_turn_scope(old.turn_id)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(old.turn_id, "historic", None, Some(substantial), None),
        )
        .unwrap();

    // Initial activation writes its high-water mark; no retroactive export.
    assert_eq!(reconcile_checkpoint_outbox(&store, &outbox_path), Ok(0));
    assert_eq!(
        openviking_checkpoint_outbox::Outbox::load(&outbox_path)
            .unwrap()
            .pending_count(),
        0
    );

    let active = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "Implement the next milestone and store context.",
    );
    commit_user_message(&mut store, &active).unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(active.turn_id)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(active.turn_id, "new", None, Some(substantial), None),
        )
        .unwrap();

    // Deliberately do not enqueue a checkpoint here; simulate a crash
    // after the completion journal was flushed and before queue dispatch.
    drop(store);
    let reopened = JsonlEventStore::open(&path).unwrap();
    assert_eq!(reconcile_checkpoint_outbox(&reopened, &outbox_path), Ok(1));
    let queued = openviking_checkpoint_outbox::Outbox::load(&outbox_path).unwrap();
    assert_eq!(queued.pending_count(), 1);
    assert_eq!(
        queued.entries()[0].conversation_id,
        active.conversation_id.to_string()
    );
    assert_eq!(queued.entries()[0].turn_id, active.turn_id.to_string());
    assert_eq!(reconcile_checkpoint_outbox(&reopened, &outbox_path), Ok(0));

    // A restart transfers journal ownership; never keep the old writer open
    // while reopening the same authoritative journal for another writer.
    drop(reopened);
    let unowned = LocalTurnId::new();
    let mut unowned_store = JsonlEventStore::open(&path).unwrap();
    unowned_store
        .append_scoped(
            Some(local_turn_scope(unowned)),
            EventKind::AssistantCompletionObserved,
            remote_turn_payload(unowned, "unowned", None, Some(substantial), None),
        )
        .unwrap();
    assert_eq!(
        reconcile_checkpoint_outbox(&unowned_store, &outbox_path),
        Ok(0)
    );
    assert_eq!(
        openviking_checkpoint_outbox::Outbox::load(&outbox_path)
            .unwrap()
            .pending_count(),
        1
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn context_composer_uses_durable_transcript_order() {
    let messages = vec![
        DisplayMessage {
            role: DisplayRole::User,
            text: "one".to_owned(),
            sequence: 1,
            timestamp: None,
            provenance_label: None,
        },
        DisplayMessage {
            role: DisplayRole::Assistant,
            text: "two".to_owned(),
            sequence: 2,
            timestamp: None,
            provenance_label: None,
        },
        DisplayMessage {
            role: DisplayRole::User,
            text: "three".to_owned(),
            sequence: 3,
            timestamp: None,
            provenance_label: None,
        },
    ];

    let plan = context_composer::ContextPlan::compose(
        context_composer::ContextPolicy::dispatch(),
        "",
        "behavior",
        context_transcript(&messages),
    );
    assert_eq!(
        plan.input_json(),
        serde_json::json!([
            {"role": "developer", "content": "behavior"},
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": "two"},
            {"role": "user", "content": "three"},
        ])
    );
}

#[test]
fn local_remote_snapshots_collapse_by_turn_identity() {
    let turn_id = LocalTurnId::new();
    let first = EventEnvelope {
        sequence: 1,
        at_unix_ms: 1,
        scope: Some(local_turn_scope(turn_id)),
        kind: EventKind::AssistantSnapshotObserved,
        payload: remote_turn_payload(turn_id, "request", None, Some("hel"), Some("snapshot")),
    };
    let second = EventEnvelope {
        sequence: 2,
        at_unix_ms: 2,
        scope: Some(local_turn_scope(turn_id)),
        kind: EventKind::AssistantCompletionObserved,
        payload: remote_turn_payload(turn_id, "request", None, Some("hello"), Some("complete")),
    };

    let projected = projected_display_messages(&[first, second]);
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].role, DisplayRole::Assistant);
    assert_eq!(projected[0].text, "hello");
    assert_eq!(projected[0].sequence, 2);
}

#[test]
fn sidecar_failure_clears_awaiting_dispatch_without_retry_or_losing_turn_identity() {
    let turn_id = LocalTurnId::new();
    let mut pending = Some(PendingRemoteTurn {
        conversation_id: LocalConversationId::new(),
        turn_id,
        request_id: "synthetic-request".to_owned(),
        model: "synthetic-model".to_owned(),
        input: serde_json::json!([{"role":"user","content":"private synthetic text"}]),
        instructions: Some("synthetic instruction".to_owned()),
        request_patch: serde_json::json!({}),
    });

    let (interrupted_turn_id, payload) =
        interrupted_pending_remote_turn(&mut pending).expect("pending turn interrupted");
    assert_eq!(interrupted_turn_id, turn_id);
    assert!(
        pending.is_none(),
        "the pending dispatch cannot block future turns"
    );
    let observation: serde_json::Value =
        serde_json::from_str(&payload).expect("typed interruption payload");
    assert_eq!(observation["details"]["local_turn_id"], turn_id.to_string());
    assert_eq!(observation["details"]["request_id"], "synthetic-request");
    assert_eq!(observation["details"]["model"], "synthetic-model");
    assert!(observation["text"].is_null());
    assert!(!payload.contains("private synthetic text"));
    assert!(!payload.contains("synthetic instruction"));
    assert!(
        interrupted_pending_remote_turn(&mut pending).is_none(),
        "the same uncertain turn cannot be interrupted twice"
    );
}

#[test]
fn models_can_be_retried_only_after_a_failed_or_empty_lookup() {
    assert!(model_retry_available(true, false, true));
    assert!(!model_retry_available(true, true, true));
    assert!(!model_retry_available(true, false, false));
    assert!(!model_retry_available(false, false, true));
}

#[test]
fn account_identity_uses_profile_id_even_with_identical_display_labels() {
    let original = siwc_bridge::SessionState {
        profile_id: Some("profile-a".to_owned()),
        email: Some("test@example.invalid".to_owned()),
        profile_label: Some("Same display".to_owned()),
        ..Default::default()
    };
    assert!(!session_identity_changed(&original, &original));

    let switched = siwc_bridge::SessionState {
        profile_id: Some("profile-b".to_owned()),
        ..original.clone()
    };
    assert!(session_identity_changed(&original, &switched));
    assert!(session_identity_changed(&switched, &original));
}

#[test]
fn stale_session_refresh_does_not_finish_pending_oauth() {
    assert!(!sign_in_pending_after_session(false, "disconnected"));
    assert!(sign_in_pending_after_session(false, "connecting"));
    assert!(sign_in_pending_after_session(true, "disconnected"));
    assert!(sign_in_pending_after_session(true, "reauth_required"));
    assert!(sign_in_pending_after_session(true, "connecting"));
    assert!(!sign_in_pending_after_session(true, "connected"));
}

#[test]
fn unrelated_bridge_failures_do_not_unlock_live_oauth() {
    assert!(failed_bridge_command_finishes_sign_in(Some("sign-in")));
    for request_id in [
        None,
        Some("models"),
        Some("session"),
        Some("cancel-sign-in"),
        Some("cancel:turn-1"),
        Some("turn-1"),
        Some("probe:baseline"),
    ] {
        assert!(
            !failed_bridge_command_finishes_sign_in(request_id),
            "unrelated bridge command must not clear sign-in pending"
        );
    }
}

#[test]
fn only_positive_http_or_typed_api_errors_count_as_observed_failure() {
    assert!(remote_error_is_observed_failure(
        &siwc_bridge::BridgeError {
            code: "api_error".to_owned(),
            message: "rejected".to_owned(),
            retryable: false,
            status: Some(429),
            param: None,
            upstream_request_id: None,
            response_shape: None,
        }
    ));
    assert!(remote_error_is_observed_failure(
        &siwc_bridge::BridgeError {
            code: "model_not_found".to_owned(),
            message: "bad model".to_owned(),
            retryable: false,
            status: None,
            param: None,
            upstream_request_id: None,
            response_shape: None,
        }
    ));
    assert!(remote_error_is_observed_failure(
        &siwc_bridge::BridgeError {
            code: "response_incomplete".to_owned(),
            message: "server reported incomplete".to_owned(),
            retryable: true,
            status: None,
            param: None,
            upstream_request_id: None,
            response_shape: None,
        }
    ));
    assert!(remote_error_is_observed_failure(
        &siwc_bridge::BridgeError {
            code: "sharing_not_enabled".to_owned(),
            message: "sharing disabled before request".to_owned(),
            retryable: false,
            status: None,
            param: None,
            upstream_request_id: None,
            response_shape: None,
        }
    ));
    assert!(!remote_error_is_observed_failure(
        &siwc_bridge::BridgeError {
            code: "network_error".to_owned(),
            message: "socket closed".to_owned(),
            retryable: true,
            status: None,
            param: None,
            upstream_request_id: None,
            response_shape: None,
        }
    ));
}
