//! Workspace type imports used by the desktop crate and its child modules.
pub(crate) use chatarium_core::chat_container::{ChatContainerId, SessionLifecyclePhase};
pub(crate) use chatarium_core::control::{
    ControlId, WorkerControl, WorkerControlKind, validate_control_admission,
};
pub(crate) use chatarium_core::control_provenance::{ControlIssuer, ControlProvenance};
pub(crate) use chatarium_core::control_route::ControlRouteBinding;
pub(crate) use chatarium_core::coordination_suggestion::{
    CoordinationSuggestion, CoordinationSuggestionAction, CoordinationSuggestionId,
};
pub(crate) use chatarium_core::local_memory::{LocalMemoryId, LocalMemoryLabel};
pub(crate) use chatarium_core::orchestration::{
    ContinuationLease, ContinuationLeaseId, WorkerAction, WorkerGoalId, WorkerId, WorkerLifecycle,
    WorkerPhase,
};
pub(crate) use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteEndpointId, RouteGate, RouteGateState, RouteId,
    RoutePayloadId, RoutePolicy, RouteRequest,
};
pub(crate) use chatarium_core::session::{
    SessionEndpointBinding, SessionId, WorkerSessionBinding, WorkerSessionSuccessorBinding,
};
pub(crate) use chatarium_core::supervision::{ControllerDesignation, ControllerWorkerBinding};
pub(crate) use chatarium_core::tool::{
    StdioToolProviderConfig, ToolCallId, ToolOperationName, ToolProviderEndpointBinding,
    ToolProviderId, ToolProviderName,
};
pub(crate) use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId,
    LocalTurnId, RemoteEvidence, TurnEvidence,
};
pub(crate) use chatarium_protocol::conversation_list::ConversationListItem;
pub(crate) use chatarium_protocol::mcp_schema_draft::draft_required_arguments;
pub(crate) use chatarium_protocol::mcp_wire::{McpResponse, decode_stdio_response, parse_tools_list_page};
pub(crate) use chatarium_protocol::tool_envelope::parse_legacy_tool_call;
pub(crate) use chatarium_store::archive_maintenance::{
    check_archive, create_backup, restore_backup, verify_backup,
};
pub(crate) use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, commit_user_message_with_attachment,
    decode_user_message_attachment, decode_user_message_commit, local_turn_scope,
};
pub(crate) use chatarium_store::chat_container_audit::{
    record_chat_container_created, replay_chat_container_audit,
};
pub(crate) use chatarium_store::continuation_audit::{
    reconstruct_continuation_lease_for_next_issue, reconstruct_unconsumed_continuation_permit,
    record_continuation_lease_created, record_continuation_permit_issued,
    replay_continuation_audit,
};
pub(crate) use chatarium_store::continuation_execution_audit::{
    continuation_execution_transport_state, record_worker_continuation_execution_result,
    record_worker_continuation_execution_started, replay_worker_continuation_execution_audit,
    transport_terminal_outcome,
};
pub(crate) use chatarium_store::control_ack_audit::{
    record_worker_control_acknowledged, replay_worker_control_acknowledgement_audit,
};
pub(crate) use chatarium_store::control_action_audit::{
    action_for_kind, record_worker_control_action_result, record_worker_control_action_started,
    replay_worker_control_action_audit,
};
pub(crate) use chatarium_store::control_admission_audit::validate_control_freshness_before;
pub(crate) use chatarium_store::control_audit::{record_worker_control_admitted, replay_control_audit};
pub(crate) use chatarium_store::control_delivery_audit::{
    record_worker_control_delivered, replay_worker_control_delivery_audit,
};
pub(crate) use chatarium_store::control_dispatch_audit::replay_validated_control_dispatches;
pub(crate) use chatarium_store::control_inbox::replay_worker_control_inbox_for_conversation;
pub(crate) use chatarium_store::control_provenance_audit::{
    record_worker_control_issuer_bound, replay_control_provenance_audit,
};
pub(crate) use chatarium_store::control_result_audit::{
    record_worker_control_status_result, replay_worker_control_status_results,
};
pub(crate) use chatarium_store::control_route_audit::{
    record_control_route_bound, replay_control_route_audit,
};
pub(crate) use chatarium_store::controller_coordination_audit::{
    ControllerCoordinationOutcome, controller_coordination_output_text,
    controller_coordination_transport_state, coordination_terminal_outcome,
    record_controller_coordination_result, record_controller_coordination_started,
    replay_controller_coordination_audit,
};
pub(crate) use chatarium_store::controller_coordination_candidate_projection::project_controller_coordination_suggestion_candidates;
pub(crate) use chatarium_store::controller_coordination_context_audit::{
    ControllerCoordinationResultContextDecision,
    record_controller_coordination_result_context_decision,
    replay_admitted_controller_coordination_result_context,
    replay_controller_coordination_result_context_audit,
};
pub(crate) use chatarium_store::controller_coordination_suggestion_audit::{
    ControllerCoordinationSuggestionRecord, record_controller_coordination_suggestion,
    record_controller_coordination_suggestion_promoted,
    replay_controller_coordination_suggestion_audit,
};
pub(crate) use chatarium_store::controller_result_context_audit::{
    ControllerWorkerResultContextDecision, record_controller_worker_result_context_decision,
    replay_admitted_controller_worker_result_context,
    replay_controller_worker_result_context_audit,
};
pub(crate) use chatarium_store::controller_result_inbox::{
    ControllerWorkerResultDetail, replay_controller_worker_results,
    replay_controller_worker_results_for_conversation,
};
pub(crate) use chatarium_store::historical_transcript::{
    HistoricalConversationCatalogEntry, HistoricalTranscriptMessage, HistoricalTranscriptRole,
    latest_historical_conversation_catalog, load_historical_active_transcript,
};
pub(crate) use chatarium_store::local_conversation_chat_container_audit::{
    LocalConversationTopologyRecord, record_local_conversation_chat_container_bound,
    replay_local_conversation_chat_container_bindings, replay_local_conversation_topologies,
};
pub(crate) use chatarium_store::local_conversation_worker_audit::{
    LocalConversationWorkerBindingRecord, record_local_conversation_worker_bound,
    replay_local_conversation_worker_bindings,
};
pub(crate) use chatarium_store::local_memory_audit::{
    record_local_memory_artifact, replay_local_memory_audit,
};
pub(crate) use chatarium_store::local_memory_context_audit::{
    LocalMemoryContextDecision, record_local_memory_context_decision,
    replay_admitted_local_memory_context, replay_local_memory_context_audit,
};
pub(crate) use chatarium_store::local_memory_label_audit::{
    record_local_memory_label_added, record_local_memory_label_removed,
    replay_local_memory_label_audit,
};
pub(crate) use chatarium_store::local_memory_search::{
    local_memory_label_facets, local_memory_source_facets, search_local_memory_faceted,
};
pub(crate) use chatarium_store::local_memory_supersession_audit::{
    record_local_memory_superseded, replay_local_memory_supersession_audit,
};
pub(crate) use chatarium_store::local_memory_turn_selection_audit::{
    MAX_ONE_SHOT_MEMORIES_PER_TURN, record_local_memory_turn_selection,
    replay_local_memory_turn_selection_audit, validate_local_memory_one_shot_snapshot,
};
pub(crate) use chatarium_store::local_route_context_audit::{
    LocalRouteContextDecision, record_local_route_context_decision,
    replay_admitted_local_route_context, replay_local_route_context_audit,
};
pub(crate) use chatarium_store::local_route_delivery_audit::{
    record_local_route_delivered, replay_local_route_delivery_audit,
};
pub(crate) use chatarium_store::local_route_payload_audit::{
    record_local_route_payload_attached, replay_local_route_payload_audit,
};
pub(crate) use chatarium_store::local_routed_inbox::replay_local_routed_inbox_for_conversation;
pub(crate) use chatarium_store::local_routing_directory::replay_local_routing_directory;
pub(crate) use chatarium_store::orchestration_route_audit::replay_validated_orchestration_routes;
pub(crate) use chatarium_store::remote_health::{
    MirrorIntent, RemoteHealthController, RemoteHealthSignal, record_remote_health_intent,
    record_remote_health_signal,
};
pub(crate) use chatarium_store::remote_mirror_bootstrap::{
    promote_discovered_live_mirror_body, promote_historical_live_mirror_body,
};
pub(crate) use chatarium_store::remote_mirror_queue::{
    RemoteMirrorQueueStatus, derive_remote_mirror_queue,
    record_remote_mirror_queue_capture_started, record_remote_mirror_queue_completed,
    record_remote_mirror_queue_failed, record_remote_mirror_queue_item_queued,
    record_remote_mirror_queue_rate_limited,
};
pub(crate) use chatarium_store::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
pub(crate) use chatarium_store::remote_mirror_transcript::{
    RemoteTranscriptMessage, RemoteTranscriptProjection, RemoteTranscriptRole,
    project_remote_active_transcript,
};
pub(crate) use chatarium_store::routing_audit::{
    RouteAuditRecord, RouteUserDecision, record_route_dispatched, record_route_proposed,
    record_route_user_decision, replay_routing_audit,
};
pub(crate) use chatarium_store::session_audit::{
    active_worker_session, record_local_session_registered, record_session_endpoint_bound,
    record_worker_session_bound, record_worker_session_successor_bound, replay_session_audit,
};
pub(crate) use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
pub(crate) use chatarium_store::supervision_audit::{
    record_controller_session_designated, record_controller_worker_bound, replay_supervision_audit,
};
pub(crate) use chatarium_store::tool_call_audit::{
    record_tool_call, record_tool_call_route_bound, replay_tool_call_audit,
};
pub(crate) use chatarium_store::tool_outcome_audit::{
    ToolCallOutcomeKind, append_tool_call_outcome_checked, replay_tool_call_outcome_audit,
};
pub(crate) use chatarium_store::tool_provider_activation_audit::{
    ProviderActivationDecision, replay_tool_provider_activation_audit,
};
pub(crate) use chatarium_store::tool_provider_audit::{
    record_tool_provider_endpoint_bound, record_tool_provider_registered,
    replay_tool_provider_audit,
};
pub(crate) use chatarium_store::tool_result_context_audit::{
    MAX_CONTEXT_TOOL_RESULT_BYTES, ToolResultContextDecision,
    append_tool_result_context_decision_checked, replay_admitted_tool_results,
    replay_tool_result_context_audit, tool_outcome_owning_conversation,
};
pub(crate) use chatarium_store::tool_stdio_dispatch::{
    replay_unresolved_external_tool_dispatches, reserve_activated_stdio_tool_dispatch,
};
pub(crate) use chatarium_store::tool_stdio_preflight::{
    MCP_LIST_TOOLS_OPERATION, preview_activated_stdio_tool_invocation,
};
pub(crate) use chatarium_store::tool_transport_config_audit::{
    append_tool_transport_config_checked, replay_tool_transport_config_audit,
};
pub(crate) use chatarium_store::worker_audit::{
    WorkerAuditRecord, WorkerControlTransitionProvenance, record_worker_control_transition,
    record_worker_goal_assigned, record_worker_transition, replay_worker_audit,
};
pub(crate) use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
pub(crate) use eframe::egui;
pub(crate) use serde_json::Value;
pub(crate) use std::collections::{BTreeMap, BTreeSet, HashSet};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::mpsc::{self, Receiver, Sender};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::thread::{self, JoinHandle};
pub(crate) use std::time::{Duration, Instant};
