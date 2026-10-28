mod account_bridge;
mod app;
#[cfg(test)]
mod tests;
mod ui;
mod worker;
use worker::persistence_worker;
mod attachment_intake;
mod behavior_profile;
mod capability_probes;
mod context_composer;
mod conversation_keybindings;
mod conversation_navigation;
mod coordination_tool_budget;
mod coordination_wake_scheduler;
mod coordination_workspace;
mod delivery_telemetry;
mod diagnostics;
mod display_preferences;
mod host_shell_mcp;
#[cfg(test)]
mod integration_journey;
mod local_archive_search;
mod local_conversations;
mod local_http_mcp;
mod local_inference_contract;
mod local_inference_settings;
mod local_stdio_host_diagnostics;
mod openviking_checkpoint_outbox;
mod openviking_memory_delivery;
mod openviking_memory_summary;
mod openviking_project_cache;
mod openviking_project_ui;
mod openviking_write_preflight;

mod local_stdio_runner;
mod local_tool_adapter;
mod local_tool_provider_control;
mod markdown_inline;
mod mcp_agent_defaults;
#[cfg(test)]
mod mcp_async_tests;
mod mcp_call_review;
mod mcp_context_inventory;
mod mcp_dispatch_manifest;
mod mcp_output_audit;
mod mcp_provider_workflow;
mod mcp_result_review;
mod mcp_transcript;
mod native_conversation_search;
mod native_transcript_export;
mod network_health;
mod offline_reader;
mod reader_visual_qa;
mod response_text_provenance;
mod send_admission_gate;
mod siwc_activity_transcript;
mod siwc_bridge;
mod system_fonts;
mod timestamp_display;
mod virtual_reader;

// History mirroring is parked. Local archives remain readable and the code is retained.
// This never disables the separate ChatGPT inference/sign-in runtime.
const ACCOUNT_MIRRORING_ENABLED: bool = false;

use chatarium_core::chat_container::{ChatContainerId, SessionLifecyclePhase};
use chatarium_core::control::{
    ControlId, WorkerControl, WorkerControlKind, validate_control_admission,
};
use chatarium_core::control_provenance::{ControlIssuer, ControlProvenance};
use chatarium_core::control_route::ControlRouteBinding;
use chatarium_core::coordination_suggestion::{
    CoordinationSuggestion, CoordinationSuggestionAction, CoordinationSuggestionId,
};
use chatarium_core::local_memory::{LocalMemoryId, LocalMemoryLabel};
use chatarium_core::orchestration::{
    ContinuationLease, ContinuationLeaseId, WorkerAction, WorkerGoalId, WorkerId, WorkerLifecycle,
    WorkerPhase,
};
use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteEndpointId, RouteGate, RouteGateState, RouteId,
    RoutePayloadId, RoutePolicy, RouteRequest,
};
use chatarium_core::session::{
    SessionEndpointBinding, SessionId, WorkerSessionBinding, WorkerSessionSuccessorBinding,
};
use chatarium_core::supervision::{ControllerDesignation, ControllerWorkerBinding};
use chatarium_core::tool::{
    StdioToolProviderConfig, ToolCallId, ToolOperationName, ToolProviderEndpointBinding,
    ToolProviderId, ToolProviderName,
};
use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId,
    LocalTurnId, RemoteEvidence, TurnEvidence,
};
use chatarium_protocol::conversation_list::ConversationListItem;
use chatarium_protocol::mcp_schema_draft::draft_required_arguments;
use chatarium_protocol::mcp_wire::{McpResponse, decode_stdio_response, parse_tools_list_page};
use chatarium_protocol::tool_envelope::parse_legacy_tool_call;
use chatarium_store::archive_maintenance::{
    check_archive, create_backup, restore_backup, verify_backup,
};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, commit_user_message_with_attachment,
    decode_user_message_attachment, decode_user_message_commit, local_turn_scope,
};
use chatarium_store::chat_container_audit::{
    record_chat_container_created, replay_chat_container_audit,
};
use chatarium_store::continuation_audit::{
    reconstruct_continuation_lease_for_next_issue, reconstruct_unconsumed_continuation_permit,
    record_continuation_lease_created, record_continuation_permit_issued,
    replay_continuation_audit,
};
use chatarium_store::continuation_execution_audit::{
    continuation_execution_transport_state, record_worker_continuation_execution_result,
    record_worker_continuation_execution_started, replay_worker_continuation_execution_audit,
    transport_terminal_outcome,
};
use chatarium_store::control_ack_audit::{
    record_worker_control_acknowledged, replay_worker_control_acknowledgement_audit,
};
use chatarium_store::control_action_audit::{
    action_for_kind, record_worker_control_action_result, record_worker_control_action_started,
    replay_worker_control_action_audit,
};
use chatarium_store::control_admission_audit::validate_control_freshness_before;
use chatarium_store::control_audit::{record_worker_control_admitted, replay_control_audit};
use chatarium_store::control_delivery_audit::{
    record_worker_control_delivered, replay_worker_control_delivery_audit,
};
use chatarium_store::control_dispatch_audit::replay_validated_control_dispatches;
use chatarium_store::control_inbox::replay_worker_control_inbox_for_conversation;
use chatarium_store::control_provenance_audit::{
    record_worker_control_issuer_bound, replay_control_provenance_audit,
};
use chatarium_store::control_result_audit::{
    record_worker_control_status_result, replay_worker_control_status_results,
};
use chatarium_store::control_route_audit::{
    record_control_route_bound, replay_control_route_audit,
};
use chatarium_store::controller_coordination_audit::{
    ControllerCoordinationOutcome, controller_coordination_output_text,
    controller_coordination_transport_state, coordination_terminal_outcome,
    record_controller_coordination_result, record_controller_coordination_started,
    replay_controller_coordination_audit,
};
use chatarium_store::controller_coordination_candidate_projection::project_controller_coordination_suggestion_candidates;
use chatarium_store::controller_coordination_context_audit::{
    ControllerCoordinationResultContextDecision,
    record_controller_coordination_result_context_decision,
    replay_admitted_controller_coordination_result_context,
    replay_controller_coordination_result_context_audit,
};
use chatarium_store::controller_coordination_suggestion_audit::{
    ControllerCoordinationSuggestionRecord, record_controller_coordination_suggestion,
    record_controller_coordination_suggestion_promoted,
    replay_controller_coordination_suggestion_audit,
};
use chatarium_store::controller_result_context_audit::{
    ControllerWorkerResultContextDecision, record_controller_worker_result_context_decision,
    replay_admitted_controller_worker_result_context,
    replay_controller_worker_result_context_audit,
};
use chatarium_store::controller_result_inbox::{
    ControllerWorkerResultDetail, replay_controller_worker_results,
    replay_controller_worker_results_for_conversation,
};
use chatarium_store::historical_transcript::{
    HistoricalConversationCatalogEntry, HistoricalTranscriptMessage, HistoricalTranscriptRole,
    latest_historical_conversation_catalog, load_historical_active_transcript,
};
use chatarium_store::local_conversation_chat_container_audit::{
    LocalConversationTopologyRecord, record_local_conversation_chat_container_bound,
    replay_local_conversation_chat_container_bindings, replay_local_conversation_topologies,
};
use chatarium_store::local_conversation_worker_audit::{
    LocalConversationWorkerBindingRecord, record_local_conversation_worker_bound,
    replay_local_conversation_worker_bindings,
};
use chatarium_store::local_memory_audit::{
    record_local_memory_artifact, replay_local_memory_audit,
};
use chatarium_store::local_memory_context_audit::{
    LocalMemoryContextDecision, record_local_memory_context_decision,
    replay_admitted_local_memory_context, replay_local_memory_context_audit,
};
use chatarium_store::local_memory_label_audit::{
    record_local_memory_label_added, record_local_memory_label_removed,
    replay_local_memory_label_audit,
};
use chatarium_store::local_memory_search::{
    local_memory_label_facets, local_memory_source_facets, search_local_memory_faceted,
};
use chatarium_store::local_memory_supersession_audit::{
    record_local_memory_superseded, replay_local_memory_supersession_audit,
};
use chatarium_store::local_memory_turn_selection_audit::{
    MAX_ONE_SHOT_MEMORIES_PER_TURN, record_local_memory_turn_selection,
    replay_local_memory_turn_selection_audit, validate_local_memory_one_shot_snapshot,
};
use chatarium_store::local_route_context_audit::{
    LocalRouteContextDecision, record_local_route_context_decision,
    replay_admitted_local_route_context, replay_local_route_context_audit,
};
use chatarium_store::local_route_delivery_audit::{
    record_local_route_delivered, replay_local_route_delivery_audit,
};
use chatarium_store::local_route_payload_audit::{
    record_local_route_payload_attached, replay_local_route_payload_audit,
};
use chatarium_store::local_routed_inbox::replay_local_routed_inbox_for_conversation;
use chatarium_store::local_routing_directory::replay_local_routing_directory;
use chatarium_store::orchestration_route_audit::replay_validated_orchestration_routes;
use chatarium_store::remote_health::{
    MirrorIntent, RemoteHealthController, RemoteHealthSignal, record_remote_health_intent,
    record_remote_health_signal,
};
use chatarium_store::remote_mirror_bootstrap::{
    promote_discovered_live_mirror_body, promote_historical_live_mirror_body,
};
use chatarium_store::remote_mirror_queue::{
    RemoteMirrorQueueStatus, derive_remote_mirror_queue,
    record_remote_mirror_queue_capture_started, record_remote_mirror_queue_completed,
    record_remote_mirror_queue_failed, record_remote_mirror_queue_item_queued,
    record_remote_mirror_queue_rate_limited,
};
use chatarium_store::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use chatarium_store::remote_mirror_transcript::{
    RemoteTranscriptMessage, RemoteTranscriptProjection, RemoteTranscriptRole,
    project_remote_active_transcript,
};
use chatarium_store::routing_audit::{
    RouteAuditRecord, RouteUserDecision, record_route_dispatched, record_route_proposed,
    record_route_user_decision, replay_routing_audit,
};
use chatarium_store::session_audit::{
    active_worker_session, record_local_session_registered, record_session_endpoint_bound,
    record_worker_session_bound, record_worker_session_successor_bound, replay_session_audit,
};
use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
use chatarium_store::supervision_audit::{
    record_controller_session_designated, record_controller_worker_bound, replay_supervision_audit,
};
use chatarium_store::tool_call_audit::{
    record_tool_call, record_tool_call_route_bound, replay_tool_call_audit,
};
use chatarium_store::tool_outcome_audit::{
    ToolCallOutcomeKind, append_tool_call_outcome_checked, replay_tool_call_outcome_audit,
};
use chatarium_store::tool_provider_activation_audit::{
    ProviderActivationDecision, replay_tool_provider_activation_audit,
};
use chatarium_store::tool_provider_audit::{
    record_tool_provider_endpoint_bound, record_tool_provider_registered,
    replay_tool_provider_audit,
};
use chatarium_store::tool_result_context_audit::{
    MAX_CONTEXT_TOOL_RESULT_BYTES, ToolResultContextDecision,
    append_tool_result_context_decision_checked, replay_admitted_tool_results,
    replay_tool_result_context_audit, tool_outcome_owning_conversation,
};
use chatarium_store::tool_stdio_dispatch::{
    replay_unresolved_external_tool_dispatches, reserve_activated_stdio_tool_dispatch,
};
use chatarium_store::tool_stdio_preflight::{
    MCP_LIST_TOOLS_OPERATION, preview_activated_stdio_tool_invocation,
};
use chatarium_store::tool_transport_config_audit::{
    append_tool_transport_config_checked, replay_tool_transport_config_audit,
};
use chatarium_store::worker_audit::{
    WorkerAuditRecord, WorkerControlTransitionProvenance, record_worker_control_transition,
    record_worker_goal_assigned, record_worker_transition, replay_worker_audit,
};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControllerControlAction {
    StartOrResume,
    Continue,
    Stop,
    StatusRequest,
}

impl ControllerControlAction {
    const fn label(self) -> &'static str {
        match self {
            Self::StartOrResume => "start/resume",
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::StatusRequest => "status request",
        }
    }
}

enum PersistCommand {
    RetrieveConfirmedProjectOpenViking {
        conversation_id: LocalConversationId,
        generation: u64,
        catalog: local_conversations::LocalConversationCatalog,
        grants: openviking_memory_delivery::ProjectMemoryGrants,
        query: String,
    },
    RetrieveConfirmedOpenViking {
        conversation_id: LocalConversationId,
        query: String,
    },
    SaveOpenVikingMemorySummary {
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
        completed_sequence: u64,
        summary: String,
    },
    DispatchOpenVikingReadyWrite,
    OpenVikingWritePreflightFinished {
        selected: (String, String, u64),
        result: Result<(), String>,
    },
    RecordOpenVikingUncertainReadback {
        conversation_id: String,
        turn_id: String,
        completed_sequence: u64,
        result: Result<bool, String>,
    },
    RecordOpenVikingWriteOutcome {
        conversation_id: String,
        turn_id: String,
        completed_sequence: u64,
        result: Result<(), String>,
    },
    ProposeAgentMcpCall {
        turn_id: LocalTurnId,
        source_conversation_id: LocalConversationId,
        proposal: mcp_agent_defaults::ModelToolRequest,
        policy: mcp_agent_defaults::ToolPolicy,
    },
    SaveMcpAgentDefaults {
        defaults: mcp_agent_defaults::AgentDefaults,
    },
    SaveHostShellSettings {
        settings: host_shell_mcp::Settings,
    },
    DispatchHostShell {
        route_id: RouteId,
    },
    DispatchLocalHttpMcp {
        route_id: RouteId,
    },
    QueueMemoryCheckpoint {
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
    },
    RecordLocalHttpMcpOutcome {
        call_id: ToolCallId,
        route_id: RouteId,
        operation: String,
        result: Result<Value, String>,
    },
    RecordHostShellOutcome {
        call_id: ToolCallId,
        route_id: RouteId,
        result: Result<Value, String>,
    },
    SaveInferenceSettings {
        store: local_inference_settings::InferenceSettingsStore,
    },
    SaveBehaviorProfiles {
        store: behavior_profile::BehaviorProfileStore,
    },
    InitializeLocalOrchestrationTopology {
        conversation_id: LocalConversationId,
        container_id: ChatContainerId,
        root_session_id: SessionId,
    },
    BindCurrentSessionRouteEndpoint {
        conversation_id: LocalConversationId,
        session_id: SessionId,
        endpoint_id: RouteEndpointId,
    },
    ProposeLocalSessionRoute {
        route_id: RouteId,
        source_conversation_id: LocalConversationId,
        destination_conversation_id: LocalConversationId,
    },
    AttachLocalSessionRoutePayload {
        payload_id: RoutePayloadId,
        route_id: RouteId,
        text: String,
    },
    DecideLocalSessionRoute {
        route_id: RouteId,
        decision: RouteUserDecision,
    },
    DispatchLocalSessionRoute {
        route_id: RouteId,
    },
    DecideLocalRouteContext {
        route_id: RouteId,
        destination_conversation_id: LocalConversationId,
        decision: LocalRouteContextDecision,
    },
    RegisterToolProvider {
        provider_id: ToolProviderId,
        name: ToolProviderName,
    },
    BindToolProviderEndpoint {
        provider_id: ToolProviderId,
        endpoint_id: RouteEndpointId,
    },
    ConfigureToolProviderTransport {
        provider_id: ToolProviderId,
        config: StdioToolProviderConfig,
    },
    DecideToolProviderActivation {
        provider_id: ToolProviderId,
        configured_sequence: u64,
        decision: ProviderActivationDecision,
    },
    ProposeToolCall {
        call_id: ToolCallId,
        route_id: RouteId,
        source_conversation_id: LocalConversationId,
        provider_id: ToolProviderId,
        operation: ToolOperationName,
        arguments_text: String,
    },
    DecideToolCallRoute {
        route_id: RouteId,
        decision: RouteUserDecision,
    },
    DispatchBuiltinHello {
        route_id: RouteId,
    },
    CheckSandboxHostReadiness,
    ExecuteApprovedStdioTool {
        call_id: ToolCallId,
    },
    ReserveApprovedStdioToolAfterProbe {
        call_id: ToolCallId,
    },
    RecordExternalStdioOutcome {
        observation: local_stdio_runner::ConfinedStdioObservation,
    },
    DecideToolResultContext {
        call_id: ToolCallId,
        conversation_id: LocalConversationId,
        decision: ToolResultContextDecision,
    },
    RecordLocalMemory {
        memory_id: LocalMemoryId,
        source_conversation_id: LocalConversationId,
        text: String,
    },
    DecideLocalMemoryContext {
        memory_id: LocalMemoryId,
        destination_conversation_id: LocalConversationId,
        decision: LocalMemoryContextDecision,
    },
    SupersedeLocalMemory {
        predecessor_memory_id: LocalMemoryId,
        successor_memory_id: LocalMemoryId,
    },
    AddLocalMemoryLabel {
        memory_id: LocalMemoryId,
        label: LocalMemoryLabel,
    },
    RemoveLocalMemoryLabel {
        memory_id: LocalMemoryId,
        label: LocalMemoryLabel,
    },
    BindLocalConversationWorker {
        conversation_id: LocalConversationId,
        worker_id: WorkerId,
    },
    BindCurrentSessionWorker {
        conversation_id: LocalConversationId,
        worker_id: WorkerId,
    },
    DesignateCurrentSessionController {
        conversation_id: LocalConversationId,
    },
    BindControllerToLocalWorker {
        controller_conversation_id: LocalConversationId,
        worker_conversation_id: LocalConversationId,
    },
    CreateControllerContinuationLease {
        controller_conversation_id: LocalConversationId,
        worker_conversation_id: LocalConversationId,
        allowance: u32,
    },
    ProposeControllerWorkerControl {
        controller_conversation_id: LocalConversationId,
        worker_conversation_id: LocalConversationId,
        action: ControllerControlAction,
    },
    DecideControllerWorkerControlRoute {
        route_id: RouteId,
        decision: RouteUserDecision,
    },
    DispatchControllerWorkerControlRoute {
        route_id: RouteId,
    },
    DecideControllerWorkerResultContext {
        route_id: RouteId,
        controller_conversation_id: LocalConversationId,
        decision: ControllerWorkerResultContextDecision,
    },
    StartControllerCoordination {
        controller_conversation_id: LocalConversationId,
    },
    DecideControllerCoordinationResultContext {
        coordination_turn_id: LocalTurnId,
        controller_conversation_id: LocalConversationId,
        decision: ControllerCoordinationResultContextDecision,
    },
    RecordControllerCoordinationSuggestion {
        controller_conversation_id: LocalConversationId,
        coordination_turn_id: LocalTurnId,
        basis_result_route_id: RouteId,
        action: CoordinationSuggestionAction,
    },
    PromoteControllerCoordinationSuggestion {
        controller_conversation_id: LocalConversationId,
        suggestion_id: CoordinationSuggestionId,
    },
    AcknowledgeWorkerControl {
        worker_conversation_id: LocalConversationId,
        route_id: RouteId,
    },
    RecordWorkerControlStatusResult {
        worker_conversation_id: LocalConversationId,
        route_id: RouteId,
    },
    ApplyWorkerControl {
        worker_conversation_id: LocalConversationId,
        route_id: RouteId,
    },
    StartWorkerContinuationExecution {
        worker_conversation_id: LocalConversationId,
        route_id: RouteId,
    },
    AssignWorkerGoal {
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
    },
    TransitionWorker {
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        action: WorkerAction,
    },
    SaveLocalConversationCatalog {
        catalog: local_conversations::LocalConversationCatalog,
    },
    SaveDraft {
        conversation_id: LocalConversationId,
        revision: u64,
        text: String,
    },
    CommitMessage {
        request_id: u64,
        message: AuthoredUserMessage,
        attachment: Option<Value>,
        one_shot_memory_ids: Vec<LocalMemoryId>,
        one_shot_memory_snapshot_after_sequence: Option<u64>,
    },
    AppendTurnEvent {
        turn_id: LocalTurnId,
        kind: EventKind,
        payload: String,
    },
    AppendAgentActivity {
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
        frame: String,
    },
    LoadHistoricalConversation {
        local_conversation_id: LocalConversationId,
    },
    LoadLiveConversation {
        local_conversation_id: LocalConversationId,
    },
    PromoteHistoricalLiveMirror {
        local_conversation_id: LocalConversationId,
        expected_remote_conversation_id: String,
        body: Value,
    },
    PromoteDiscoveredLiveMirror {
        expected_remote_conversation_id: String,
        body: Value,
    },
    MirrorQueuePrepare {
        remote_conversation_id: String,
        catalog_index: usize,
        reply: Sender<Result<(), String>>,
    },
    MirrorQueueCapture {
        remote_conversation_id: String,
        body: Value,
        reply: Sender<Result<MirrorPersisted, String>>,
    },
    MirrorQueueFailure {
        remote_conversation_id: String,
        failure_class: MirrorFailureClass,
        reply: Sender<Result<(), String>>,
    },
    RecordRemoteHealthSignal {
        signal: RemoteHealthSignal,
        now_ms: u64,
        reply: Sender<Result<RemoteHealthController, String>>,
    },
    RecordRemoteHealthIntent {
        intent: MirrorIntent,
        now_ms: u64,
        reply: Sender<Result<RemoteHealthController, String>>,
    },
    Shutdown,
}

enum PersistNotice {
    ProjectOpenVikingRetrievalFinished {
        conversation_id: LocalConversationId,
        generation: u64,
        result: Result<Vec<openviking_memory_delivery::RetrievedCheckpoint>, String>,
    },
    OpenVikingRetrievalFinished {
        conversation_id: LocalConversationId,
        result: Result<Vec<openviking_memory_delivery::RetrievedCheckpoint>, String>,
    },
    OpenVikingMemorySummarySaved {
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
        skipped: bool,
    },
    OpenVikingMemoryWriteFinished {
        conversation_id: String,
        confirmed: bool,
    },
    DraftSaved {
        conversation_id: LocalConversationId,
        revision: u64,
        event: EventEnvelope,
    },
    MessageCommitted {
        request_id: u64,
        message: AuthoredUserMessage,
        event: EventEnvelope,
        one_shot_memory_event: Option<EventEnvelope>,
    },
    TurnEventAppended {
        turn_id: LocalTurnId,
        kind: EventKind,
        event: EventEnvelope,
    },
    AgentActivityAppended {
        event: EventEnvelope,
    },
    MemoryCheckpointQueued {
        conversation_id: LocalConversationId,
        pending_count: usize,
    },
    HistoricalConversationLoaded {
        local_conversation_id: LocalConversationId,
        imported_sequence: u64,
        messages: Vec<HistoricalTranscriptMessage>,
    },
    HistoricalConversationLoadFailed {
        local_conversation_id: LocalConversationId,
        error: String,
    },
    LiveConversationLoaded {
        local_conversation_id: LocalConversationId,
        snapshot_sequence: u64,
        truncated_before: bool,
        messages: Vec<RemoteTranscriptMessage>,
    },
    LiveConversationLoadFailed {
        local_conversation_id: LocalConversationId,
        error: String,
    },
    HistoricalLiveMirrorPromoted {
        local_conversation_id: LocalConversationId,
        snapshot_sequence: u64,
        appended_events: Vec<EventEnvelope>,
        truncated_before: bool,
        messages: Vec<RemoteTranscriptMessage>,
        projection_error: Option<String>,
    },
    HistoricalLiveMirrorPromotionFailed {
        local_conversation_id: LocalConversationId,
        error: String,
    },
    DiscoveredLiveMirrorPromoted {
        local_conversation_id: LocalConversationId,
        remote_conversation_id: String,
        snapshot_sequence: u64,
        appended_events: Vec<EventEnvelope>,
        truncated_before: bool,
        messages: Vec<RemoteTranscriptMessage>,
        projection_error: Option<String>,
    },
    DiscoveredLiveMirrorPromotionFailed {
        remote_conversation_id: String,
        error: String,
    },
    MirrorQueueUpdated {
        appended_events: Vec<EventEnvelope>,
    },
    RemoteHealthUpdated {
        event: EventEnvelope,
        controller: RemoteHealthController,
    },
    OrchestrationTopologyInitialized {
        appended_events: Vec<EventEnvelope>,
    },
    RouteEndpointBound {
        event: EventEnvelope,
    },
    LocalRoutePolicyEventAppended {
        event: EventEnvelope,
    },
    LocalRoutePayloadAttached {
        route_id: RouteId,
        event: EventEnvelope,
    },
    LocalRouteDispatchUpdated {
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    LocalRouteContextDecisionUpdated {
        route_id: RouteId,
        event: EventEnvelope,
    },
    ToolProviderUpdated {
        provider_id: ToolProviderId,
        event: EventEnvelope,
    },
    AgentMcpCallProposed {
        turn_id: LocalTurnId,
        call_id: ToolCallId,
        route_id: RouteId,
        policy: mcp_agent_defaults::ToolPolicy,
        appended_events: Vec<EventEnvelope>,
    },
    ToolCallProposed {
        call_id: ToolCallId,
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    ToolCallRoutePolicyUpdated {
        route_id: RouteId,
        event: EventEnvelope,
    },
    BuiltinHelloDispatched {
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    HostShellDispatched {
        route_id: RouteId,
        event: EventEnvelope,
    },
    SandboxHostChecked {
        result: Result<(), String>,
    },
    StdioToolDispatchReserved {
        call_id: ToolCallId,
        event: EventEnvelope,
    },
    StdioToolDispatchFinished {
        call_id: ToolCallId,
        event: EventEnvelope,
    },
    ToolResultContextDecisionUpdated {
        call_id: ToolCallId,
        event: EventEnvelope,
    },
    LocalMemoryArtifactRecorded {
        memory_id: LocalMemoryId,
        event: EventEnvelope,
    },
    LocalMemoryContextDecisionUpdated {
        memory_id: LocalMemoryId,
        event: EventEnvelope,
    },
    LocalMemorySupersessionRecorded {
        predecessor_memory_id: LocalMemoryId,
        successor_memory_id: LocalMemoryId,
        event: EventEnvelope,
    },
    LocalMemoryLabelUpdated {
        memory_id: LocalMemoryId,
        event: EventEnvelope,
    },
    LifecycleEventAppended {
        event: EventEnvelope,
    },
    SupervisionEventAppended {
        event: EventEnvelope,
    },
    ControllerContinuationLeaseCreated {
        worker_id: WorkerId,
        event: EventEnvelope,
    },
    ControllerControlProposed {
        control_id: ControlId,
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    ControllerControlRoutePolicyUpdated {
        route_id: RouteId,
        event: EventEnvelope,
    },
    ControllerControlDispatched {
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    ControllerWorkerResultContextDecisionUpdated {
        route_id: RouteId,
        event: EventEnvelope,
    },
    ControllerCoordinationStarted {
        turn_id: LocalTurnId,
        event: EventEnvelope,
    },
    ControllerCoordinationResultRecorded {
        turn_id: LocalTurnId,
        event: EventEnvelope,
    },
    ControllerCoordinationResultContextDecisionUpdated {
        turn_id: LocalTurnId,
        event: EventEnvelope,
    },
    ControllerCoordinationSuggestionRecorded {
        suggestion_id: CoordinationSuggestionId,
        event: EventEnvelope,
    },
    ControllerCoordinationSuggestionPromoted {
        suggestion_id: CoordinationSuggestionId,
        control_id: ControlId,
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    WorkerControlAcknowledged {
        route_id: RouteId,
        event: EventEnvelope,
    },
    WorkerControlStatusResultRecorded {
        route_id: RouteId,
        event: EventEnvelope,
    },
    WorkerControlActionAdvanced {
        route_id: RouteId,
        appended_events: Vec<EventEnvelope>,
    },
    WorkerContinuationExecutionStarted {
        route_id: RouteId,
        event: EventEnvelope,
    },
    WorkerContinuationExecutionResultRecorded {
        route_id: RouteId,
        event: EventEnvelope,
    },
    Failed {
        operation: &'static str,
        revision: Option<u64>,
        request_id: Option<u64>,
        turn_id: Option<LocalTurnId>,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorFailureClass {
    Transient,
    RateLimited,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MirrorPersisted {
    snapshot_sequence: u64,
    mirror_state: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorControllerState {
    Stopped,
    Running,
    Paused,
    RateLimited,
    AuthenticationRequired,
    Completed,
    Failed,
}

enum MirrorControllerCommand {
    Start,
    Pause,
    Resume,
    RecheckHealth,
    Shutdown,
}

enum MirrorControllerNotice {
    State {
        state: MirrorControllerState,
        current_catalog_index: Option<usize>,
        detail: Option<String>,
    },
    Prepare {
        remote_conversation_id: String,
        catalog_index: usize,
        reply: Sender<Result<(), String>>,
    },
    Capture {
        remote_conversation_id: String,
        body: Value,
        reply: Sender<Result<MirrorPersisted, String>>,
    },
    Failure {
        remote_conversation_id: String,
        failure_class: MirrorFailureClass,
        reply: Sender<Result<(), String>>,
    },
    HealthSignal {
        signal: RemoteHealthSignal,
        now_ms: u64,
        reply: Sender<Result<RemoteHealthController, String>>,
    },
    HealthIntent {
        intent: MirrorIntent,
        now_ms: u64,
        reply: Sender<Result<RemoteHealthController, String>>,
    },
}

enum LiveMirrorFetchNotice {
    HistoryAuthenticationObserved {
        observation: account_bridge::AuthenticationObservation,
    },
    HistoryProbeFailed {
        error: account_bridge::BrowserBridgeError,
    },
    HistoryListLoaded {
        observation: account_bridge::ConversationListObservation,
    },
    HistoryListFailed {
        error: String,
    },
    HistoryDiscoveryLoaded {
        observation: account_bridge::HistoryDiscoveryObservation,
    },
    HistoryFreshTabDiscoveryLoaded {
        primary: account_bridge::HistoryDiscoveryObservation,
        fallback: account_bridge::FreshTabHistoryDiscoveryObservation,
    },
    HistoryFreshTabDiscoveryFailed {
        primary: account_bridge::HistoryDiscoveryObservation,
        error: String,
    },
    HistoryDiscoveryFailed {
        error: String,
    },
    Fetched {
        local_conversation_id: LocalConversationId,
        remote_conversation_id: String,
        body: Value,
        proof: account_bridge::BrowserProof,
        http_status: u16,
    },
    Failed {
        local_conversation_id: LocalConversationId,
        error: String,
    },
    DiscoveredFetched {
        remote_conversation_id: String,
        body: Value,
        proof: account_bridge::BrowserProof,
        http_status: u16,
    },
    DiscoveredFailed {
        remote_conversation_id: String,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayRole {
    User,
    Assistant,
    /// Audited local tool activity; never user-authored or assistant-authored.
    Mcp,
    /// Source-observed inference lifecycle, not assistant-authored content.
    Activity,
}

#[derive(Debug, Clone)]
struct DisplayMessage {
    role: DisplayRole,
    text: String,
    sequence: u64,
    timestamp: Option<f64>,
    provenance_label: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingOneShotMemory {
    memory_id: LocalMemoryId,
    source_conversation_id: LocalConversationId,
    text: String,
    artifact_sequence: u64,
}

#[derive(Debug, Clone)]
struct PendingInferenceIntent {
    model: String,
    instructions: Option<String>,
    developer_context: String,
    routed_context: Vec<context_composer::TranscriptMessage>,
    tool_result_context: Vec<context_composer::TranscriptMessage>,
    local_memory_context: Vec<context_composer::TranscriptMessage>,
    controller_result_context: Vec<context_composer::TranscriptMessage>,
    controller_coordination_result_context: Vec<context_composer::TranscriptMessage>,
    request_patch: Value,
    attachment: Option<attachment_intake::PreparedAttachment>,
    coordination_ids: Vec<u64>,
}

#[derive(Debug, Clone)]
struct PendingRemoteTurn {
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    request_id: String,
    model: String,
    input: Value,
    instructions: Option<String>,
    request_patch: Value,
}

/// One user-authored turn may contain several remote response steps and MCP
/// calls. All remote steps remain correlated to the ORIGINAL local turn ID.
/// Tool dispatches are independently journaled before side effects.
#[derive(Debug, Clone)]
struct AgentMcpFlow {
    turn_id: LocalTurnId,
    conversation_id: LocalConversationId,
    command: siwc_bridge::BridgeCommand,
    proposal_text: String,
    call_count: u32,
    pending_call: Option<ToolCallId>,
    pending_route: Option<RouteId>,
    policy: mcp_agent_defaults::ToolPolicy,
}

/// Durable, conversation-scoped status for the latest MCP request.
/// Presentation only: buttons still use the original audited route checks.
#[derive(Clone)]
struct InlineMcpStatus {
    call_id: ToolCallId,
    route_id: RouteId,
    provider: String,
    operation: String,
    arguments: String,
    stage: mcp_call_review::CallReviewStage,
    automatically_authorized: bool,
    successful: Option<bool>,
}

fn project_inline_mcp_status(
    events: &[EventEnvelope],
    conversation: LocalConversationId,
) -> Option<InlineMcpStatus> {
    let session = replay_local_routing_directory(events)
        .ok()?
        .into_iter()
        .find(|entry| entry.conversation_id == conversation)?
        .current_session_id;
    let call = replay_tool_call_audit(events)
        .ok()?
        .into_iter()
        .filter(|call| call.source_session_id == session)
        .max_by_key(|call| call.recorded_sequence)?;
    let route_id = call.route_id?;
    let route = replay_routing_audit(events)
        .ok()?
        .into_iter()
        .find(|route| route.request.id == route_id)?;
    let provider = replay_tool_provider_audit(events)
        .ok()?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)?;
    let outcome = replay_tool_call_outcome_audit(events)
        .ok()?
        .into_iter()
        .find(|outcome| outcome.call_id == call.call_id);
    let successful = outcome
        .as_ref()
        .map(|outcome| matches!(outcome.kind, ToolCallOutcomeKind::Result));
    let stage = mcp_call_review::classify(
        call.route_bound_sequence.is_some(),
        route.latest_user_decision,
        route.dispatch_sequence.is_some(),
        successful.is_some(),
    );
    Some(InlineMcpStatus {
        call_id: call.call_id,
        route_id,
        provider: provider.name.as_str().to_owned(),
        operation: call.operation.as_str().to_owned(),
        arguments: call.arguments_text.clone(),
        stage,
        automatically_authorized: route.initial_policy == RoutePolicy::Allow,
        successful,
    })
}

#[derive(Debug, Clone)]
struct ActiveRemoteTurn {
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    request_id: String,
    cumulative_text: String,
    observed_output: bool,
    source_suspect_logged: bool,
    command: siwc_bridge::BridgeCommand,
    retries: u32,
    retry_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct LiveMirrorCatalogEntry {
    local_conversation_id: LocalConversationId,
    remote_conversation_id: String,
    title: String,
    snapshot_sequence: u64,
    truncated_before: bool,
}

#[derive(Debug, Clone)]
struct RemoteCatalogViewEntry {
    catalog_index: usize,
    item: ConversationListItem,
    status: RemoteMirrorQueueStatus,
    local_conversation_id: Option<LocalConversationId>,
}

#[derive(Debug, Clone)]
struct PendingHistoryFetchProof {
    local_conversation_id: Option<LocalConversationId>,
    remote_conversation_id: String,
    proof: account_bridge::BrowserProof,
    http_status: u16,
}

#[derive(Debug, Clone)]
struct OpenVikingSummaryAttempt {
    request_id: String,
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    completed_sequence: u64,
    had_confirmed_prior: bool,
}

struct ChatariumApp {
    draft: String,
    attachment_path_draft: String,
    staged_attachment: Option<attachment_intake::PreparedAttachment>,
    draft_revision: u64,
    saved_revision: u64,
    next_commit_request: u64,
    commit_in_flight: Option<u64>,
    evidence: TurnEvidence,
    local_conversation_id: LocalConversationId,
    local_conversation_catalog: local_conversations::LocalConversationCatalog,
    project_memory_grants: openviking_memory_delivery::ProjectMemoryGrants,
    coordination_workspace: coordination_workspace::Workspace,
    coordination_pending_admissions: BTreeMap<String, Vec<u64>>,
    coordination_ui: coordination_workspace::UiDrafts,
    coordination_window_open: bool,
    coordination_tool_budget: coordination_tool_budget::Budget,
    coordination_last_queue_check: Instant,
    coordination_auto_wake_targets: HashSet<LocalConversationId>,
    coordination_auto_wake_count: u32,
    coordination_pending_wake: Option<(LocalTurnId, LocalConversationId, Vec<u64>)>,
    local_conversation_rename: String,
    local_conversation_project_directory: String,
    show_archived_local_conversations: bool,
    local_conversation_search_query: String,
    local_conversation_search_selection: Option<LocalConversationId>,
    events: Vec<EventEnvelope>,
    historical_catalog: Vec<HistoricalConversationCatalogEntry>,
    selected_historical_conversation: Option<LocalConversationId>,
    loaded_historical_conversation: Option<LocalConversationId>,
    historical_messages: Vec<DisplayMessage>,
    historical_load_pending: Option<LocalConversationId>,
    live_mirrored_conversations: HashSet<LocalConversationId>,
    live_mirror_catalog: Vec<LiveMirrorCatalogEntry>,
    live_mirror_pending: Option<LocalConversationId>,
    remote_discovery_pending: Option<String>,
    remote_mirror_failures: BTreeMap<String, String>,
    remote_mirror_retry_after: BTreeMap<String, Instant>,
    mirror_status: String,
    live_mirror_truncated_before: bool,
    remote_conversation_catalog: Vec<ConversationListItem>,
    remote_catalog_view: Vec<RemoteCatalogViewEntry>,
    local_archive_search_index: local_archive_search::LocalArchiveSearchIndex,
    archive_search_query: String,
    archive_search_mode: local_archive_search::ArchiveSearchMode,
    archive_state_filter: local_archive_search::ArchiveStateFilter,
    archive_search_selection: Option<usize>,
    reader_state_path: PathBuf,
    reader_positions: offline_reader::ReaderPositionStore,
    reader_restore_pending: bool,
    reader_last_saved_offset: f32,
    reader_last_position_write: Instant,
    reader_search_query: String,
    reader_search_hit: Option<usize>,
    inference_settings: local_inference_settings::InferenceSettingsStore,
    mcp_agent_defaults: mcp_agent_defaults::AgentDefaults,
    host_shell_settings: host_shell_mcp::Settings,
    behavior_profiles: behavior_profile::BehaviorProfileStore,
    conversation_behavior_profile: behavior_profile::BehaviorProfile,
    topology_command_pending: bool,
    route_addressability_command_pending: bool,
    route_policy_command_pending: bool,
    route_payload_command_pending: bool,
    route_payload_drafts: BTreeMap<RouteId, String>,
    route_dispatch_command_pending: bool,
    route_context_command_pending: bool,
    tool_command_pending: bool,
    // Read-only, point-in-time host observation; never an execution permit.
    last_sandbox_host_check: Option<Result<(), String>>,
    tool_provider_name_draft: String,
    tool_selected_provider: Option<ToolProviderId>,
    tool_stdio_executable_draft: String,
    tool_stdio_argv_draft: String,
    tool_stdio_operations_draft: String,
    tool_operation_draft: String,
    tool_arguments_draft: String,
    local_memory_command_pending: bool,
    local_memory_draft: String,
    // Transient source of an unedited message staged from native search.
    // Editing the draft clears this origin; recording remains an explicit act.
    staged_local_memory_origin: Option<(LocalConversationId, u64)>,
    local_memory_search_query: String,
    local_memory_label_filter: Option<LocalMemoryLabel>,
    local_memory_source_filter: Option<LocalConversationId>,
    show_superseded_local_memory: bool,
    local_memory_label_drafts: BTreeMap<LocalMemoryId, String>,
    local_memory_one_shot_selections: BTreeMap<LocalConversationId, BTreeSet<LocalMemoryId>>,
    lifecycle_command_pending: bool,
    supervision_command_pending: bool,
    controller_control_command_pending: bool,
    controller_result_context_command_pending: bool,
    controller_coordination_command_pending: bool,
    controller_coordination_result_context_command_pending: bool,
    controller_coordination_suggestion_command_pending: bool,
    continuation_allowance_drafts: BTreeMap<LocalConversationId, u32>,
    worker_control_command_pending: bool,
    conversation_instructions: String,
    conversation_developer_context: String,
    conversation_mcp_enabled: bool,
    archive_backup_path: String,
    archive_maintenance_status: String,
    archive_restore_confirmation_pending: bool,
    selected_remote_catalog_id: Option<String>,
    remote_conversation_total: Option<u64>,
    history_discovery_started: bool,
    history_list_pending: bool,
    history_bridge_proven: bool,
    pending_history_fetch_proof: Option<PendingHistoryFetchProof>,
    journal_path: PathBuf,
    persist_tx: Option<Sender<PersistCommand>>,
    notice_rx: Option<Receiver<PersistNotice>>,
    worker: Option<JoinHandle<()>>,
    status: String,
    account_bridge: Option<account_bridge::AccountBridgeRuntime>,
    account_bridge_provider: Option<account_bridge::BrowserBridgeProvider>,
    account_bridge_status: String,
    live_mirror_fetch_tx: Sender<LiveMirrorFetchNotice>,
    live_mirror_fetch_rx: Receiver<LiveMirrorFetchNotice>,
    mirror_controller_tx: Option<Sender<MirrorControllerCommand>>,
    mirror_controller_notice_rx: Option<Receiver<MirrorControllerNotice>>,
    mirror_controller_worker: Option<JoinHandle<()>>,
    mirror_controller_state: MirrorControllerState,
    remote_health: RemoteHealthController,
    mirror_controller_current_item: Option<usize>,
    mirror_controller_detail: Option<String>,
    remote: siwc_bridge::BridgeRuntime,
    remote_session: siwc_bridge::SessionState,
    remote_models: Vec<siwc_bridge::Model>,
    capability_probe: capability_probes::ProbeRun,
    local_inference_contract: Option<local_inference_contract::LoadedContract>,
    // One-click verification intent; never enable a different chat/model/profile.
    pending_web_search_enable: Option<(LocalConversationId, String, String)>,
    model_list_pending: bool,
    selected_model: Option<String>,
    slow_warning_seconds: u32,
    remote_status: String,
    network_health: network_health::NetworkHealth,
    remote_runtime_ready: bool,
    remote_runtime_failed: bool,
    sign_in_requested: bool,
    sign_in_pending: bool,
    pending_remote_turn: Option<PendingRemoteTurn>,
    active_remote_turn: Option<ActiveRemoteTurn>,
    agent_mcp_flow: Option<AgentMcpFlow>,
    openviking_checkpoint_status: String,
    openviking_summary_in_flight: Option<OpenVikingSummaryAttempt>,
    openviking_summary_last_attempt: Option<Instant>,
    openviking_write_last_attempt: Option<Instant>,
    openviking_summary_blocked: HashSet<String>,
    openviking_retrieval_cache:
        BTreeMap<LocalConversationId, Vec<openviking_memory_delivery::RetrievedCheckpoint>>,
    openviking_retrieval_in_flight: HashSet<LocalConversationId>,
    openviking_retrieval_retry_after: BTreeMap<LocalConversationId, Instant>,
    openviking_retrieval_stale: HashSet<LocalConversationId>,
    openviking_query_tracker: openviking_project_cache::QueryRefreshTracker,
    openviking_project_cache: openviking_project_cache::RetrievalCache,
    commit_remote_intents: BTreeMap<u64, PendingInferenceIntent>,
    delivery: delivery_telemetry::Timeline,
}

impl ChatariumApp {
    fn new(repaint: &egui::Context) -> Self {
        let journal_path = default_journal_path();
        diagnostics::info(
            "startup",
            format!("opening durable journal at {}", journal_path.display()),
        );
        let data_dir = journal_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        match JsonlEventStore::open(&journal_path) {
            Ok(mut store) => {
                let recovery = recover_interrupted_remote_turns(&mut store);
                let events = store.events().to_vec();
                diagnostics::info(
                    "startup",
                    format!("journal replay complete: {} events", events.len()),
                );
                let local_conversation_catalog_path =
                    local_conversation_catalog_path(&journal_path);
                let (local_conversation_catalog, local_conversation_id, mut startup_status) =
                    match load_local_conversation_workspace(
                        &local_conversation_catalog_path,
                        &events,
                    ) {
                        Ok((catalog, id)) => (catalog, id, "journal ready".to_owned()),
                        Err(error) => {
                            let fallback = projected_local_conversation_id(&events)
                                .ok()
                                .flatten()
                                .unwrap_or_default();
                            let mut catalog =
                                local_conversations::LocalConversationCatalog::default();
                            catalog.create(fallback, unix_now_ms());
                            (
                                catalog,
                                fallback,
                                format!(
                                    "journal ready; local conversation catalog warning: {error}"
                                ),
                            )
                        }
                    };
                let draft = projected_working_draft(&events, local_conversation_id);
                match recovery {
                    Ok(0) => {}
                    Ok(count) => {
                        startup_status = format!(
                            "{startup_status}; recovered {count} interrupted remote turn{}",
                            if count == 1 { "" } else { "s" }
                        );
                    }
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; remote-turn recovery warning: {error}");
                    }
                }
                let historical_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match latest_historical_conversation_catalog(&events) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status = format!(
                                "{startup_status}; historical archive replay warning: {error}"
                            );
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                let live_mirror_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match latest_live_mirror_catalog(&events) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; live mirror replay warning: {error}");
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                // Preserve cached account history on disk but never read or
                // index it in the native-only workspace.
                let remote_conversation_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match load_remote_history_cache(&remote_history_cache_path(&journal_path)) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; remote history cache warning: {error}");
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                let remote_catalog_view = match build_remote_catalog_view(
                    &remote_conversation_catalog,
                    &events,
                    &live_mirror_catalog,
                ) {
                    Ok(view) => view,
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; local mirror view warning: {error}");
                        Vec::new()
                    }
                };
                diagnostics::info(
                    "startup",
                    format!(
                        "catalogs ready: historical={} live-mirrors={} discovered-history-cache={}",
                        historical_catalog.len(),
                        live_mirror_catalog.len(),
                        remote_conversation_catalog.len()
                    ),
                );
                let live_mirrored_conversations = live_mirror_catalog
                    .iter()
                    .map(|entry| entry.local_conversation_id)
                    .collect();
                let (account_bridge, account_bridge_provider, account_bridge_status) =
                    start_account_bridge();
                let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
                let (persist_tx, persist_rx) = mpsc::channel();
                let (notice_tx, notice_rx) = mpsc::channel();
                let local_archive_search_index = if ACCOUNT_MIRRORING_ENABLED {
                    build_local_archive_search_index(&remote_catalog_view, &events)
                } else {
                    local_archive_search::LocalArchiveSearchIndex::new(Vec::new())
                };
                let reader_state_path = local_reader_state_path(&journal_path);
                let reader_positions =
                    offline_reader::ReaderPositionStore::load(&reader_state_path);
                let inference_settings_path = local_inference_settings_path(&journal_path);
                let inference_settings =
                    match local_inference_settings::InferenceSettingsStore::load(
                        &inference_settings_path,
                    ) {
                        Ok(settings) => settings,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; inference settings warning: {error}");
                            local_inference_settings::InferenceSettingsStore::default()
                        }
                    };
                let mcp_agent_defaults = mcp_agent_defaults::AgentDefaults::load(
                    &data_dir.join(mcp_agent_defaults::FILE_NAME),
                )
                .unwrap_or_default();
                let host_shell_settings =
                    host_shell_mcp::Settings::load(&data_dir.join(host_shell_mcp::FILE_NAME))
                        .unwrap_or_default();
                let active_inference_settings = inference_settings
                    .for_conversation_with_mcp_defaults(local_conversation_id, &mcp_agent_defaults);
                let coordination_workspace = coordination_workspace::Workspace::load(
                    &data_dir.join(coordination_workspace::FILE_NAME),
                )
                .unwrap_or_default();
                let project_memory_grants =
                    match openviking_memory_delivery::ProjectMemoryGrants::load(
                        &data_dir.join(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME),
                    ) {
                        Ok(grants) => grants,
                        Err(error) => {
                            startup_status = format!(
                                "{startup_status}; project memory permission warning: {error}"
                            );
                            openviking_memory_delivery::ProjectMemoryGrants::default()
                        }
                    };
                let behavior_profile_path = local_behavior_profile_path(&journal_path);
                let behavior_profiles =
                    match behavior_profile::BehaviorProfileStore::load(&behavior_profile_path) {
                        Ok(profiles) => profiles,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; behavior profile warning: {error}");
                            behavior_profile::BehaviorProfileStore::default()
                        }
                    };
                let active_behavior_profile =
                    behavior_profiles.for_conversation(local_conversation_id);
                let worker_data_dir = data_dir.clone();
                let completion_tx = persist_tx.clone();
                let worker = thread::Builder::new()
                    .name("chatarium-persistence".to_owned())
                    .spawn(move || {
                        persistence_worker(
                            store,
                            worker_data_dir,
                            persist_rx,
                            completion_tx,
                            notice_tx,
                        )
                    });

                match worker {
                    Ok(worker) => Self {
                        draft,
                        attachment_path_draft: String::new(),
                        staged_attachment: None,
                        draft_revision: 0,
                        saved_revision: 0,
                        next_commit_request: 1,
                        commit_in_flight: None,
                        evidence: TurnEvidence::default(),
                        local_conversation_id,
                        local_conversation_rename: local_conversation_catalog
                            .entry(local_conversation_id)
                            .and_then(|entry| entry.title.clone())
                            .unwrap_or_default(),
                        local_conversation_project_directory: local_conversation_catalog
                            .entry(local_conversation_id)
                            .and_then(|entry| entry.project_directory.clone())
                            .unwrap_or_default(),
                        local_conversation_catalog,
                        project_memory_grants,
                        coordination_workspace,
                        coordination_pending_admissions: BTreeMap::new(),
                        coordination_ui: coordination_workspace::UiDrafts::default(),
                        coordination_window_open: false,
                        coordination_tool_budget: coordination_tool_budget::Budget::default(),
                        coordination_last_queue_check: Instant::now(),
                        coordination_auto_wake_targets: HashSet::new(),
                        coordination_auto_wake_count: 0,
                        coordination_pending_wake: None,
                        show_archived_local_conversations: false,
                        local_conversation_search_query: String::new(),
                        local_conversation_search_selection: None,
                        events: events.clone(),
                        historical_catalog,
                        selected_historical_conversation: None,
                        loaded_historical_conversation: None,
                        historical_messages: Vec::new(),
                        historical_load_pending: None,
                        live_mirrored_conversations,
                        live_mirror_catalog,
                        live_mirror_pending: None,
                        remote_discovery_pending: None,
                        remote_mirror_failures: BTreeMap::new(),
                        remote_mirror_retry_after: BTreeMap::new(),
                        mirror_status: "idle · no mirror in progress".to_owned(),
                        live_mirror_truncated_before: false,
                        remote_conversation_catalog,
                        remote_catalog_view,
                        local_archive_search_index,
                        archive_search_query: String::new(),
                        archive_search_mode: local_archive_search::ArchiveSearchMode::AllLocalData,
                        archive_state_filter: local_archive_search::ArchiveStateFilter::All,
                        archive_search_selection: None,
                        reader_state_path,
                        reader_positions,
                        reader_restore_pending: true,
                        reader_last_saved_offset: 0.0,
                        reader_last_position_write: Instant::now(),
                        reader_search_query: String::new(),
                        reader_search_hit: None,
                        inference_settings,
                        mcp_agent_defaults,
                        host_shell_settings,
                        behavior_profiles,
                        conversation_behavior_profile: active_behavior_profile,
                        topology_command_pending: false,
                        route_addressability_command_pending: false,
                        route_policy_command_pending: false,
                        route_payload_command_pending: false,
                        route_payload_drafts: BTreeMap::new(),
                        route_dispatch_command_pending: false,
                        route_context_command_pending: false,
                        tool_command_pending: false,
                        last_sandbox_host_check: None,
                        tool_provider_name_draft: String::new(),
                        tool_selected_provider: None,
                        tool_stdio_executable_draft: String::new(),
                        tool_stdio_argv_draft: String::new(),
                        tool_stdio_operations_draft: String::new(),
                        tool_operation_draft: String::new(),
                        tool_arguments_draft: String::new(),
                        local_memory_command_pending: false,
                        local_memory_draft: String::new(),
                        staged_local_memory_origin: None,
                        local_memory_search_query: String::new(),
                        local_memory_label_filter: None,
                        local_memory_source_filter: None,
                        show_superseded_local_memory: false,
                        local_memory_label_drafts: BTreeMap::new(),
                        local_memory_one_shot_selections: BTreeMap::new(),
                        lifecycle_command_pending: false,
                        supervision_command_pending: false,
                        controller_control_command_pending: false,
                        controller_result_context_command_pending: false,
                        controller_coordination_command_pending: false,
                        controller_coordination_result_context_command_pending: false,
                        controller_coordination_suggestion_command_pending: false,
                        continuation_allowance_drafts: BTreeMap::new(),
                        worker_control_command_pending: false,
                        conversation_instructions: active_inference_settings.instructions,
                        conversation_developer_context: active_inference_settings.developer_context,
                        conversation_mcp_enabled: active_inference_settings.mcp_enabled,
                        archive_backup_path: String::new(),
                        archive_maintenance_status: "archive maintenance idle".to_owned(),
                        archive_restore_confirmation_pending: false,
                        selected_remote_catalog_id: None,
                        remote_conversation_total: None,
                        history_discovery_started: false,
                        history_list_pending: false,
                        history_bridge_proven: false,
                        pending_history_fetch_proof: None,
                        journal_path: journal_path.clone(),
                        persist_tx: Some(persist_tx),
                        notice_rx: Some(notice_rx),
                        worker: Some(worker),
                        status: startup_status,
                        account_bridge,
                        account_bridge_provider,
                        account_bridge_status,
                        live_mirror_fetch_tx,
                        live_mirror_fetch_rx,
                        mirror_controller_tx: None,
                        mirror_controller_notice_rx: None,
                        mirror_controller_worker: None,
                        mirror_controller_state: MirrorControllerState::Stopped,
                        remote_health: RemoteHealthController::from_events(&events, unix_now_ms())
                            .unwrap_or_default(),
                        mirror_controller_current_item: None,
                        mirror_controller_detail: None,
                        remote: siwc_bridge::BridgeRuntime::start(repaint),
                        remote_session: siwc_bridge::SessionState::default(),
                        remote_models: Vec::new(),
                        capability_probe: load_capability_probe_state(&journal_path),
                        local_inference_contract: load_local_inference_contract_state(
                            &journal_path,
                        ),
                        pending_web_search_enable: None,
                        model_list_pending: false,
                        slow_warning_seconds: active_inference_settings.slow_warning_seconds,
                        selected_model: active_inference_settings.model,
                        remote_status: "starting sign-in runtime…".to_owned(),
                        network_health: network_health::NetworkHealth::default(),
                        remote_runtime_ready: false,
                        remote_runtime_failed: false,
                        sign_in_requested: false,
                        sign_in_pending: false,
                        pending_remote_turn: None,
                        active_remote_turn: None,
                        agent_mcp_flow: None,
                        openviking_checkpoint_status:
                            "Awaiting an eligible completed assistant turn".to_owned(),
                        openviking_summary_in_flight: None,
                        openviking_summary_last_attempt: None,
                        openviking_write_last_attempt: None,
                        openviking_summary_blocked: HashSet::new(),
                        openviking_retrieval_cache: BTreeMap::new(),
                        openviking_retrieval_in_flight: HashSet::new(),
                        openviking_retrieval_retry_after: BTreeMap::new(),
                        openviking_retrieval_stale: HashSet::new(),
                        openviking_query_tracker:
                            openviking_project_cache::QueryRefreshTracker::default(),
                        openviking_project_cache: openviking_project_cache::RetrievalCache::default(
                        ),
                        commit_remote_intents: BTreeMap::new(),
                        delivery: delivery_telemetry::Timeline::default(),
                    },
                    Err(error) => Self::without_persistence(
                        repaint,
                        journal_path,
                        draft,
                        events,
                        format!("failed to start persistence worker: {error}"),
                    ),
                }
            }
            Err(error) => Self::without_persistence(
                repaint,
                journal_path,
                String::new(),
                Vec::new(),
                format!("failed to open journal: {error}"),
            ),
        }
    }

    fn without_persistence(
        repaint: &egui::Context,
        journal_path: PathBuf,
        draft: String,
        events: Vec<EventEnvelope>,
        status: String,
    ) -> Self {
        let live_mirror_catalog = if ACCOUNT_MIRRORING_ENABLED {
            latest_live_mirror_catalog(&events).unwrap_or_default()
        } else {
            Vec::new()
        };
        let live_mirrored_conversations = live_mirror_catalog
            .iter()
            .map(|entry| entry.local_conversation_id)
            .collect();
        let (account_bridge, account_bridge_provider, account_bridge_status) =
            start_account_bridge();
        let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
        let remote_conversation_catalog = if ACCOUNT_MIRRORING_ENABLED {
            load_remote_history_cache(&remote_history_cache_path(&journal_path)).unwrap_or_default()
        } else {
            Vec::new()
        };
        let remote_catalog_view =
            build_remote_catalog_view(&remote_conversation_catalog, &events, &live_mirror_catalog)
                .unwrap_or_default();
        let local_archive_search_index = if ACCOUNT_MIRRORING_ENABLED {
            build_local_archive_search_index(&remote_catalog_view, &events)
        } else {
            local_archive_search::LocalArchiveSearchIndex::new(Vec::new())
        };
        let reader_state_path = local_reader_state_path(&journal_path);
        let reader_positions = offline_reader::ReaderPositionStore::load(&reader_state_path);
        let local_conversation_catalog_path = local_conversation_catalog_path(&journal_path);
        let (local_conversation_catalog, local_conversation_id) =
            load_local_conversation_workspace(&local_conversation_catalog_path, &events)
                .unwrap_or_else(|_| {
                    let fallback = projected_local_conversation_id(&events)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let mut catalog = local_conversations::LocalConversationCatalog::default();
                    catalog.create(fallback, unix_now_ms());
                    (catalog, fallback)
                });
        let draft = projected_working_draft(&events, local_conversation_id);
        let inference_settings_path = local_inference_settings_path(&journal_path);
        let inference_settings =
            local_inference_settings::InferenceSettingsStore::load(&inference_settings_path)
                .unwrap_or_default();
        let mcp_agent_defaults = mcp_agent_defaults::AgentDefaults::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(mcp_agent_defaults::FILE_NAME),
        )
        .unwrap_or_default();
        let coordination_workspace = coordination_workspace::Workspace::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(coordination_workspace::FILE_NAME),
        )
        .unwrap_or_default();
        let project_memory_grants = openviking_memory_delivery::ProjectMemoryGrants::load(
            &journal_path
                .with_file_name(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME),
        )
        .unwrap_or_default();
        let host_shell_settings = host_shell_mcp::Settings::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(host_shell_mcp::FILE_NAME),
        )
        .unwrap_or_default();
        let active_inference_settings = inference_settings
            .for_conversation_with_mcp_defaults(local_conversation_id, &mcp_agent_defaults);
        let behavior_profiles = behavior_profile::BehaviorProfileStore::load(
            &local_behavior_profile_path(&journal_path),
        )
        .unwrap_or_default();
        let active_behavior_profile = behavior_profiles.for_conversation(local_conversation_id);

        Self {
            draft,
            attachment_path_draft: String::new(),
            staged_attachment: None,
            draft_revision: 0,
            saved_revision: 0,
            next_commit_request: 1,
            commit_in_flight: None,
            evidence: TurnEvidence::default(),
            local_conversation_id,
            local_conversation_rename: local_conversation_catalog
                .entry(local_conversation_id)
                .and_then(|entry| entry.title.clone())
                .unwrap_or_default(),
            local_conversation_project_directory: local_conversation_catalog
                .entry(local_conversation_id)
                .and_then(|entry| entry.project_directory.clone())
                .unwrap_or_default(),
            local_conversation_catalog,
            project_memory_grants,
            coordination_workspace,
            coordination_pending_admissions: BTreeMap::new(),
            coordination_ui: coordination_workspace::UiDrafts::default(),
            coordination_window_open: false,
            coordination_tool_budget: coordination_tool_budget::Budget::default(),
            coordination_last_queue_check: Instant::now(),
            coordination_auto_wake_targets: HashSet::new(),
            coordination_auto_wake_count: 0,
            coordination_pending_wake: None,
            show_archived_local_conversations: false,
            local_conversation_search_query: String::new(),
            local_conversation_search_selection: None,
            historical_catalog: if ACCOUNT_MIRRORING_ENABLED {
                latest_historical_conversation_catalog(&events).unwrap_or_default()
            } else {
                Vec::new()
            },
            events: events.clone(),
            selected_historical_conversation: None,
            loaded_historical_conversation: None,
            historical_messages: Vec::new(),
            historical_load_pending: None,
            live_mirrored_conversations,
            live_mirror_catalog,
            live_mirror_pending: None,
            remote_discovery_pending: None,
            remote_mirror_failures: BTreeMap::new(),
            remote_mirror_retry_after: BTreeMap::new(),
            mirror_status: "idle · no mirror in progress".to_owned(),
            live_mirror_truncated_before: false,
            remote_conversation_catalog,
            remote_catalog_view,
            local_archive_search_index,
            archive_search_query: String::new(),
            archive_search_mode: local_archive_search::ArchiveSearchMode::AllLocalData,
            archive_state_filter: local_archive_search::ArchiveStateFilter::All,
            archive_search_selection: None,
            reader_state_path,
            reader_positions,
            reader_restore_pending: true,
            reader_last_saved_offset: 0.0,
            reader_last_position_write: Instant::now(),
            reader_search_query: String::new(),
            reader_search_hit: None,
            inference_settings,
            mcp_agent_defaults,
            host_shell_settings,
            behavior_profiles,
            conversation_behavior_profile: active_behavior_profile,
            topology_command_pending: false,
            route_addressability_command_pending: false,
            route_policy_command_pending: false,
            route_payload_command_pending: false,
            route_payload_drafts: BTreeMap::new(),
            route_dispatch_command_pending: false,
            route_context_command_pending: false,
            tool_command_pending: false,
            last_sandbox_host_check: None,
            tool_provider_name_draft: String::new(),
            tool_selected_provider: None,
            tool_stdio_executable_draft: String::new(),
            tool_stdio_argv_draft: String::new(),
            tool_stdio_operations_draft: String::new(),
            tool_operation_draft: String::new(),
            tool_arguments_draft: String::new(),
            local_memory_command_pending: false,
            local_memory_draft: String::new(),
            staged_local_memory_origin: None,
            local_memory_search_query: String::new(),
            local_memory_label_filter: None,
            local_memory_source_filter: None,
            show_superseded_local_memory: false,
            local_memory_label_drafts: BTreeMap::new(),
            local_memory_one_shot_selections: BTreeMap::new(),
            lifecycle_command_pending: false,
            supervision_command_pending: false,
            controller_control_command_pending: false,
            controller_result_context_command_pending: false,
            controller_coordination_command_pending: false,
            controller_coordination_result_context_command_pending: false,
            controller_coordination_suggestion_command_pending: false,
            continuation_allowance_drafts: BTreeMap::new(),
            worker_control_command_pending: false,
            conversation_instructions: active_inference_settings.instructions,
            conversation_developer_context: active_inference_settings.developer_context,
            conversation_mcp_enabled: active_inference_settings.mcp_enabled,
            archive_backup_path: String::new(),
            archive_maintenance_status: "archive maintenance idle".to_owned(),
            archive_restore_confirmation_pending: false,
            selected_remote_catalog_id: None,
            remote_conversation_total: None,
            history_discovery_started: false,
            history_list_pending: false,
            history_bridge_proven: false,
            pending_history_fetch_proof: None,
            journal_path: journal_path.clone(),
            persist_tx: None,
            notice_rx: None,
            worker: None,
            status,
            account_bridge,
            account_bridge_provider,
            account_bridge_status,
            live_mirror_fetch_tx,
            live_mirror_fetch_rx,
            mirror_controller_tx: None,
            mirror_controller_notice_rx: None,
            mirror_controller_worker: None,
            mirror_controller_state: MirrorControllerState::Stopped,
            remote_health: RemoteHealthController::from_events(&events, unix_now_ms())
                .unwrap_or_default(),
            mirror_controller_current_item: None,
            mirror_controller_detail: None,
            remote: siwc_bridge::BridgeRuntime::start(repaint),
            remote_session: siwc_bridge::SessionState::default(),
            remote_models: Vec::new(),
            capability_probe: load_capability_probe_state(&journal_path),
            local_inference_contract: load_local_inference_contract_state(&journal_path),
            pending_web_search_enable: None,
            model_list_pending: false,
            slow_warning_seconds: active_inference_settings.slow_warning_seconds,
            selected_model: active_inference_settings.model,
            remote_status: "starting sign-in runtime…".to_owned(),
            network_health: network_health::NetworkHealth::default(),
            remote_runtime_ready: false,
            remote_runtime_failed: false,
            sign_in_requested: false,
            sign_in_pending: false,
            pending_remote_turn: None,
            active_remote_turn: None,
            agent_mcp_flow: None,
            openviking_checkpoint_status: "Awaiting an eligible completed assistant turn"
                .to_owned(),
            openviking_summary_in_flight: None,
            openviking_summary_last_attempt: None,
            openviking_write_last_attempt: None,
            openviking_summary_blocked: HashSet::new(),
            openviking_retrieval_cache: BTreeMap::new(),
            openviking_retrieval_in_flight: HashSet::new(),
            openviking_retrieval_retry_after: BTreeMap::new(),
            openviking_retrieval_stale: HashSet::new(),
            openviking_query_tracker: openviking_project_cache::QueryRefreshTracker::default(),
            openviking_project_cache: openviking_project_cache::RetrievalCache::default(),
            commit_remote_intents: BTreeMap::new(),
            delivery: delivery_telemetry::Timeline::default(),
        }
    }

    fn select_local_conversation(&mut self) {
        self.selected_historical_conversation = None;
        self.selected_remote_catalog_id = None;
        self.historical_load_pending = None;
        self.live_mirror_truncated_before = false;
        self.reader_restore_pending = true;
        self.reader_search_hit = None;
        self.status = "local conversation selected".to_owned();
    }

    fn local_conversation_busy(&self) -> bool {
        self.saved_revision < self.draft_revision
            || self.commit_in_flight.is_some()
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || self.agent_mcp_flow.is_some()
            || self.coordination_pending_wake.is_some()
    }

    fn apply_local_conversation(&mut self, conversation_id: LocalConversationId) {
        self.local_conversation_id = conversation_id;
        self.staged_attachment = None;
        self.attachment_path_draft.clear();
        self.draft = projected_working_draft(&self.events, conversation_id);
        self.draft_revision = 0;
        self.saved_revision = 0;
        self.evidence = TurnEvidence::default();
        self.coordination_tool_budget.reset();

        let settings = self
            .inference_settings
            .for_conversation_with_mcp_defaults(conversation_id, &self.mcp_agent_defaults);
        self.slow_warning_seconds = settings.slow_warning_seconds;
        self.selected_model = settings.model;
        self.conversation_instructions = settings.instructions;
        self.conversation_developer_context = settings.developer_context;
        self.conversation_mcp_enabled = settings.mcp_enabled;
        self.conversation_behavior_profile =
            self.behavior_profiles.for_conversation(conversation_id);
        if !self.remote_models.is_empty()
            && !self.selected_model.as_ref().is_some_and(|selected| {
                self.remote_models
                    .iter()
                    .any(|model| &model.slug == selected)
            })
        {
            self.selected_model = self.remote_models.first().map(|model| model.slug.clone());
            self.persist_current_inference_settings();
        }

        self.local_conversation_rename = self
            .local_conversation_catalog
            .entry(conversation_id)
            .and_then(|entry| entry.title.clone())
            .unwrap_or_default();
        self.local_conversation_project_directory = self
            .local_conversation_catalog
            .entry(conversation_id)
            .and_then(|entry| entry.project_directory.clone())
            .unwrap_or_default();
        self.select_local_conversation();
    }

    fn activate_local_conversation(&mut self, conversation_id: LocalConversationId) {
        if self.local_conversation_busy() {
            self.status =
                "finish or stop the active local turn before switching conversations".to_owned();
            return;
        }
        let was_archived = self
            .local_conversation_catalog
            .entry(conversation_id)
            .map(|entry| entry.archived);
        let Some(was_archived) = was_archived else {
            self.status = "local conversation is absent from workspace catalog".to_owned();
            return;
        };
        if was_archived {
            if let Err(error) =
                self.local_conversation_catalog
                    .set_archived(conversation_id, false, unix_now_ms())
            {
                self.status = format!("failed to restore local conversation: {error}");
                return;
            }
        }
        if let Err(error) = self
            .local_conversation_catalog
            .set_active(conversation_id, unix_now_ms())
        {
            self.status = format!("failed to select local conversation: {error}");
            return;
        }
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(conversation_id);
    }

    fn create_local_conversation(&mut self) {
        if self.local_conversation_busy() {
            self.status =
                "finish or stop the active local turn before creating a conversation".to_owned();
            return;
        }
        let conversation_id = LocalConversationId::new();
        self.local_conversation_catalog
            .create(conversation_id, unix_now_ms());
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(conversation_id);
        if self.mcp_agent_defaults.seed_new_conversations {
            self.conversation_developer_context = self.mcp_agent_defaults.bootstrap.clone();
            self.conversation_mcp_enabled = true;
            self.persist_current_inference_settings();
        }
        self.status = "new isolated local conversation created".to_owned();
    }

    fn rename_current_local_conversation(&mut self) {
        let title = self.local_conversation_rename.clone();
        if let Err(error) = self.local_conversation_catalog.rename(
            self.local_conversation_id,
            Some(title),
            unix_now_ms(),
        ) {
            self.status = format!("failed to rename local conversation: {error}");
        } else if self.persist_local_conversation_catalog() {
            self.local_conversation_rename = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.title.clone())
                .unwrap_or_default();
            self.status = "local conversation title saved".to_owned();
        }
    }

    fn save_current_project_directory(&mut self) {
        if self.local_conversation_busy() {
            self.status =
                "finish the active turn before changing this chat's project folder".to_owned();
            return;
        }
        if let Err(error) = self.local_conversation_catalog.set_project_directory(
            self.local_conversation_id,
            Some(self.local_conversation_project_directory.clone()),
            unix_now_ms(),
        ) {
            self.status = format!("project folder not saved: {error}");
            return;
        }
        if self.persist_local_conversation_catalog() {
            self.invalidate_project_memory_cache();
            self.local_conversation_project_directory = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.project_directory.clone())
                .unwrap_or_default();
            self.status = if self.local_conversation_project_directory.is_empty() {
                "project folder cleared for this chat".to_owned()
            } else {
                format!(
                    "project folder saved for this chat: {}",
                    self.local_conversation_project_directory
                )
            };
        }
    }

    /// In-flight project reads cannot remain eligible after revocation.
    fn invalidate_project_memory_cache(&mut self) {
        self.openviking_project_cache.invalidate();
    }

    fn change_project_memory_permission(&mut self, source: LocalConversationId, allow: bool) {
        if self.local_conversation_busy() {
            self.status =
                "finish the active turn before changing project memory sharing".to_owned();
            return;
        }
        let path = self
            .journal_path
            .with_file_name(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME);
        if let Err(error) = openviking_project_ui::change_permission_durable(
            &path,
            &self.local_conversation_catalog,
            &mut self.project_memory_grants,
            source,
            self.local_conversation_id,
            allow,
        ) {
            self.status = format!("project memory permission not saved: {error}");
            return;
        }
        self.invalidate_project_memory_cache();
        self.status = if allow {
            "Project checkpoint sharing granted for this source chat only".to_owned()
        } else {
            "Project checkpoint sharing revoked for this source chat".to_owned()
        };
    }

    fn archive_current_local_conversation(&mut self) {
        if self.local_conversation_busy() {
            self.status = "finish or stop the active local turn before archiving this conversation"
                .to_owned();
            return;
        }
        let archived = self.local_conversation_id;
        if let Err(error) =
            self.local_conversation_catalog
                .set_archived(archived, true, unix_now_ms())
        {
            self.status = format!("failed to archive local conversation: {error}");
            return;
        }
        let next = self
            .local_conversation_catalog
            .first_unarchived()
            .unwrap_or_else(|| {
                let id = LocalConversationId::new();
                self.local_conversation_catalog.create(id, unix_now_ms());
                id
            });
        if let Err(error) = self
            .local_conversation_catalog
            .set_active(next, unix_now_ms())
        {
            self.status = format!("failed to archive local conversation: {error}");
            return;
        }
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(next);
        self.status = "local conversation archived".to_owned();
    }

    fn select_historical_conversation(&mut self, local_conversation_id: LocalConversationId) {
        self.selected_remote_catalog_id = None;
        if self.selected_historical_conversation == Some(local_conversation_id)
            && self.loaded_historical_conversation == Some(local_conversation_id)
        {
            return;
        }

        self.selected_historical_conversation = Some(local_conversation_id);
        self.loaded_historical_conversation = None;
        self.historical_messages.clear();
        self.historical_load_pending = Some(local_conversation_id);
        self.reader_restore_pending = true;
        self.reader_search_hit = None;

        let Some(sender) = &self.persist_tx else {
            self.historical_load_pending = None;
            self.status = "cannot load historical conversation: persistence unavailable".to_owned();
            return;
        };
        let command = if self
            .live_mirrored_conversations
            .contains(&local_conversation_id)
        {
            PersistCommand::LoadLiveConversation {
                local_conversation_id,
            }
        } else {
            PersistCommand::LoadHistoricalConversation {
                local_conversation_id,
            }
        };
        if let Err(error) = sender.send(command) {
            self.historical_load_pending = None;
            self.status = format!("failed to queue conversation snapshot load: {error}");
        } else if self
            .live_mirrored_conversations
            .contains(&local_conversation_id)
        {
            self.status = "loading validated live mirror snapshot…".to_owned();
        } else {
            self.status = "loading verified historical snapshot…".to_owned();
        }
    }

    fn ensure_mirror_controller(&mut self) -> bool {
        if self.mirror_controller_worker.is_some() {
            return true;
        }
        let Some(provider) = self.account_bridge_provider.clone() else {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail = Some("browser bridge unavailable".to_owned());
            return false;
        };
        let catalog = self.remote_conversation_catalog.clone();
        let events = self.events.clone();
        let (command_tx, command_rx) = mpsc::channel();
        let (notice_tx, notice_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("chatarium-mirror-controller".to_owned())
            .spawn(move || {
                mirror_controller_worker(provider, catalog, events, None, command_rx, notice_tx)
            });
        match worker {
            Ok(worker) => {
                self.mirror_controller_tx = Some(command_tx);
                self.mirror_controller_notice_rx = Some(notice_rx);
                self.mirror_controller_worker = Some(worker);
                true
            }
            Err(error) => {
                self.mirror_controller_state = MirrorControllerState::Failed;
                self.mirror_controller_detail = Some(format!(
                    "failed to start production mirror controller: {error}"
                ));
                false
            }
        }
    }

    fn start_mirror_controller(&mut self) {
        if self.ensure_mirror_controller() {
            self.send_mirror_controller_command(MirrorControllerCommand::Start);
        }
    }

    fn recheck_remote_health(&mut self) {
        if self.ensure_mirror_controller() {
            self.send_mirror_controller_command(MirrorControllerCommand::RecheckHealth);
        }
    }

    fn send_mirror_controller_command(&mut self, command: MirrorControllerCommand) {
        let Some(sender) = &self.mirror_controller_tx else {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail = Some("mirror controller is not initialized".to_owned());
            return;
        };
        if sender.send(command).is_err() {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail =
                Some("mirror controller stopped unexpectedly".to_owned());
        }
    }

    fn process_mirror_controller_notices(&mut self) {
        let notices = self
            .mirror_controller_notice_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for notice in notices {
            match notice {
                MirrorControllerNotice::State {
                    state,
                    current_catalog_index,
                    detail,
                } => {
                    self.mirror_controller_state = state;
                    self.mirror_controller_current_item = current_catalog_index;
                    self.mirror_controller_detail = detail;
                }
                MirrorControllerNotice::Prepare {
                    remote_conversation_id,
                    catalog_index,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueuePrepare {
                            remote_conversation_id,
                            catalog_index,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::Capture {
                    remote_conversation_id,
                    body,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueueCapture {
                            remote_conversation_id,
                            body,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::Failure {
                    remote_conversation_id,
                    failure_class,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueueFailure {
                            remote_conversation_id,
                            failure_class,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::HealthSignal {
                    signal,
                    now_ms,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    let _ = sender.send(PersistCommand::RecordRemoteHealthSignal {
                        signal,
                        now_ms,
                        reply,
                    });
                }
                MirrorControllerNotice::HealthIntent {
                    intent,
                    now_ms,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    let _ = sender.send(PersistCommand::RecordRemoteHealthIntent {
                        intent,
                        now_ms,
                        reply,
                    });
                }
            }
        }
    }

    fn start_history_discovery(&mut self, repaint: &egui::Context) {
        if self.history_list_pending {
            diagnostics::debug(
                "history",
                "discovery request ignored: one is already running",
            );
            return;
        }
        self.history_discovery_started = true;

        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.account_bridge_status = "listener unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        let durable_history_items_at_start = self.remote_conversation_catalog.len();
        self.history_list_pending = true;
        self.history_bridge_proven = false;
        self.account_bridge_status = "listener ready · checking Edge extension…".to_owned();
        diagnostics::info(
            "history",
            "discovery started: probing authenticated Edge bridge",
        );

        let spawn = thread::Builder::new()
            .name("chatarium-history-discovery".to_owned())
            .spawn(move || {
                use chatarium_core::authenticated_session::SessionAuthenticationEvidence;

                diagnostics::info("history", "authentication probe dispatched");
                match provider.probe_authentication() {
                    Ok(observation) => {
                        let authenticated = matches!(
                            observation.evidence,
                            SessionAuthenticationEvidence::Authenticated
                        );
                        diagnostics::info(
                            "history",
                            format!(
                                "authentication probe complete: authenticated={authenticated} HTTP={}",
                                observation.http_status
                            ),
                        );
                        let _ =
                            notices.send(LiveMirrorFetchNotice::HistoryAuthenticationObserved {
                                observation,
                            });
                        if !authenticated {
                            repaint.request_repaint();
                            return;
                        }
                    }
                    Err(error) => {
                        diagnostics::error("history", format!("authentication probe failed: {error}"));
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryProbeFailed { error });
                        repaint.request_repaint();
                        return;
                    }
                }

                diagnostics::info(
                    "history",
                    "authenticated; starting frozen 0.3 bounded CDP reload discovery",
                );
                match provider.discover_history_surfaces() {
                    Ok(observation) => {
                        let unique_items = observation
                            .candidates
                            .iter()
                            .flat_map(|candidate| candidate.items.iter().map(|item| item.id.as_str()))
                            .collect::<HashSet<_>>()
                            .len();
                        diagnostics::info(
                            "history",
                            format!(
                                "discovery complete: candidates={} unique-items={} responses={} backend-200={}",
                                observation.candidates.len(),
                                unique_items,
                                observation.proof.responses_seen,
                                observation.proof.backend_http_200_seen,
                            ),
                        );
                        for candidate in &observation.candidates {
                            diagnostics::debug(
                                "history",
                                format!(
                                    "candidate surface={} kind={} conversations={} observations={} cursor-count={} truncated={}",
                                    candidate.path,
                                    candidate.surface_kind,
                                    candidate.conversation_count,
                                    candidate.observations,
                                    candidate.cursor_count,
                                    candidate.traversal_truncated,
                                ),
                            );
                        }
                        if !should_run_fresh_tab_history_recovery(
                            unique_items,
                            durable_history_items_at_start,
                        ) {
                            let _ = notices.send(
                                LiveMirrorFetchNotice::HistoryDiscoveryLoaded { observation },
                            );
                        } else {
                            diagnostics::warn(
                                "history",
                                "frozen 0.3 discovery observed zero conversations with no durable cache; starting isolated fresh-tab recovery",
                            );
                            match provider.discover_history_surfaces_fresh_tab() {
                                Ok(fallback) => {
                                    let fallback_unique_items = fallback
                                        .candidates
                                        .iter()
                                        .flat_map(|candidate| {
                                            candidate.items.iter().map(|item| item.id.as_str())
                                        })
                                        .collect::<HashSet<_>>()
                                        .len();
                                    diagnostics::info(
                                        "history",
                                        format!(
                                            "fresh-tab recovery complete: candidates={} unique-items={} responses={} backend-200={}",
                                            fallback.candidates.len(),
                                            fallback_unique_items,
                                            fallback.proof.responses_seen,
                                            fallback.proof.backend_http_200_seen,
                                        ),
                                    );
                                    let _ = notices.send(
                                        LiveMirrorFetchNotice::HistoryFreshTabDiscoveryLoaded {
                                            primary: observation,
                                            fallback,
                                        },
                                    );
                                }
                                Err(error) => {
                                    diagnostics::error(
                                        "history",
                                        format!("fresh-tab recovery failed: {error}"),
                                    );
                                    let _ = notices.send(
                                        LiveMirrorFetchNotice::HistoryFreshTabDiscoveryFailed {
                                            primary: observation,
                                            error: error.to_string(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        diagnostics::error("history", format!("discovery failed: {error}"));
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryDiscoveryFailed {
                            error: error.to_string(),
                        });
                    }
                }
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
            diagnostics::error(
                "history",
                format!("failed to start discovery worker: {error}"),
            );
            self.history_list_pending = false;
            self.account_bridge_status =
                format!("listener ready · failed to start browser check: {error}");
        }
    }

    fn open_discovered_remote_conversation(
        &mut self,
        remote_conversation_id: String,
        repaint: &egui::Context,
    ) {
        if self.remote_discovery_pending.is_some() || self.live_mirror_pending.is_some() {
            return;
        }
        if let Some(retry_after) = self
            .remote_mirror_retry_after
            .get(&remote_conversation_id)
            .copied()
        {
            let now = Instant::now();
            if retry_after > now {
                let remaining = retry_after.saturating_duration_since(now).as_secs().max(1);
                diagnostics::warn(
                    "mirror",
                    format!(
                        "rate-limit cooldown active for {}; retry in about {remaining}s",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                self.mirror_status =
                    format!("RATE LIMITED · retry available in about {remaining}s");
                self.status = "remote mirror is cooling down after ChatGPT HTTP 429".to_owned();
                return;
            }
            self.remote_mirror_retry_after
                .remove(&remote_conversation_id);
        }
        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.status = "cannot open remote chat: history bridge unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        diagnostics::info(
            "mirror",
            format!(
                "mirror requested for discovered conversation {}",
                diagnostics::short_id(&remote_conversation_id)
            ),
        );
        self.remote_mirror_failures.remove(&remote_conversation_id);
        self.remote_discovery_pending = Some(remote_conversation_id.clone());
        self.pending_history_fetch_proof = None;
        self.mirror_status =
            "FETCHING · opening temporary ChatGPT tab and waiting for first-party conversation response…"
                .to_owned();
        self.status =
            "mirroring remote ChatGPT conversation through first-party browser navigation…"
                .to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-remote-history-open".to_owned())
            .spawn(move || {
                diagnostics::info(
                    "mirror",
                    format!(
                        "browser capture started for {}",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(observation) => {
                        diagnostics::info(
                            "mirror",
                            format!(
                                "browser capture complete for {}: HTTP={} exact-response={} debugger={} network={}",
                                diagnostics::short_id(&remote_conversation_id),
                                observation.http_status,
                                observation.proof.exact_response_seen,
                                observation.proof.debugger_attached,
                                observation.proof.network_enabled,
                            ),
                        );
                        LiveMirrorFetchNotice::DiscoveredFetched {
                            remote_conversation_id,
                            body: observation.body,
                            proof: observation.proof,
                            http_status: observation.http_status,
                        }
                    }
                    Err(error) => {
                        diagnostics::error(
                            "mirror",
                            format!(
                                "browser capture failed for {}: {error}",
                                diagnostics::short_id(&remote_conversation_id)
                            ),
                        );
                        LiveMirrorFetchNotice::DiscoveredFailed {
                            remote_conversation_id,
                            error: error.to_string(),
                        }
                    }
                };
                let _ = notices.send(notice);
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
            diagnostics::error("mirror", format!("failed to start mirror worker: {error}"));
            self.remote_discovery_pending = None;
            self.status = format!("failed to start remote conversation fetch: {error}");
        }
    }

    fn refresh_live_mirror_catalog(&mut self) {
        match latest_live_mirror_catalog(&self.events) {
            Ok(catalog) => {
                self.live_mirrored_conversations = catalog
                    .iter()
                    .map(|entry| entry.local_conversation_id)
                    .collect();
                self.live_mirror_catalog = catalog;
                self.remote_catalog_view = build_remote_catalog_view(
                    &self.remote_conversation_catalog,
                    &self.events,
                    &self.live_mirror_catalog,
                )
                .unwrap_or_default();
                self.local_archive_search_index =
                    build_local_archive_search_index(&self.remote_catalog_view, &self.events);
            }
            Err(error) => {
                self.status = format!("live mirror replay warning: {error}");
            }
        }
    }

    fn sync_historical_conversation(
        &mut self,
        local_conversation_id: LocalConversationId,
        repaint: &egui::Context,
    ) {
        if self.live_mirror_pending.is_some() {
            return;
        }
        let Some(entry) = self
            .historical_catalog
            .iter()
            .find(|entry| entry.local_conversation_id == local_conversation_id)
        else {
            self.status = "cannot sync: imported conversation is missing".to_owned();
            return;
        };
        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.status = format!("cannot sync from ChatGPT: {}", self.account_bridge_status);
            return;
        };
        let Some(sender) = self.persist_tx.clone() else {
            self.status = "cannot sync from ChatGPT: persistence unavailable".to_owned();
            return;
        };

        let remote_conversation_id = entry.remote_conversation_id.clone();
        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        self.live_mirror_pending = Some(local_conversation_id);
        self.pending_history_fetch_proof = None;
        self.mirror_status =
            "FETCHING · opening temporary ChatGPT tab and waiting for first-party conversation response…"
                .to_owned();
        self.status =
            "mirroring imported ChatGPT conversation through first-party navigation…".to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-live-mirror-fetch".to_owned())
            .spawn(move || {
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(observation) => LiveMirrorFetchNotice::Fetched {
                        local_conversation_id,
                        remote_conversation_id,
                        body: observation.body,
                        proof: observation.proof,
                        http_status: observation.http_status,
                    },
                    Err(error) => LiveMirrorFetchNotice::Failed {
                        local_conversation_id,
                        error: error.to_string(),
                    },
                };
                let _ = notices.send(notice);
                repaint.request_repaint();
                drop(sender);
            });

        if let Err(error) = spawn {
            self.live_mirror_pending = None;
            self.status = format!("failed to start live mirror fetch worker: {error}");
        }
    }

    fn process_live_mirror_fetch_notices(&mut self) {
        let notices = self
            .live_mirror_fetch_rx
            .try_iter()
            .collect::<Vec<LiveMirrorFetchNotice>>();

        for notice in notices {
            match notice {
                LiveMirrorFetchNotice::HistoryAuthenticationObserved { observation } => {
                    use chatarium_core::authenticated_session::SessionAuthenticationEvidence;

                    self.history_bridge_proven = false;
                    match observation.evidence {
                        SessionAuthenticationEvidence::Authenticated => {
                            self.account_bridge_status = format!(
                                "PROOF PARTIAL: {} · auth=yes · HTTP {} · parser=pending · semantic=pending · durable=pending",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        SessionAuthenticationEvidence::Unauthenticated => {
                            self.history_list_pending = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=no · HTTP {}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        SessionAuthenticationEvidence::Unknown => {
                            self.history_list_pending = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=unknown · HTTP {}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                    }
                }
                LiveMirrorFetchNotice::HistoryProbeFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status = history_probe_failure_status(&error);
                }
                LiveMirrorFetchNotice::HistoryListLoaded { observation } => {
                    self.history_list_pending = false;
                    self.remote_conversation_total = Some(observation.page.total);
                    self.remote_conversation_catalog = observation.page.items;

                    let semantic = classify_history_list_semantics(
                        observation.page.total,
                        !self.historical_catalog.is_empty() || !self.live_mirror_catalog.is_empty(),
                    );
                    match semantic {
                        HistoryListSemanticVerdict::Contradiction => {
                            self.history_bridge_proven = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=yes · HTTP {} · parser=yes · semantic=contradiction(remote total=0 while local history exists)",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        HistoryListSemanticVerdict::UnconfirmedZero => {
                            self.history_bridge_proven = false;
                            self.account_bridge_status = format!(
                                "PROOF PARTIAL: {} · auth=yes · HTTP {} · parser=yes · semantic=unconfirmed-zero · items=0 · total=0",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        HistoryListSemanticVerdict::Valid => {
                            self.history_bridge_proven = true;
                            self.account_bridge_status = format!(
                                "PROOF: {} · auth=yes · HTTP {} · parser=yes · semantic=yes · items={} · total={}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                                self.remote_conversation_catalog.len(),
                                self.remote_conversation_total.unwrap_or(0),
                            );
                        }
                    }
                }
                LiveMirrorFetchNotice::HistoryListFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status =
                        format!("PROOF FAILED after authenticated extension path: {error}");
                }
                LiveMirrorFetchNotice::HistoryDiscoveryLoaded { observation } => {
                    diagnostics::info(
                        "history",
                        format!(
                            "UI received discovery result: surfaces={} responses={} backend-200={}",
                            observation.candidates.len(),
                            observation.proof.responses_seen,
                            observation.proof.backend_http_200_seen
                        ),
                    );
                    self.history_list_pending = false;
                    self.remote_conversation_total = None;

                    let candidate_count = observation.candidates.len();
                    let previously_observed = self.remote_conversation_catalog.len();
                    let mut best_surface = None::<(String, u64)>;
                    for candidate in &observation.candidates {
                        if best_surface
                            .as_ref()
                            .is_none_or(|(_, count)| candidate.conversation_count > *count)
                        {
                            best_surface =
                                Some((candidate.path.clone(), candidate.conversation_count));
                        }
                    }
                    let (catalog, current_pass_observed) = merge_history_discovery_catalog(
                        std::mem::take(&mut self.remote_conversation_catalog),
                        &observation.candidates,
                    );
                    self.remote_conversation_catalog = catalog;
                    self.remote_catalog_view = build_remote_catalog_view(
                        &self.remote_conversation_catalog,
                        &self.events,
                        &self.live_mirror_catalog,
                    )
                    .unwrap_or_default();

                    let best_surface = best_surface
                        .map(|(path, count)| format!("{path} ({count})"))
                        .unwrap_or_else(|| "none".to_owned());
                    if current_pass_observed == 0 {
                        diagnostics::warn(
                            "history",
                            format!(
                                "discovery completed with zero current-pass items; retained-items={previously_observed}"
                            ),
                        );
                        self.history_bridge_proven = false;
                        self.account_bridge_status = format!(
                            "DISCOVERY INCOMPLETE: {} · candidates={} · current-pass-items=0 · retained-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&observation.proof),
                            candidate_count,
                            previously_observed,
                            best_surface,
                        );
                    } else {
                        diagnostics::info(
                            "history",
                            format!(
                                "history catalog ready: current-pass-items={current_pass_observed} accumulated-items={} best-surface={best_surface}",
                                self.remote_conversation_catalog.len()
                            ),
                        );
                        let cache_path = remote_history_cache_path(&self.journal_path);
                        match persist_remote_history_cache(
                            &cache_path,
                            &self.remote_conversation_catalog,
                        ) {
                            Ok(()) => diagnostics::info(
                                "history",
                                format!(
                                    "discovered history cache updated: {} items at {}",
                                    self.remote_conversation_catalog.len(),
                                    cache_path.display()
                                ),
                            ),
                            Err(error) => diagnostics::warn(
                                "history",
                                format!("failed to update discovered history cache: {error}"),
                            ),
                        }
                        self.history_bridge_proven = true;
                        self.account_bridge_status = format!(
                            "DISCOVERED: {} · candidates={} · current-pass-items={} · accumulated-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&observation.proof),
                            candidate_count,
                            current_pass_observed,
                            self.remote_conversation_catalog.len(),
                            best_surface,
                        );
                    }
                }
                LiveMirrorFetchNotice::HistoryFreshTabDiscoveryLoaded { primary, fallback } => {
                    diagnostics::info(
                        "history",
                        format!(
                            "UI received fresh-tab recovery: primary-responses={} fallback-surfaces={} fallback-responses={} fallback-backend-200={}",
                            primary.proof.responses_seen,
                            fallback.candidates.len(),
                            fallback.proof.responses_seen,
                            fallback.proof.backend_http_200_seen,
                        ),
                    );
                    self.history_list_pending = false;
                    self.remote_conversation_total = None;

                    let previously_observed = self.remote_conversation_catalog.len();
                    let candidate_count = fallback.candidates.len();
                    let mut best_surface = None::<(String, u64)>;
                    for candidate in &fallback.candidates {
                        if best_surface
                            .as_ref()
                            .is_none_or(|(_, count)| candidate.conversation_count > *count)
                        {
                            best_surface =
                                Some((candidate.path.clone(), candidate.conversation_count));
                        }
                    }
                    let (catalog, current_pass_observed) = merge_history_discovery_catalog(
                        std::mem::take(&mut self.remote_conversation_catalog),
                        &fallback.candidates,
                    );
                    self.remote_conversation_catalog = catalog;
                    self.remote_catalog_view = build_remote_catalog_view(
                        &self.remote_conversation_catalog,
                        &self.events,
                        &self.live_mirror_catalog,
                    )
                    .unwrap_or_default();

                    let best_surface = best_surface
                        .map(|(path, count)| format!("{path} ({count})"))
                        .unwrap_or_else(|| "none".to_owned());

                    if current_pass_observed == 0 {
                        diagnostics::warn(
                            "history",
                            format!(
                                "fresh-tab recovery completed with zero items; retained-items={previously_observed}"
                            ),
                        );
                        self.history_bridge_proven = false;
                        self.account_bridge_status = format!(
                            "DISCOVERY INCOMPLETE: primary={} · fresh-tab={} · candidates={} · current-pass-items=0 · retained-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&primary.proof),
                            fresh_tab_history_discovery_proof_label(&fallback.proof),
                            candidate_count,
                            previously_observed,
                            best_surface,
                        );
                    } else {
                        diagnostics::info(
                            "history",
                            format!(
                                "fresh-tab history catalog ready: current-pass-items={current_pass_observed} accumulated-items={} best-surface={best_surface}",
                                self.remote_conversation_catalog.len()
                            ),
                        );
                        let cache_path = remote_history_cache_path(&self.journal_path);
                        match persist_remote_history_cache(
                            &cache_path,
                            &self.remote_conversation_catalog,
                        ) {
                            Ok(()) => diagnostics::info(
                                "history",
                                format!(
                                    "discovered history cache updated: {} items at {}",
                                    self.remote_conversation_catalog.len(),
                                    cache_path.display()
                                ),
                            ),
                            Err(error) => diagnostics::warn(
                                "history",
                                format!("failed to update discovered history cache: {error}"),
                            ),
                        }
                        self.history_bridge_proven = true;
                        self.account_bridge_status = format!(
                            "DISCOVERED VIA FRESH TAB: primary={} · fresh-tab={} · candidates={} · current-pass-items={} · accumulated-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&primary.proof),
                            fresh_tab_history_discovery_proof_label(&fallback.proof),
                            candidate_count,
                            current_pass_observed,
                            self.remote_conversation_catalog.len(),
                            best_surface,
                        );
                    }
                }
                LiveMirrorFetchNotice::HistoryFreshTabDiscoveryFailed { primary, error } => {
                    diagnostics::error(
                        "history",
                        format!(
                            "UI received fresh-tab recovery failure after primary zero-result: {error}"
                        ),
                    );
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status = format!(
                        "DISCOVERY INCOMPLETE: primary={} · fresh-tab-recovery-failed={} · retained-items={} · coverage=unknown",
                        history_discovery_proof_label(&primary.proof),
                        error,
                        self.remote_conversation_catalog.len(),
                    );
                }
                LiveMirrorFetchNotice::HistoryDiscoveryFailed { error } => {
                    diagnostics::error(
                        "history",
                        format!("UI received discovery failure: {error}"),
                    );
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status =
                        format!("PROOF FAILED during CDP history discovery: {error}");
                }
                LiveMirrorFetchNotice::DiscoveredFetched {
                    remote_conversation_id,
                    body,
                    proof,
                    http_status,
                } => {
                    diagnostics::info(
                        "mirror",
                        format!(
                            "exact response arrived remote={} HTTP={http_status}; queuing validation/persistence",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    let Some(sender) = &self.persist_tx else {
                        self.remote_discovery_pending = None;
                        self.remote_mirror_failures.insert(
                            remote_conversation_id.clone(),
                            "local persistence unavailable".to_owned(),
                        );
                        self.mirror_status =
                            "FAILED · exact first-party response captured, but local persistence is unavailable"
                                .to_owned();
                        self.status = "remote conversation fetched, but persistence is unavailable"
                            .to_owned();
                        continue;
                    };
                    self.pending_history_fetch_proof = Some(PendingHistoryFetchProof {
                        local_conversation_id: None,
                        remote_conversation_id: remote_conversation_id.clone(),
                        proof,
                        http_status,
                    });
                    self.mirror_status =
                        "VALIDATED · exact remote response captured · persisting durable local mirror…"
                            .to_owned();
                    if let Err(error) = sender.send(PersistCommand::PromoteDiscoveredLiveMirror {
                        expected_remote_conversation_id: remote_conversation_id.clone(),
                        body,
                    }) {
                        self.pending_history_fetch_proof = None;
                        self.remote_discovery_pending = None;
                        self.remote_mirror_failures
                            .insert(remote_conversation_id, error.to_string());
                        self.mirror_status =
                            format!("FAILED · could not queue durable mirror: {error}");
                        self.status = format!("failed to queue remote mirror creation: {error}");
                    } else {
                        self.status =
                            "remote conversation validated; creating durable live mirror…"
                                .to_owned();
                    }
                }
                LiveMirrorFetchNotice::DiscoveredFailed {
                    remote_conversation_id,
                    error,
                } => {
                    diagnostics::error(
                        "mirror",
                        format!(
                            "mirror fetch failed remote={}: {error}",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.remote_mirror_failures
                        .insert(remote_conversation_id.clone(), error.clone());
                    if error.contains("HTTP 429") {
                        self.remote_mirror_retry_after.insert(
                            remote_conversation_id.clone(),
                            Instant::now() + Duration::from_secs(60),
                        );
                        self.mirror_status =
                            "RATE LIMITED · ChatGPT returned HTTP 429 · cooldown 60s".to_owned();
                    } else {
                        self.mirror_status = format!("FAILED · mirror fetch: {error}");
                    }
                    self.status = format!("remote ChatGPT conversation fetch failed: {error}");
                }
                LiveMirrorFetchNotice::Fetched {
                    local_conversation_id,
                    remote_conversation_id,
                    body,
                    proof,
                    http_status,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        self.live_mirror_pending = None;
                        self.mirror_status =
                            "FAILED · exact first-party response captured, but local persistence is unavailable"
                                .to_owned();
                        self.status =
                            "live conversation fetched, but persistence is unavailable".to_owned();
                        continue;
                    };
                    self.pending_history_fetch_proof = Some(PendingHistoryFetchProof {
                        local_conversation_id: Some(local_conversation_id),
                        remote_conversation_id: remote_conversation_id.clone(),
                        proof,
                        http_status,
                    });
                    self.mirror_status =
                        "VALIDATED · exact remote response captured · persisting durable local mirror…"
                            .to_owned();
                    if let Err(error) = sender.send(PersistCommand::PromoteHistoricalLiveMirror {
                        local_conversation_id,
                        expected_remote_conversation_id: remote_conversation_id,
                        body,
                    }) {
                        self.pending_history_fetch_proof = None;
                        self.live_mirror_pending = None;
                        self.mirror_status =
                            format!("FAILED · could not queue durable mirror: {error}");
                        self.status = format!("failed to queue live mirror promotion: {error}");
                    } else {
                        self.status =
                            "live response validated by browser; persisting mirror evidence…"
                                .to_owned();
                    }
                }
                LiveMirrorFetchNotice::Failed {
                    local_conversation_id,
                    error,
                } => {
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.mirror_status = format!("FAILED · mirror fetch: {error}");
                    self.status = format!("live ChatGPT fetch failed: {error}");
                }
            }
        }
    }

    fn queue_draft_snapshot(&mut self) {
        self.draft_revision = self.draft_revision.saturating_add(1);
        let revision = self.draft_revision;
        let Some(sender) = &self.persist_tx else {
            self.status = "persistence unavailable; draft is not durable".to_owned();
            return;
        };

        if let Err(error) = sender.send(PersistCommand::SaveDraft {
            conversation_id: self.local_conversation_id,
            revision,
            text: self.draft.clone(),
        }) {
            self.status = format!("persistence worker unavailable: {error}");
        }
    }

    fn stage_attachment(&mut self, path: &std::path::Path) {
        if self.commit_in_flight.is_some() {
            self.status = "cannot change attachment during durable commit".to_owned();
            return;
        }
        match attachment_intake::prepare(path) {
            Ok(attachment) => {
                self.status = format!(
                    "staged {} ({} bytes); content is not durable until Send",
                    attachment.filename, attachment.byte_len
                );
                self.staged_attachment = Some(attachment);
            }
            Err(error) => {
                self.status = format!("attachment not staged: {error}");
            }
        }
    }

    fn commit_current_message(&mut self) {
        if self.draft.trim().is_empty() || self.commit_in_flight.is_some() {
            return;
        }
        if self.staged_attachment.is_some() && !self.remote_connected() {
            self.status = "attachment send requires an active ChatGPT connection".to_owned();
            return;
        }
        let send_started = Instant::now();
        // Final Send admission cannot use a prior topic's verified checkpoint.
        // A newly typed topic may still be fetching; in that case proceed
        // without old-topic memory instead of smuggling stale evidence.
        let send_query = self.draft.clone();
        self.observe_openviking_query(self.local_conversation_id, &send_query, true);
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot commit: persistence unavailable".to_owned();
            return;
        };

        let request_id = self.next_commit_request;
        self.next_commit_request = self.next_commit_request.saturating_add(1);
        let message = AuthoredUserMessage::new(
            self.local_conversation_id,
            LocalTurnId::new(),
            LocalMessageId::new(),
            self.draft.clone(),
        );
        let mut one_shot_memory_ids = Vec::new();
        let mut one_shot_memory_snapshot_after_sequence = None;
        if self.remote_connected() {
            if let Some(model) = self.selected_model.clone() {
                let request_patch = match self.current_behavior_request_patch() {
                    Ok(patch) => patch,
                    Err(error) => {
                        self.status = format!("cannot send with current behavior profile: {error}");
                        return;
                    }
                };
                let presence = send_admission_gate::Presence::from_kinds(
                    self.events.iter().map(|event| event.kind),
                );
                let routed_context = match send_admission_gate::replay_when(presence.routed, || {
                    admitted_routed_context_messages(&self.events, self.local_conversation_id)
                }) {
                    Ok(messages) => messages,
                    Err(error) => {
                        self.status = format!("cannot snapshot admitted routed context: {error}");
                        return;
                    }
                };
                let tool_result_context =
                    match send_admission_gate::replay_when(presence.tools, || {
                        admitted_tool_result_context_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!("cannot snapshot admitted tool results: {error}");
                            return;
                        }
                    };
                let mut local_memory_context =
                    match send_admission_gate::replay_when(presence.local_memory, || {
                        admitted_local_memory_messages(&self.events, self.local_conversation_id)
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!("cannot snapshot admitted local memory: {error}");
                            return;
                        }
                    };
                // Freeze the already verified, strictly same-conversation
                // OpenViking evidence at Send click alongside local admissions.
                // A later background result cannot mutate this request.
                local_memory_context.extend(
                    self.openviking_project_cache.admitted_relevant_context(
                        self.openviking_retrieval_cache
                            .get(&self.local_conversation_id)
                            .map(Vec::as_slice),
                        self.local_conversation_id,
                        &self.local_conversation_catalog,
                        &self.project_memory_grants,
                        &self.draft,
                    ),
                );
                let selected_one_shot = self
                    .local_memory_one_shot_selections
                    .get(&self.local_conversation_id)
                    .cloned()
                    .unwrap_or_default();
                let (local_memory_one_shot, snapshot_after_sequence) =
                    match snapshot_one_shot_local_memory(
                        &self.events,
                        self.local_conversation_id,
                        &selected_one_shot,
                    ) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.status = format!("cannot snapshot one-shot local memory: {error}");
                            return;
                        }
                    };
                one_shot_memory_ids = local_memory_one_shot
                    .iter()
                    .map(|memory| memory.memory_id)
                    .collect();
                one_shot_memory_snapshot_after_sequence = snapshot_after_sequence;
                let controller_result_context =
                    match send_admission_gate::replay_when(presence.controller_results, || {
                        admitted_controller_worker_result_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status =
                                format!("cannot snapshot admitted controller results: {error}");
                            return;
                        }
                    };
                let controller_coordination_result_context =
                    match send_admission_gate::replay_when(presence.coordination_results, || {
                        admitted_controller_coordination_result_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!(
                                "cannot snapshot admitted controller coordination results: {error}"
                            );
                            return;
                        }
                    };
                self.commit_remote_intents.insert(
                    request_id,
                    PendingInferenceIntent {
                        model,
                        instructions: (!self.conversation_instructions.trim().is_empty())
                            .then(|| self.conversation_instructions.clone()),
                        developer_context: {
                            let mut context = self.conversation_developer_context.clone();
                            if let Some(directory) = self
                                .local_conversation_catalog
                                .entry(self.local_conversation_id)
                                .and_then(|entry| entry.project_directory.as_deref())
                            {
                                context.push_str(&format!(
                                    "\n\nChatarium project directory for this conversation: {directory}\n                                     For local Git, file, and shell tasks, invoke chatarium.builtin::shell.command directly,                                      e.g. {{\"command\":\"git status --short --branch\"}}.                                      The shell's default cwd is this project folder; an explicit cwd overrides it.                                      tools.list is optional discovery, not a prerequisite to shell execution.                                      Do not claim results before the tool reply is admitted.                                      This directory is a workspace hint, not a request to execute anything."
                                ));
                            }
                            context.push_str(&self.coordination_workspace.identity_context(
                                &self.local_conversation_id.to_string(),
                            ));
                            context
                        },
                        routed_context,
                        tool_result_context,
                        local_memory_context,
                        controller_result_context,
                        controller_coordination_result_context,
                        request_patch,
                        attachment: self.staged_attachment.clone(),
                        coordination_ids: self.coordination_workspace.context_delivered_ids(
                            &self.local_conversation_id.to_string(),
                        ),
                    },
                );
            }
        }
        let send_elapsed = send_started.elapsed();
        if send_elapsed >= Duration::from_millis(100) {
            diagnostics::warn(
                "send",
                format!(
                    "Send admission took {}ms across {} journal events",
                    send_elapsed.as_millis(),
                    self.events.len()
                ),
            );
        }
        match sender.send(PersistCommand::CommitMessage {
            request_id,
            message,
            attachment: self
                .staged_attachment
                .as_ref()
                .map(attachment_intake::PreparedAttachment::durable_record),
            one_shot_memory_ids,
            one_shot_memory_snapshot_after_sequence,
        }) {
            Ok(()) => {
                self.commit_in_flight = Some(request_id);
                self.status = "committing exact user message to local journal…".to_owned();
            }
            Err(error) => {
                self.commit_remote_intents.remove(&request_id);
                self.status = format!("failed to queue commit: {error}");
            }
        }
    }

    fn queue_turn_event(&mut self, turn_id: LocalTurnId, kind: EventKind, payload: String) -> bool {
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist remote turn evidence: persistence unavailable".to_owned();
            return false;
        };
        if let Err(error) = sender.send(PersistCommand::AppendTurnEvent {
            turn_id,
            kind,
            payload,
        }) {
            self.status = format!("failed to queue remote turn evidence: {error}");
            return false;
        }
        true
    }

    /// A diagnostic of the latest non-authored wake. Completion means model
    /// output finished, not that a worker sent a separate coordination reply.
    fn latest_coordination_wake_status(&self) -> Option<String> {
        let conversation = self.local_conversation_id.to_string();
        let latest = self
            .events
            .iter()
            .rev()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .find(|payload| payload["conversation_id"].as_str() == Some(conversation.as_str()))?;
        let request_id = latest["turn_id"].as_str()?;
        let ids = latest["inbox_ids"]
            .as_array()?
            .iter()
            .filter_map(Value::as_u64)
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = self
            .delivery
            .for_conversation(&conversation)
            .find(|attempt| attempt.request_id == request_id)
            .map(|attempt| match attempt.phase {
                delivery_telemetry::Phase::Durable => "activation saved; dispatch pending",
                delivery_telemetry::Phase::BridgeQueued => "request queued to local bridge",
                delivery_telemetry::Phase::WireAttempted => "HTTP attempted; receipt unconfirmed",
                delivery_telemetry::Phase::Acknowledged => "server acknowledged; awaiting model",
                delivery_telemetry::Phase::Streaming => "model producing output",
                delivery_telemetry::Phase::RetryScheduled => "rate-limit retry scheduled",
                delivery_telemetry::Phase::Complete => {
                    "model replied; separate coordination reply unverified"
                }
                delivery_telemetry::Phase::Failed => "request failed",
                delivery_telemetry::Phase::Unconfirmed => "delivery uncertain; not resent",
            })
            .unwrap_or("recorded earlier or not dispatched this session");
        Some(format!("Last auto-wake [{ids}]: {outcome}"))
    }

    /// Operator-armed only for this open chat. Each incoming directive must
    /// receive a durable, at-most-once claim before a model turn is journaled.
    fn maybe_wake_open_coordination_worker(&mut self) {
        if self.coordination_auto_wake_targets.is_empty()
            || self.selected_historical_conversation.is_some()
            || self.selected_remote_catalog_id.is_some()
            || self.coordination_auto_wake_count >= 8
            || self.local_conversation_busy()
            || !self.remote_connected()
            || self.capability_probe.running()
        {
            return;
        }
        // Serialized, deterministic scheduling; every worker uses its own
        // saved model and behavior, never the currently displayed chat's.
        let candidates = self
            .coordination_auto_wake_targets
            .iter()
            .filter_map(|&worker| {
                if self.local_conversation_catalog.entry(worker)?.archived {
                    return None;
                }
                if worker == self.local_conversation_id && !self.draft.trim().is_empty() {
                    return None;
                }
                let model = if worker == self.local_conversation_id {
                    self.selected_model.clone()?
                } else {
                    self.inference_settings.for_conversation(worker).model?
                };
                if !self
                    .remote_models
                    .iter()
                    .any(|available| available.slug == model)
                {
                    return None;
                }
                let profile = if worker == self.local_conversation_id {
                    self.conversation_behavior_profile.clone()
                } else {
                    self.behavior_profiles.for_conversation(worker)
                };
                let gate = context_composer::CapabilityGate::from_contract(
                    self.local_inference_contract.as_ref(),
                    self.remote_session.profile_id.as_deref(),
                    Some(model.as_str()),
                );
                if profile.request_patch(&gate).is_err() {
                    return None;
                }
                self.coordination_workspace
                    .next_eligible_wake_id(&worker.to_string())
                    .map(|message_id| (worker, message_id))
            })
            .collect::<Vec<_>>();
        let Some(target) = coordination_wake_scheduler::oldest_candidate(&candidates) else {
            return;
        };
        let target_message_id = candidates
            .iter()
            .find_map(|(worker, message_id)| (*worker == target).then_some(*message_id))
            .expect("selected wake candidate must have its message ID");
        // Do not spend the durable at-most-once wake claim while authorized
        // OpenViking reads for this exact worker are still in flight. Each
        // read runs off the egui thread. An offline backend uses backoff and
        // the wake proceeds without invented historical memory.
        let directive = match self
            .coordination_workspace
            .wake_dispatch_envelope(&target.to_string(), target_message_id)
        {
            Ok(exact) => exact.model_context(),
            Err(_) => return,
        };
        if !self.background_worker_memory_ready(target, &directive) {
            return;
        }
        let conversation = target.to_string();
        let path = self.coordination_store_path();
        let claim = match self
            .coordination_workspace
            .claim_next_wake_durable(&path, &conversation)
        {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(error) => {
                self.status = format!("coordination wake claim not saved: {error}");
                self.coordination_auto_wake_targets.remove(&target);
                return;
            }
        };
        // The saved claim remains spent if journal delivery becomes uncertain.
        // It is never silently replayed or reissued as a second remote request.
        let inbox_ids = vec![claim.message_id];
        let turn_id = LocalTurnId::new();
        let payload = serde_json::json!({
            "schema": "chatarium-coordination-activation",
            "version": 1,
            "conversation_id": conversation,
            "turn_id": turn_id.to_string(),
            "inbox_ids": inbox_ids,
            "not_user_authored": true
        })
        .to_string();
        self.coordination_pending_wake = Some((turn_id, target, inbox_ids.clone()));
        if self.queue_turn_event(turn_id, EventKind::CoordinationActivationStarted, payload) {
            self.coordination_auto_wake_count += 1;
            self.status = format!(
                "journaling non-authored coordination wake for {} message(s)",
                inbox_ids.len()
            );
        } else {
            self.coordination_pending_wake = None;
            self.coordination_auto_wake_targets.remove(&target);
        }
    }

    fn dispatch_durable_coordination_wake(&mut self, turn_id: LocalTurnId, start: &EventEnvelope) {
        let Some((pending_id, conversation_id, inbox_ids)) = self.coordination_pending_wake.take()
        else {
            return;
        };
        if pending_id != turn_id
            || !self
                .coordination_auto_wake_targets
                .contains(&conversation_id)
        {
            self.status =
                "coordination wake target changed or disarmed; dispatch denied".to_owned();
            return;
        }
        let [message_id] = inbox_ids.as_slice() else {
            self.status = "coordination wake requires exactly one claimed directive".to_owned();
            return;
        };
        // Delivery permission and source identity must still be valid now,
        // after the activation event was durably appended.
        let exact = match self
            .coordination_workspace
            .wake_dispatch_envelope(&conversation_id.to_string(), *message_id)
        {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake authority denied: {error}");
                return;
            }
        };
        if self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || !self.remote_connected()
        {
            self.status = "coordination wake persisted; bridge unavailable".to_owned();
            return;
        }
        let selected = conversation_id == self.local_conversation_id;
        let settings = self
            .inference_settings
            .for_conversation_with_mcp_defaults(conversation_id, &self.mcp_agent_defaults);
        let Some(model) = (if selected {
            self.selected_model.clone()
        } else {
            settings.model.clone()
        }) else {
            self.status = "coordination wake model is not configured".to_owned();
            return;
        };
        let profile = if selected {
            self.conversation_behavior_profile.clone()
        } else {
            self.behavior_profiles.for_conversation(conversation_id)
        };
        let gate = context_composer::CapabilityGate::from_contract(
            self.local_inference_contract.as_ref(),
            self.remote_session.profile_id.as_deref(),
            Some(&model),
        );
        let patch = match profile.request_patch(&gate) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake policy rejected: {error}");
                return;
            }
        };
        let mut transcript = context_transcript(&projected_local_display_messages(
            &self.events,
            conversation_id,
        ));
        let tools = match admitted_tool_result_context_messages(&self.events, conversation_id) {
            Ok(tools) => tools,
            Err(error) => {
                self.status = format!("coordination wake tool evidence invalid: {error}");
                return;
            }
        };
        let frozen_sources = tools
            .iter()
            .map(|item| item.source.clone())
            .collect::<Vec<_>>();
        transcript.extend(tools);
        match admitted_routed_context_messages(&self.events, conversation_id) {
            Ok(context) => transcript.extend(context),
            Err(error) => {
                self.status = format!("coordination wake routed context invalid: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        // Freeze the exact worker-owned and currently granted project memory
        // at wake dispatch. Never use the displayed conversation's cache or
        // borrow the controller's authorization for an inactive worker.
        transcript.extend(
            self.openviking_project_cache.admitted_relevant_context(
                self.openviking_retrieval_cache
                    .get(&conversation_id)
                    .map(Vec::as_slice),
                conversation_id,
                &self.local_conversation_catalog,
                &self.project_memory_grants,
                &exact.model_context(),
            ),
        );
        let mut activation = context_composer::TranscriptMessage::coordination_activation(
            turn_id.to_string(),
            start.sequence,
            inbox_ids.clone(),
        );
        // User-level context, never a developer/system instruction. This
        // includes the exact claimed directive even when the regular inbox
        // preview has excluded it because later messages filled that window.
        activation.text.push_str("\n\n");
        activation.text.push_str(&exact.model_context());
        transcript.push(activation);
        let mut developer = if selected {
            self.conversation_developer_context.clone()
        } else {
            settings.developer_context.clone()
        };
        developer.push_str(
            &self
                .coordination_workspace
                .identity_context(&conversation_id.to_string()),
        );
        let instructions = if selected {
            &self.conversation_instructions
        } else {
            &settings.instructions
        };
        let plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            instructions,
            &developer,
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("coordination wake context budget rejected: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            conversation_id,
            "coordination_auto_wake",
            &frozen_sources,
            &plan,
        ) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake manifest rejected: {error}");
                return;
            }
        };
        let request_id = turn_id.to_string();
        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                turn_id,
                &request_id,
                Some(&model),
                None,
                Some("non-authored coordination inbox wake"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.status = format!("coordination wake context rejected: {error}");
                return;
            }
        };
        self.coordination_pending_admissions
            .insert(request_id.clone(), inbox_ids);
        self.delivery
            .begin(conversation_id.to_string(), request_id.clone());
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id,
            turn_id,
            request_id,
            model,
            input: plan.input_json(),
            instructions: plan.instructions,
            request_patch: patch,
        });
        if self.queue_turn_event(turn_id, EventKind::DispatchAttempted, payload) {
            self.status = "coordination wake journaled; remote dispatch queued".to_owned();
        } else {
            self.coordination_pending_admissions
                .remove(&turn_id.to_string());
            self.pending_remote_turn = None;
        }
    }

    /// Authenticate the inactive worker identity against its persisted
    /// activation turn and still-active exact claimed directive.
    fn is_authorized_background_wake_turn(
        &self,
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
    ) -> bool {
        if !self
            .coordination_auto_wake_targets
            .contains(&conversation_id)
        {
            return false;
        }
        let conversation = conversation_id.to_string();
        let turn = turn_id.to_string();
        self.events
            .iter()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .any(|payload| {
                payload["conversation_id"].as_str() == Some(conversation.as_str())
                    && payload["turn_id"].as_str() == Some(turn.as_str())
                    && payload["inbox_ids"].as_array().is_some_and(|ids| {
                        ids.len() == 1
                            && ids[0].as_u64().is_some_and(|message_id| {
                                self.coordination_workspace
                                    .validate_wake_claim(&conversation, message_id)
                                    .is_ok()
                            })
                    })
            })
    }

    /// Terminal evidence has already been appended to the main event journal
    /// when this method runs. Only now may an active, saved wake claim be
    /// cleared; an unknown transport outcome still consumes its allowance.
    fn finish_durable_coordination_wake(&mut self, turn_id: LocalTurnId) {
        // The first model completion may be intermediate if an authorized
        // coordination tool requires a second Responses request.
        if self
            .pending_remote_turn
            .as_ref()
            .is_some_and(|pending| pending.turn_id == turn_id)
            || self
                .active_remote_turn
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id)
            || self
                .agent_mcp_flow
                .as_ref()
                .is_some_and(|flow| flow.turn_id == turn_id)
        {
            return;
        }
        let wake = self
            .events
            .iter()
            .rev()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .find(|value| value["turn_id"].as_str() == Some(turn_id.to_string().as_str()));
        let Some(wake) = wake else {
            return;
        };
        let (Some(conversation), Some(message_id)) = (
            wake["conversation_id"].as_str(),
            wake["inbox_ids"]
                .as_array()
                .and_then(|ids| (ids.len() == 1).then(|| ids[0].as_u64()).flatten()),
        ) else {
            return;
        };
        if !self.coordination_workspace.wake_leases.iter().any(|lease| {
            lease.conversation == conversation && lease.active_message_id == Some(message_id)
        }) {
            return;
        }
        let path = self.coordination_store_path();
        if let Err(error) =
            self.coordination_workspace
                .finish_wake_claim_durable(&path, conversation, message_id)
        {
            self.status = format!("coordination wake terminal saved but claim not closed: {error}");
            self.coordination_auto_wake_targets.clear();
        }
    }

    /// A topic change immediately removes both owner and project evidence.
    /// An in-flight owner read is marked stale, and the project cache similarly
    /// discards its old completion. No network request runs on the GUI thread.
    fn observe_openviking_query(
        &mut self,
        owner: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        let (changed, ready) =
            self.openviking_query_tracker
                .observe(owner, query, Instant::now(), interactive);
        if changed {
            self.openviking_retrieval_cache.remove(&owner);
            self.openviking_retrieval_retry_after.remove(&owner);
            if self.openviking_retrieval_in_flight.contains(&owner) {
                self.openviking_retrieval_stale.insert(owner);
            }
            self.openviking_project_cache.invalidate_destination(owner);
        }
        ready
    }

    /// Schedule a source-owner-scoped read on the persistence worker.
    /// Return false only while a read is outstanding, so offline OpenViking
    /// never indefinitely blocks a non-authored coordination wake.
    fn maybe_retrieve_openviking_context_for(
        &mut self,
        owner: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
            return true;
        }
        if !self.observe_openviking_query(owner, query, interactive) {
            return true;
        }
        if self.openviking_retrieval_cache.contains_key(&owner) {
            return true;
        }
        if self.openviking_retrieval_in_flight.contains(&owner) {
            return false;
        }
        if self
            .openviking_retrieval_retry_after
            .get(&owner)
            .is_some_and(|next| Instant::now() < *next)
        {
            return true;
        }
        let Some(sender) = &self.persist_tx else {
            return true;
        };
        if sender
            .send(PersistCommand::RetrieveConfirmedOpenViking {
                conversation_id: owner,
                query: query.chars().take(4096).collect(),
            })
            .is_ok()
        {
            self.openviking_retrieval_in_flight.insert(owner);
            return false;
        }
        true
    }

    /// Shared foreground/worker path: only the destination's exact directed
    /// project grants are considered. A failed/offline read uses backoff and
    /// permits the wake without unverified memory.
    fn maybe_retrieve_project_openviking_context_for(
        &mut self,
        destination: LocalConversationId,
        query: &str,
        interactive: bool,
    ) -> bool {
        if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
            return true;
        }
        if !self.observe_openviking_query(destination, query, interactive) {
            return true;
        }
        if !self
            .openviking_project_cache
            .can_schedule(destination, Instant::now())
        {
            return !self.openviking_project_cache.is_in_flight(destination);
        }
        if !self
            .project_memory_grants
            .has_sources(&self.local_conversation_catalog, destination)
        {
            return true;
        }
        let Some(sender) = &self.persist_tx else {
            return true;
        };
        let Some(generation) = self
            .openviking_project_cache
            .begin(destination, Instant::now())
        else {
            return !self.openviking_project_cache.is_in_flight(destination);
        };
        if sender
            .send(PersistCommand::RetrieveConfirmedProjectOpenViking {
                conversation_id: destination,
                generation,
                catalog: self.local_conversation_catalog.clone(),
                grants: self.project_memory_grants.clone(),
                query: query.chars().take(4096).collect(),
            })
            .is_ok()
        {
            return false;
        }
        self.openviking_project_cache
            .cancel(destination, generation);
        true
    }

    /// Foreground prefetch is also used by inactive workers, but each worker
    /// retains its own conversation identity, journal and directed grants.
    fn maybe_retrieve_openviking_context(&mut self) {
        let owner = self.local_conversation_id;
        // Querying the verified local history is done only in the background
        // worker, not on the UI thread. If an authored draft exists, use it;
        // otherwise use the last durable user message for this conversation.
        // Query-dependent caches invalidate when the normalized topic changes.
        let interactive = !self.draft.trim().is_empty();
        let query = if interactive {
            self.draft.clone()
        } else {
            projected_local_display_messages(&self.events, owner)
                .into_iter()
                .rev()
                .find(|message| matches!(message.role, DisplayRole::User))
                .map_or_else(String::new, |message| message.text)
        };
        self.maybe_retrieve_openviking_context_for(owner, &query, interactive);
    }

    fn maybe_retrieve_project_openviking_context(&mut self) {
        let owner = self.local_conversation_id;
        let interactive = !self.draft.trim().is_empty();
        let query = if interactive {
            self.draft.clone()
        } else {
            projected_local_display_messages(&self.events, owner)
                .into_iter()
                .rev()
                .find(|message| matches!(message.role, DisplayRole::User))
                .map_or_else(String::new, |message| message.text)
        };
        self.maybe_retrieve_project_openviking_context_for(owner, &query, interactive);
    }

    fn background_worker_memory_ready(
        &mut self,
        worker: LocalConversationId,
        directive: &str,
    ) -> bool {
        let own_ready = self.maybe_retrieve_openviking_context_for(worker, directive, false);
        let project_ready =
            self.maybe_retrieve_project_openviking_context_for(worker, directive, false);
        own_ready && project_ready
    }

    fn draft_state(&self) -> &'static str {
        if self.persist_tx.is_none() {
            "NOT DURABLE"
        } else if self.saved_revision >= self.draft_revision {
            "durable"
        } else {
            "saving…"
        }
    }

    /// Flush ready summaries even without model sign-in. The persistence
    /// worker alone claims each write. Uncertain writes are never replayed.
    fn maybe_dispatch_ready_openviking_write(&mut self) {
        if self.persist_tx.is_none()
            || std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0")
            || self
                .openviking_write_last_attempt
                .is_some_and(|when| when.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.openviking_write_last_attempt = Some(Instant::now());
        let path = self
            .journal_path
            .with_file_name(openviking_checkpoint_outbox::FILE_NAME);
        let outbox = match openviking_checkpoint_outbox::Outbox::load(&path) {
            Ok(outbox) => outbox,
            Err(error) => {
                self.openviking_checkpoint_status = format!("Queue unavailable: {error}");
                return;
            }
        };
        self.openviking_checkpoint_status =
            if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
                "Automatic checkpoints disabled".to_owned()
            } else {
                outbox
                    .entries()
                    .iter()
                    .filter(|entry| entry.conversation_id == self.local_conversation_id.to_string())
                    .max_by_key(|entry| entry.completed_sequence)
                    .map(|entry| match entry.state {
                        openviking_checkpoint_outbox::State::PendingSummary => {
                            "Summary pending; waiting for idle model inference"
                        }
                        openviking_checkpoint_outbox::State::Ready => {
                            "Summary saved; awaiting safe write preflight"
                        }
                        openviking_checkpoint_outbox::State::Dispatched => {
                            "Write dispatched; exact verification pending"
                        }
                        openviking_checkpoint_outbox::State::Confirmed => {
                            "Checkpoint confirmed by exact read-back"
                        }
                        openviking_checkpoint_outbox::State::Uncertain => {
                            "Write uncertain; no automatic retry"
                        }
                        openviking_checkpoint_outbox::State::Skipped => {
                            "Checkpoint skipped; no durable memory"
                        }
                    })
                    .unwrap_or("No checkpoint yet; waiting for an eligible completed turn")
                    .to_owned()
            };
        if outbox.ready_entries().next().is_some() {
            if let Some(sender) = &self.persist_tx {
                let _ = sender.send(PersistCommand::DispatchOpenVikingReadyWrite);
            }
        }
    }

    /// One bounded, low-priority model summarization at a time, never during
    /// an active human/worker turn. The original transcript remains untouched.
    fn maybe_generate_openviking_summary(&mut self) {
        if self.persist_tx.is_none()
            || std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0")
            || self.openviking_summary_in_flight.is_some()
            || !self.remote_connected()
            || self.local_conversation_busy()
            || self.capability_probe.running()
            || self.sign_in_pending
            || self.selected_remote_catalog_id.is_some()
            || self.selected_historical_conversation.is_some()
            || self
                .openviking_summary_last_attempt
                .is_some_and(|when| when.elapsed() < Duration::from_secs(90))
        {
            return;
        }
        self.openviking_summary_last_attempt = Some(Instant::now());
        let path = self
            .journal_path
            .with_file_name(openviking_checkpoint_outbox::FILE_NAME);
        let outbox = match openviking_checkpoint_outbox::Outbox::load(&path) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("OpenViking memory queue cannot be read: {error}");
                return;
            }
        };
        for entry in outbox.pending_summaries() {
            let key = format!(
                "openviking-summary:{}:{}",
                entry.conversation_id, entry.turn_id
            );
            if self.openviking_summary_blocked.contains(&key) {
                continue;
            }
            let Ok(conversation_id) = entry.conversation_id.parse::<LocalConversationId>() else {
                continue;
            };
            let Ok(turn_id) = entry.turn_id.parse::<LocalTurnId>() else {
                continue;
            };
            if self
                .local_conversation_catalog
                .entry(conversation_id)
                .is_none()
            {
                continue;
            }
            let model = if conversation_id == self.local_conversation_id {
                self.selected_model.clone()
            } else {
                self.inference_settings
                    .for_conversation(conversation_id)
                    .model
            };
            let Some(model) = model else {
                continue;
            };
            if !self
                .remote_models
                .iter()
                .any(|available| available.slug == model)
            {
                continue;
            }
            let messages = projected_local_display_messages(&self.events, conversation_id);
            let Some(source) =
                openviking_memory_summary::bounded_excerpt(&messages, entry.completed_sequence)
            else {
                continue;
            };
            // Keep the rolling memory chain ordered: do not summarize a
            // later turn while an older checkpoint is still being generated
            // or awaiting a definitive external write for this conversation.
            if outbox.entries().iter().any(|older| {
                older.conversation_id == entry.conversation_id
                    && older.completed_sequence < entry.completed_sequence
                    && matches!(
                        older.state,
                        openviking_checkpoint_outbox::State::PendingSummary
                            | openviking_checkpoint_outbox::State::Ready
                            | openviking_checkpoint_outbox::State::Dispatched
                    )
                    && !self.openviking_summary_blocked.contains(&format!(
                        "openviking-summary:{}:{}",
                        older.conversation_id, older.turn_id
                    ))
            }) {
                continue;
            }
            let prior = match openviking_memory_summary::newest_confirmed_prior(
                &outbox,
                &path,
                &entry.conversation_id,
                entry.completed_sequence,
            ) {
                Ok(prior) => prior,
                Err(error) => {
                    self.openviking_summary_blocked.insert(key);
                    self.status = format!("OpenViking prior summary rejected: {error}");
                    continue;
                }
            };
            let input = openviking_memory_summary::request_input_with_prior(
                &entry.conversation_id,
                &entry.turn_id,
                entry.completed_sequence,
                &source,
                prior.as_ref(),
            );
            let request = siwc_bridge::BridgeCommand::StreamResponse {
                request_id: key.clone(),
                model,
                input,
                instructions: Some(openviking_memory_summary::GUIDANCE.to_owned()),
                request_patch: serde_json::json!({}),
            };
            if let Err(error) = self.remote.send(request) {
                self.openviking_summary_blocked.insert(key);
                self.status = format!("OpenViking summary model request not queued: {error}");
                return;
            }
            self.openviking_summary_in_flight = Some(OpenVikingSummaryAttempt {
                request_id: key,
                conversation_id,
                turn_id,
                completed_sequence: entry.completed_sequence,
                had_confirmed_prior: prior.is_some(),
            });
            return;
        }
    }

    fn process_remote_notices(&mut self) {
        for notice in self.remote.drain() {
            match notice {
                siwc_bridge::BridgeEvent::Ready => {
                    self.remote_runtime_ready = true;
                    self.remote_runtime_failed = false;
                    if self.sign_in_requested {
                        self.sign_in_requested = false;
                        self.sign_in_pending = true;
                        self.remote_status = "opening ChatGPT sign-in…".to_owned();
                        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::SignIn) {
                            self.sign_in_pending = false;
                            self.remote_runtime_ready = false;
                            self.remote_runtime_failed = true;
                            self.remote_status = error;
                        }
                    } else {
                        self.remote_status = "sign-in runtime ready".to_owned();
                        if let Err(error) =
                            self.remote.send(siwc_bridge::BridgeCommand::RefreshSession)
                        {
                            self.remote_runtime_ready = false;
                            self.remote_runtime_failed = true;
                            self.remote_status = error;
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Session(session) => {
                    let was_connected = self.remote_connected();
                    let account_changed = session_identity_changed(&self.remote_session, &session);

                    self.sign_in_pending =
                        sign_in_pending_after_session(self.sign_in_pending, &session.status);
                    self.remote_session = session;

                    if self.remote_session.status == "connected" && self.remote_session.sharing {
                        if account_changed {
                            self.remote_models.clear();
                            self.model_list_pending = false;
                        }

                        if should_request_models(
                            was_connected,
                            account_changed,
                            self.remote_models.is_empty(),
                            self.model_list_pending,
                        ) {
                            self.request_chatgpt_models();
                        } else if !was_connected && !self.model_list_pending {
                            self.remote_status = "ChatGPT plan connected".to_owned();
                        }
                    } else if self.sign_in_pending {
                        // A stale disconnected session refresh cannot finish an
                        // already requested OAuth flow.
                        self.remote_status = "waiting for ChatGPT sign-in…".to_owned();
                    } else if let Some(error) = &self.remote_session.error_message {
                        self.remote_status = error.clone();
                    } else if self.remote_session.status == "connected" {
                        self.remote_status =
                            "ChatGPT connected, but token sharing is not enabled".to_owned();
                    } else if self.remote_session.status == "reauth_required" {
                        self.remote_status = "ChatGPT sign-in needs renewal".to_owned();
                    } else {
                        self.model_list_pending = false;
                        self.remote_models.clear();
                        self.remote_status = "not connected".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::Models(models) => {
                    self.model_list_pending = false;
                    let keep_selected = self
                        .selected_model
                        .as_ref()
                        .is_some_and(|selected| models.iter().any(|model| &model.slug == selected));
                    if !keep_selected {
                        self.selected_model = models.first().map(|model| model.slug.clone());
                        self.persist_current_inference_settings();
                    }
                    self.remote_models = models;
                    self.remote_status = if self.remote_models.is_empty() {
                        "connected; no models reported".to_owned()
                    } else {
                        format!("connected · {} models", self.remote_models.len())
                    };
                }
                siwc_bridge::BridgeEvent::AgentActivity { request_id, frame } => {
                    // Attribute only an active turn; never accept a sidecar's
                    // claimed conversation identity or stale request frame.
                    if let Some(active) = self
                        .active_remote_turn
                        .as_ref()
                        .filter(|active| active.request_id == request_id)
                    {
                        if let Some(sender) = &self.persist_tx {
                            if let Err(error) = sender.send(PersistCommand::AppendAgentActivity {
                                conversation_id: active.conversation_id,
                                turn_id: active.turn_id,
                                frame,
                            }) {
                                self.status = format!("failed to queue agent activity: {error}");
                            }
                        } else {
                            self.status = "agent activity journal unavailable".to_owned();
                        }
                    }
                }
                siwc_bridge::BridgeEvent::TransportProgress { request_id, stage } => {
                    if self
                        .active_remote_turn
                        .as_ref()
                        .is_some_and(|active| active.request_id == request_id)
                    {
                        match stage.as_str() {
                            "wire_attempted" => self
                                .delivery
                                .set(&request_id, delivery_telemetry::Phase::WireAttempted),
                            "acknowledged" => self
                                .delivery
                                .set(&request_id, delivery_telemetry::Phase::Acknowledged),
                            _ => {}
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Delta { request_id, delta } => {
                    if capability_probes::probe_name_from_request_id(&request_id).is_some() {
                        continue;
                    }
                    let Some(active) = self
                        .active_remote_turn
                        .as_mut()
                        .filter(|active| active.request_id == request_id)
                    else {
                        continue;
                    };

                    let first_output = !active.observed_output;
                    active.observed_output = true;
                    active.cumulative_text.push_str(&delta);
                    if !active.source_suspect_logged {
                        if let Some(marker) = response_text_provenance::suspect_in_stream_tail(
                            &active.cumulative_text,
                        ) {
                            diagnostics::info(
                                "siwc-text-provenance",
                                format!(
                                    "bridge delta request={} suspect={} source=stream; assistant text retained verbatim",
                                    diagnostics::short_id(&request_id),
                                    marker.label(),
                                ),
                            );
                            active.source_suspect_logged = true;
                        }
                    }
                    let turn_id = active.turn_id;
                    let cumulative_text = active.cumulative_text.clone();

                    if first_output {
                        self.delivery
                            .set(&request_id, delivery_telemetry::Phase::Streaming);
                        let acceptance = remote_turn_payload(
                            turn_id,
                            &request_id,
                            None,
                            None,
                            Some("Responses stream produced assistant output"),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::RemoteAcceptanceObserved,
                            acceptance,
                        );
                        let started = remote_turn_payload(
                            turn_id,
                            &request_id,
                            None,
                            None,
                            Some("assistant output stream started"),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::AssistantStreamStarted,
                            started,
                        );
                    }

                    let snapshot = remote_turn_payload(
                        turn_id,
                        &request_id,
                        None,
                        Some(&cumulative_text),
                        Some("cumulative Responses stream text"),
                    );
                    let _ = self.queue_turn_event(
                        turn_id,
                        EventKind::AssistantSnapshotObserved,
                        snapshot,
                    );
                }
                siwc_bridge::BridgeEvent::ResponseCompleted {
                    request_id,
                    text,
                    web_search_executed,
                } => {
                    if let Some(attempt) = self
                        .openviking_summary_in_flight
                        .clone()
                        .filter(|pending| pending.request_id == request_id)
                    {
                        match openviking_memory_summary::validate_rolling_summary(
                            &text,
                            attempt.had_confirmed_prior,
                        ) {
                            Ok(summary) => {
                                let result = self
                                    .persist_tx
                                    .as_ref()
                                    .ok_or_else(|| "local persistence unavailable".to_owned())
                                    .and_then(|sender| {
                                        sender
                                            .send(PersistCommand::SaveOpenVikingMemorySummary {
                                                conversation_id: attempt.conversation_id,
                                                turn_id: attempt.turn_id,
                                                completed_sequence: attempt.completed_sequence,
                                                summary,
                                            })
                                            .map_err(|error| error.to_string())
                                    });
                                if let Err(error) = result {
                                    self.openviking_summary_in_flight = None;
                                    self.openviking_summary_blocked.insert(request_id);
                                    self.status =
                                        format!("OpenViking summary not persisted: {error}");
                                }
                            }
                            Err(error) => {
                                self.openviking_summary_in_flight = None;
                                self.openviking_summary_blocked.insert(request_id);
                                self.status = format!("OpenViking summary rejected: {error}");
                            }
                        }
                        continue;
                    }
                    if let Some(probe_name) =
                        capability_probes::probe_name_from_request_id(&request_id)
                    {
                        if probe_name == "web_search" {
                            self.capability_probe
                                .complete_web_search(!text.is_empty(), web_search_executed);
                        } else {
                            self.capability_probe
                                .complete_supported(probe_name, !text.is_empty());
                        }
                        self.dispatch_next_capability_probe();
                        continue;
                    }
                    if !self
                        .active_remote_turn
                        .as_ref()
                        .is_some_and(|active| active.request_id == request_id)
                    {
                        continue;
                    }
                    let active = self
                        .active_remote_turn
                        .take()
                        .expect("matched active remote turn");
                    let relation = response_text_provenance::final_source_relation(
                        &active.cumulative_text,
                        &text,
                    );
                    if relation
                        == response_text_provenance::FinalSourceRelation::FinalDiffersFromStream
                    {
                        diagnostics::info(
                            "siwc-text-provenance",
                            format!(
                                "bridge completion request={} final_differs_from_stream=true; content kept unmodified",
                                diagnostics::short_id(&request_id),
                            ),
                        );
                    }
                    if !active.source_suspect_logged {
                        if let Some(marker) =
                            response_text_provenance::suspect_in_stream_tail(&text)
                        {
                            diagnostics::info(
                                "siwc-text-provenance",
                                format!(
                                    "bridge completion request={} suspect={} source=final; assistant text retained verbatim",
                                    diagnostics::short_id(&request_id),
                                    marker.label(),
                                ),
                            );
                        }
                    }
                    self.delivery
                        .set(&request_id, delivery_telemetry::Phase::Complete);
                    let final_text = if text.is_empty() {
                        active.cumulative_text.clone()
                    } else {
                        text
                    };

                    if !active.observed_output {
                        let acceptance = remote_turn_payload(
                            active.turn_id,
                            &request_id,
                            None,
                            None,
                            Some("Responses stream completed"),
                        );
                        let _ = self.queue_turn_event(
                            active.turn_id,
                            EventKind::RemoteAcceptanceObserved,
                            acceptance,
                        );
                        if !final_text.is_empty() {
                            let started = remote_turn_payload(
                                active.turn_id,
                                &request_id,
                                None,
                                None,
                                Some("completed response contained assistant output"),
                            );
                            let _ = self.queue_turn_event(
                                active.turn_id,
                                EventKind::AssistantStreamStarted,
                                started,
                            );
                        }
                    }

                    let completed = remote_turn_payload(
                        active.turn_id,
                        &request_id,
                        None,
                        Some(&final_text),
                        Some("response.completed observed"),
                    );
                    let _ = self.queue_turn_event(
                        active.turn_id,
                        EventKind::AssistantCompletionObserved,
                        completed,
                    );

                    let foreground = active.conversation_id == self.local_conversation_id;
                    let opt_in = if foreground {
                        self.conversation_mcp_enabled
                    } else {
                        self.inference_settings
                            .for_conversation_with_mcp_defaults(
                                active.conversation_id,
                                &self.mcp_agent_defaults,
                            )
                            .mcp_enabled
                    };
                    match mcp_agent_defaults::parse_model_tool_request(&final_text) {
                        Ok(Some(proposal)) if opt_in || self.agent_mcp_flow.is_some() => {
                            if proposal.provider == "chatarium.builtin"
                                && matches!(
                                    proposal.tool.as_str(),
                                    "coordination.identity" | "coordination.send"
                                )
                            {
                                self.execute_coordination_proposal(&active, &final_text, &proposal);
                            } else if foreground {
                                self.propose_agent_mcp_call(&active, &final_text, proposal);
                            } else {
                                self.status =
                                    "background wake tool denied: only exact coordination operations are admitted"
                                        .to_owned();
                            }
                        }
                        Ok(Some(_)) => {
                            self.status = "Assistant produced an MCP proposal, but tool execution is not enabled for this conversation. No tool was dispatched.".to_owned();
                        }
                        Ok(None) => {
                            // The completion journal command precedes this
                            // checkpoint intent on the same persistence queue.
                            // No user reminder or model-authored MCP call needed.
                            // Do not queue a checkpoint from intermediate tool
                            // proposals or unknown remote outcomes.
                            if let Some(sender) = &self.persist_tx {
                                let _ = sender.send(PersistCommand::QueueMemoryCheckpoint {
                                    conversation_id: active.conversation_id,
                                    turn_id: active.turn_id,
                                });
                            }
                            self.agent_mcp_flow = None;
                        }
                        Err(error) => {
                            self.agent_mcp_flow = None;
                            self.status = format!("Malformed agent MCP proposal rejected: {error}");
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Failed { request_id, error } => {
                    if let Some(pending) = self.openviking_summary_in_flight.as_ref() {
                        if request_id.as_deref() == Some(pending.request_id.as_str()) {
                            let key = pending.request_id.clone();
                            self.openviking_summary_in_flight = None;
                            self.openviking_summary_blocked.insert(key);
                            self.status =
                                format!("OpenViking summary generation failed: {}", error.message);
                            continue;
                        }
                    }
                    if let Some(probe_name) = request_id
                        .as_deref()
                        .and_then(capability_probes::probe_name_from_request_id)
                    {
                        let probe_name = probe_name.to_owned();
                        self.capability_probe.complete_failed(&probe_name, &error);
                        self.dispatch_next_capability_probe();
                        continue;
                    }
                    // Responses, model discovery, and cancellation failures may
                    // arrive while OAuth is still running. Only the sign-in
                    // request itself may clear its pending UI state.
                    if failed_bridge_command_finishes_sign_in(request_id.as_deref()) {
                        self.sign_in_pending = false;
                    }
                    if request_id.as_deref() == Some("models") {
                        self.model_list_pending = false;
                    }
                    let mut handled_turn = false;
                    if let Some(request_id) = request_id.as_deref() {
                        if self
                            .active_remote_turn
                            .as_ref()
                            .is_some_and(|active| active.request_id == request_id)
                        {
                            let mut active = self
                                .active_remote_turn
                                .take()
                                .expect("matched active remote turn");
                            let detail = format!("{}: {}", error.code, error.message);
                            // A definite HTTP 429 rejection before any output is safe to
                            // retry. A timeout, EOF or failed connection has an unknown
                            // outcome and must NEVER be blindly resubmitted.
                            if error.status == Some(429)
                                && !active.observed_output
                                && active.retries < delivery_telemetry::MAX_RATE_LIMIT_RETRIES
                            {
                                let backoff = Duration::from_secs(1_u64 << (active.retries + 1));
                                active.retries += 1;
                                active.retry_at = Some(Instant::now() + backoff);
                                self.delivery.retry(&active.request_id, backoff, detail);
                                self.remote_status = format!(
                                    "HTTP 429 · retry {} of {} in {}s",
                                    active.retries,
                                    delivery_telemetry::MAX_RATE_LIMIT_RETRIES,
                                    backoff.as_secs(),
                                );
                                self.active_remote_turn = Some(active);
                                continue;
                            }
                            self.delivery.error(
                                &active.request_id,
                                if remote_error_is_observed_failure(&error) {
                                    delivery_telemetry::Phase::Failed
                                } else {
                                    delivery_telemetry::Phase::Unconfirmed
                                },
                                detail.clone(),
                            );
                            if remote_error_is_observed_failure(&error) {
                                let failure = remote_turn_payload(
                                    active.turn_id,
                                    request_id,
                                    None,
                                    (!active.cumulative_text.is_empty())
                                        .then_some(active.cumulative_text.as_str()),
                                    Some(&detail),
                                );
                                let _ = self.queue_turn_event(
                                    active.turn_id,
                                    EventKind::RemoteFailureObserved,
                                    failure,
                                );
                                if active.observed_output {
                                    let interrupted = remote_turn_payload(
                                        active.turn_id,
                                        request_id,
                                        None,
                                        Some(active.cumulative_text.as_str()),
                                        Some(
                                            "assistant output ended before completion after a definitive remote failure",
                                        ),
                                    );
                                    let _ = self.queue_turn_event(
                                        active.turn_id,
                                        EventKind::TransportInterrupted,
                                        interrupted,
                                    );
                                }
                            } else {
                                let interrupted = remote_turn_payload(
                                    active.turn_id,
                                    request_id,
                                    None,
                                    (!active.cumulative_text.is_empty())
                                        .then_some(active.cumulative_text.as_str()),
                                    Some(&detail),
                                );
                                let _ = self.queue_turn_event(
                                    active.turn_id,
                                    EventKind::TransportInterrupted,
                                    interrupted,
                                );
                            }
                            handled_turn = true;
                        }
                    }
                    self.remote_status = format!("{}: {}", error.code, error.message);
                    if handled_turn {
                        self.agent_mcp_flow = None;
                        self.status = "remote turn ended without automatic retry".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::RuntimeUnavailable(detail) => {
                    if let Some(attempt) = self.openviking_summary_in_flight.take() {
                        self.openviking_summary_blocked.insert(attempt.request_id);
                    }
                    if self.capability_probe.running() {
                        self.capability_probe.abort(&detail);
                        self.finish_capability_probes();
                    }
                    self.remote_runtime_ready = false;
                    self.remote_runtime_failed = true;
                    self.sign_in_requested = false;
                    self.sign_in_pending = false;
                    if let Some(active) = self.active_remote_turn.take() {
                        self.delivery.error(
                            &active.request_id,
                            delivery_telemetry::Phase::Unconfirmed,
                            detail.clone(),
                        );
                        let payload = remote_turn_payload(
                            active.turn_id,
                            &active.request_id,
                            None,
                            (!active.cumulative_text.is_empty())
                                .then_some(active.cumulative_text.as_str()),
                            Some("Sign in with ChatGPT runtime became unavailable"),
                        );
                        let _ = self.queue_turn_event(
                            active.turn_id,
                            EventKind::TransportInterrupted,
                            payload,
                        );
                    }
                    // DispatchAttempted is journaled before the sidecar send.
                    // If its acknowledgment is still pending at bridge death,
                    // clear the intent before that notice can later dispatch
                    // against a dead runtime. Never retry the ambiguous turn.
                    if let Some((turn_id, payload)) =
                        interrupted_pending_remote_turn(&mut self.pending_remote_turn)
                    {
                        self.delivery.error(
                            &turn_id.to_string(),
                            delivery_telemetry::Phase::Unconfirmed,
                            detail.clone(),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::TransportInterrupted,
                            payload,
                        );
                    }
                    self.remote_status = detail;
                }
                siwc_bridge::BridgeEvent::CommandSucceeded { .. } => {}
            }
        }
    }

    fn process_due_delivery_retry(&mut self) {
        let Some((request_id, command)) = self.active_remote_turn.as_ref().and_then(|active| {
            active
                .retry_at
                .filter(|at| Instant::now() >= *at)
                .map(|_| (active.request_id.clone(), active.command.clone()))
        }) else {
            return;
        };
        if let Some(active) = self.active_remote_turn.as_mut() {
            active.retry_at = None;
            active.cumulative_text.clear();
            active.observed_output = false;
        }
        match self.remote.send(command) {
            Ok(()) => {
                self.delivery
                    .set(&request_id, delivery_telemetry::Phase::BridgeQueued);
                self.remote_status =
                    "retry submitted to the local bridge · awaiting network".to_owned();
            }
            Err(error) => {
                self.delivery.error(
                    &request_id,
                    delivery_telemetry::Phase::Unconfirmed,
                    error.clone(),
                );
                self.remote_status = error;
                self.active_remote_turn = None;
            }
        }
    }

    fn start_chatgpt_sign_in(&mut self, repaint: &egui::Context) {
        if self.remote_runtime_failed {
            self.remote = siwc_bridge::BridgeRuntime::start(repaint);
            self.remote_runtime_ready = false;
            self.remote_runtime_failed = false;
            self.sign_in_requested = true;
            self.sign_in_pending = false;
            self.remote_status = "restarting ChatGPT sign-in runtime…".to_owned();
            return;
        }

        if !self.remote_runtime_ready {
            self.sign_in_requested = true;
            self.remote_status = "preparing ChatGPT sign-in runtime…".to_owned();
            return;
        }

        self.sign_in_pending = true;
        self.remote_status = "opening ChatGPT sign-in…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::SignIn) {
            self.sign_in_pending = false;
            self.remote_runtime_ready = false;
            self.remote_runtime_failed = true;
            self.remote_status = error;
        }
    }

    fn cancel_chatgpt_sign_in(&mut self) {
        // The runtime can take time to bootstrap. Cancel a sign-in that has
        // not been dispatched yet instead of leaving an armed request that
        // will unexpectedly open the browser once the sidecar is ready.
        if self.sign_in_requested {
            self.sign_in_requested = false;
            self.remote_status = "ChatGPT sign-in cancelled before launch".to_owned();
            return;
        }
        self.remote_status = "cancelling ChatGPT sign-in…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::CancelSignIn) {
            self.remote_status = error;
        }
    }

    fn disconnect_chatgpt(&mut self) {
        self.remote_status = "disconnecting ChatGPT…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::Disconnect) {
            self.remote_status = error;
        }
    }

    fn request_chatgpt_models(&mut self) {
        match self.remote.send(siwc_bridge::BridgeCommand::ListModels {
            max_attempts: self.inference_settings.read_retry_attempts(),
        }) {
            Ok(()) => {
                self.model_list_pending = true;
                self.remote_status = "loading ChatGPT models…".to_owned();
            }
            Err(error) => {
                self.model_list_pending = false;
                self.remote_status = error;
            }
        }
    }

    fn start_capability_probes(&mut self) {
        if !self.remote_connected() {
            self.capability_probe.status =
                "capability probes require a connected ChatGPT plan".to_owned();
            return;
        }
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.capability_probe.status =
                "wait for the active ChatGPT response before probing capabilities".to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.capability_probe.status = "choose a ChatGPT model before probing".to_owned();
            return;
        };
        if self.capability_probe.running() {
            return;
        }

        self.capability_probe
            .start(model, self.remote_session.profile_id.clone());
        self.dispatch_next_capability_probe();
    }

    fn start_web_search_probe(&mut self) {
        if !self.remote_connected()
            || self.capability_probe.running()
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
        {
            self.capability_probe.status =
                "cannot verify web search while disconnected, busy, or already probing".to_owned();
            return;
        }
        let (Some(model), Some(profile_id)) = (
            self.selected_model.clone(),
            self.remote_session.profile_id.clone(),
        ) else {
            self.capability_probe.status =
                "web search verification needs an active model and profile identity".to_owned();
            return;
        };
        self.capability_probe
            .start_web_search(model, Some(profile_id));
        self.dispatch_next_capability_probe();
    }

    fn dispatch_next_capability_probe(&mut self) {
        if self.capability_probe.active.is_some() {
            return;
        }
        let Some(spec) = self.capability_probe.next() else {
            self.finish_capability_probes();
            return;
        };
        let probe_name = spec.name.to_owned();
        let command = siwc_bridge::BridgeCommand::ProbeResponse {
            request_id: capability_probes::request_id(spec.name),
            model: self.capability_probe.model.clone().unwrap_or_default(),
            input: spec.input,
            request_patch: spec.request_patch,
        };
        match self.remote.send(command) {
            Ok(()) => self.capability_probe.mark_active(&probe_name),
            Err(error) => {
                self.capability_probe
                    .complete_dispatch_error(&probe_name, error);
                self.dispatch_next_capability_probe();
            }
        }
    }

    fn finish_capability_probes(&mut self) {
        if self.capability_probe.running() || self.capability_probe.results.is_empty() {
            return;
        }
        let data_dir = self.journal_path.parent().unwrap_or_else(|| Path::new("."));
        let report_path = data_dir.join("siwc-capability-probes.json");
        let contract_path = data_dir.join("local-inference-contract.json");
        let generated_unix_ms = unix_now_ms();
        let status = match capability_probes::save_report(
            &report_path,
            &self.capability_probe,
            generated_unix_ms,
        ) {
            Ok(()) => {
                self.capability_probe.generated_unix_ms = Some(generated_unix_ms);
                match local_inference_contract::save_contract(
                    &contract_path,
                    &self.capability_probe,
                    unix_now_ms(),
                ) {
                    Ok(()) => match local_inference_contract::load_contract(&contract_path) {
                        Ok(contract @ Some(_)) => {
                            self.local_inference_contract = contract;
                            format!(
                                "capability probes complete · saved {} · contract {}",
                                report_path.display(),
                                local_inference_contract::contract_state(&self.capability_probe)
                            )
                        }
                        Ok(None) => {
                            self.local_inference_contract = None;
                            "capability probes complete · report saved · contract reload missing"
                                .to_owned()
                        }
                        Err(error) => {
                            self.local_inference_contract = None;
                            format!(
                                "capability probes complete · report saved · contract reload error: {error}"
                            )
                        }
                    },
                    Err(error) => {
                        self.local_inference_contract = None;
                        format!(
                            "capability probes complete · report saved · contract error: {error}"
                        )
                    }
                }
            }
            Err(error) => format!("capability probes complete · {error}"),
        };
        self.capability_probe.status = status;
        if let Some((conversation, model, profile)) = self.pending_web_search_enable.take() {
            if self.local_conversation_id == conversation
                && self.selected_model.as_deref() == Some(model.as_str())
                && self.remote_session.profile_id.as_deref() == Some(profile.as_str())
                && self
                    .current_capability_gate()
                    .allows(context_composer::CapabilitySlot::WebSearch)
            {
                self.conversation_behavior_profile.web_search = true;
                self.persist_current_behavior_profile();
                self.capability_probe
                    .status
                    .push_str(" · web search enabled for this conversation");
            } else {
                self.capability_probe.status.push_str(
                    " · web search remains off: probe evidence did not authorize the current conversation/model/profile",
                );
            }
        }
    }

    fn remote_connected(&self) -> bool {
        self.remote_session.status == "connected" && self.remote_session.sharing
    }

    fn remote_identity_label(&self) -> String {
        self.remote_session
            .email
            .clone()
            .or_else(|| self.remote_session.profile_label.clone())
            .unwrap_or_else(|| "ChatGPT account".to_owned())
    }

    fn persist_current_inference_settings(&mut self) {
        let settings = local_inference_settings::ConversationInferenceSettings {
            model: self.selected_model.clone(),
            instructions: self.conversation_instructions.clone(),
            developer_context: self.conversation_developer_context.clone(),
            mcp_enabled: self.conversation_mcp_enabled,
            slow_warning_seconds: self.slow_warning_seconds,
        };
        if let Err(error) = self
            .inference_settings
            .set(self.local_conversation_id, settings)
        {
            self.status = format!("failed to update inference controls: {error}");
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist inference controls: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = sender.send(PersistCommand::SaveInferenceSettings {
            store: self.inference_settings.clone(),
        }) {
            self.status = format!("failed to queue inference controls: {error}");
        }
    }

    fn save_mcp_agent_defaults(&mut self) {
        if let Err(error) = self.mcp_agent_defaults.validate() {
            self.status = format!("MCP defaults not saved: {error}");
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.status = "MCP defaults not saved: persistence unavailable".to_owned();
            return;
        };
        match tx.send(PersistCommand::SaveMcpAgentDefaults {
            defaults: self.mcp_agent_defaults.clone(),
        }) {
            Ok(()) => {
                self.status =
                    "MCP bootstrap and exact tool policies queued for local save.".to_owned();
            }
            Err(error) => self.status = format!("MCP defaults could not be queued: {error}"),
        }
    }

    fn current_capability_gate(&self) -> context_composer::CapabilityGate {
        context_composer::CapabilityGate::from_contract(
            self.local_inference_contract.as_ref(),
            self.remote_session.profile_id.as_deref(),
            self.selected_model.as_deref(),
        )
    }

    fn current_behavior_request_patch(&self) -> Result<Value, String> {
        self.conversation_behavior_profile
            .request_patch(&self.current_capability_gate())
    }

    fn persist_current_behavior_profile(&mut self) {
        self.behavior_profiles.set(
            self.local_conversation_id,
            self.conversation_behavior_profile.clone(),
        );
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist behavior profile: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = sender.send(PersistCommand::SaveBehaviorProfiles {
            store: self.behavior_profiles.clone(),
        }) {
            self.status = format!("failed to queue behavior profile: {error}");
        }
    }

    fn initialize_local_orchestration_topology(&mut self) {
        if self.topology_command_pending {
            return;
        }
        match local_conversation_topology(&self.events, self.local_conversation_id) {
            Ok(Some(topology)) => {
                self.status = format!(
                    "local orchestration topology already exists · container {} · session {}",
                    topology.container_id.get(),
                    topology.current_session_id.get()
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                self.status = format!("cannot project local orchestration topology: {error}");
                return;
            }
        }

        let (container_id, root_session_id) =
            match next_available_local_orchestration_ids(&self.events) {
                Ok(ids) => ids,
                Err(error) => {
                    self.status =
                        format!("cannot allocate local orchestration identities: {error}");
                    return;
                }
            };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot initialize local orchestration topology: persistence unavailable"
                .to_owned();
            return;
        };

        match sender.send(PersistCommand::InitializeLocalOrchestrationTopology {
            conversation_id: self.local_conversation_id,
            container_id,
            root_session_id,
        }) {
            Ok(()) => {
                self.topology_command_pending = true;
                self.status = format!(
                    "initializing local orchestration topology · container {} · root session {}…",
                    container_id.get(),
                    root_session_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local orchestration topology: {error}");
            }
        }
    }

    fn bind_current_session_route_endpoint(&mut self, session_id: SessionId) {
        if self.route_addressability_command_pending {
            return;
        }
        let endpoint_id = match next_available_route_endpoint_id(&self.events) {
            Ok(endpoint_id) => endpoint_id,
            Err(error) => {
                self.status = format!("cannot allocate routing endpoint identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind routing endpoint: persistence unavailable".to_owned();
            return;
        };

        match sender.send(PersistCommand::BindCurrentSessionRouteEndpoint {
            conversation_id: self.local_conversation_id,
            session_id,
            endpoint_id,
        }) {
            Ok(()) => {
                self.route_addressability_command_pending = true;
                self.status = format!(
                    "binding current session {} to routing endpoint {}…",
                    session_id.get(),
                    endpoint_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue routing endpoint binding: {error}");
            }
        }
    }

    fn propose_local_session_route(&mut self, destination_conversation_id: LocalConversationId) {
        if self.route_policy_command_pending {
            return;
        }
        let route_id = match next_available_route_id(&self.events) {
            Ok(route_id) => route_id,
            Err(error) => {
                self.status = format!("cannot allocate local route identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose local route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeLocalSessionRoute {
            route_id,
            source_conversation_id: self.local_conversation_id,
            destination_conversation_id,
        }) {
            Ok(()) => {
                self.route_policy_command_pending = true;
                self.status = format!(
                    "proposing local route {} for explicit approval…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route proposal: {error}");
            }
        }
    }

    fn attach_local_session_route_payload(&mut self, route_id: RouteId) {
        if self.route_payload_command_pending {
            return;
        }
        let text = self
            .route_payload_drafts
            .get(&route_id)
            .cloned()
            .unwrap_or_default();
        if text.trim().is_empty() {
            self.status = "route payload text cannot be empty".to_owned();
            return;
        }
        let payload_id = match next_available_route_payload_id(&self.events) {
            Ok(payload_id) => payload_id,
            Err(error) => {
                self.status = format!("cannot allocate route payload identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot attach route payload: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AttachLocalSessionRoutePayload {
            payload_id,
            route_id,
            text,
        }) {
            Ok(()) => {
                self.route_payload_command_pending = true;
                self.status = format!(
                    "attaching immutable payload {} to local route {}…",
                    payload_id.get(),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route payload: {error}");
            }
        }
    }

    fn decide_local_session_route(&mut self, route_id: RouteId, decision: RouteUserDecision) {
        if self.route_policy_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot record local route decision: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalSessionRoute { route_id, decision }) {
            Ok(()) => {
                self.route_policy_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for local route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route decision: {error}");
            }
        }
    }

    fn dispatch_local_session_route(&mut self, route_id: RouteId) {
        if self.route_dispatch_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot dispatch local route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchLocalSessionRoute { route_id }) {
            Ok(()) => {
                self.route_dispatch_command_pending = true;
                self.status = format!(
                    "consuming local route {} dispatch authority and recording delivery…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local route dispatch: {error}");
            }
        }
    }

    fn decide_local_route_context(
        &mut self,
        route_id: RouteId,
        decision: LocalRouteContextDecision,
    ) {
        if self.route_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update routed context eligibility: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalRouteContext {
            route_id,
            destination_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.route_context_command_pending = true;
                self.status = format!(
                    "recording routed context {} decision for route {}…",
                    local_route_context_decision_label(decision),
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue routed context decision: {error}");
            }
        }
    }

    fn register_tool_provider(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let name = match ToolProviderName::new(self.tool_provider_name_draft.clone()) {
            Ok(name) => name,
            Err(error) => {
                self.status = format!("cannot register tool provider: {error:?}");
                return;
            }
        };
        let provider_id = match next_available_tool_provider_id(&self.events) {
            Ok(provider_id) => provider_id,
            Err(error) => {
                self.status = format!("cannot allocate tool provider identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot register tool provider: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RegisterToolProvider { provider_id, name }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!("registering inert tool provider {}…", provider_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue tool provider registration: {error}");
            }
        }
    }

    fn bind_tool_provider_endpoint(&mut self, provider_id: ToolProviderId) {
        if self.tool_command_pending {
            return;
        }
        let endpoint_id = match next_available_route_endpoint_id(&self.events) {
            Ok(endpoint_id) => endpoint_id,
            Err(error) => {
                self.status = format!("cannot allocate tool routing endpoint: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind tool provider endpoint: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindToolProviderEndpoint {
            provider_id,
            endpoint_id,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "binding tool provider {} to routing endpoint {}…",
                    provider_id.get(),
                    endpoint_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool endpoint binding: {error}");
            }
        }
    }

    fn configure_stdio_tool_provider(&mut self, provider_id: ToolProviderId) {
        if self.tool_command_pending {
            return;
        }
        let config = match local_tool_provider_control::parse_stdio_config_draft(
            &self.tool_stdio_executable_draft,
            &self.tool_stdio_argv_draft,
            &self.tool_stdio_operations_draft,
        ) {
            Ok(config) => config,
            Err(error) => {
                self.status = format!("cannot configure tool provider: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot configure provider: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ConfigureToolProviderTransport {
            provider_id,
            config,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording inert stdio configuration for provider {}…",
                    provider_id.get()
                );
            }
            Err(error) => self.status = format!("failed to queue tool configuration: {error}"),
        }
    }

    fn decide_stdio_provider_activation(
        &mut self,
        provider_id: ToolProviderId,
        configured_sequence: u64,
        decision: ProviderActivationDecision,
    ) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot change activation: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolProviderActivation {
            provider_id,
            configured_sequence,
            decision,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording explicit {:?} for provider {}…",
                    decision,
                    provider_id.get()
                );
            }
            Err(error) => self.status = format!("failed to queue tool activation: {error}"),
        }
    }

    fn propose_tool_call(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let Some(provider_id) = self.tool_selected_provider else {
            self.status = "cannot propose tool call: select a provider".to_owned();
            return;
        };
        let operation = match ToolOperationName::new(self.tool_operation_draft.clone()) {
            Ok(operation) => operation,
            Err(error) => {
                self.status = format!("cannot propose tool call: invalid operation: {error:?}");
                return;
            }
        };
        let call_id = match next_available_tool_call_id(&self.events) {
            Ok(call_id) => call_id,
            Err(error) => {
                self.status = format!("cannot allocate tool call identity: {error}");
                return;
            }
        };
        let route_id = match next_available_route_id(&self.events) {
            Ok(route_id) => route_id,
            Err(error) => {
                self.status = format!("cannot allocate tool route identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose tool call: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeToolCall {
            call_id,
            route_id,
            source_conversation_id: self.local_conversation_id,
            provider_id,
            operation,
            arguments_text: self.tool_arguments_draft.clone(),
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording immutable tool call {} and pending route {}…",
                    call_id.get(),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool call proposal: {error}");
            }
        }
    }

    /// Exact model output becomes a durable proposal, never a process launch.
    fn propose_agent_mcp_call(
        &mut self,
        active: &ActiveRemoteTurn,
        final_text: &str,
        mut proposal: mcp_agent_defaults::ModelToolRequest,
    ) {
        // Inject the user's selected per-chat cwd into the audited call only
        // when the model has not explicitly supplied its own cwd.
        if proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME
            && proposal.tool == host_shell_mcp::OPERATION
        {
            if let Some(directory) = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.project_directory.as_deref())
            {
                if let Some(arguments) = proposal.arguments.as_object_mut() {
                    arguments
                        .entry("cwd")
                        .or_insert_with(|| Value::String(directory.to_owned()));
                }
            }
        }
        if self.tool_command_pending {
            self.agent_mcp_flow = None;
            self.status = "MCP call stopped: another tool action is pending".to_owned();
            return;
        }
        let count = self
            .agent_mcp_flow
            .as_ref()
            .map(|flow| flow.call_count)
            .unwrap_or(0)
            .saturating_add(1);
        if count > self.mcp_agent_defaults.max_calls {
            self.agent_mcp_flow = None;
            self.status = format!(
                "MCP agent stopped at configured {}-call limit",
                self.mcp_agent_defaults.max_calls
            );
            return;
        }
        let policy = match mcp_agent_defaults::tool_policy(
            &self.mcp_agent_defaults.policy_rules,
            &proposal.provider,
            &proposal.tool,
        ) {
            Ok(policy) => policy,
            Err(error) => {
                self.agent_mcp_flow = None;
                self.status = format!("MCP policy invalid: {error}");
                return;
            }
        };
        if matches!(
            policy,
            mcp_agent_defaults::ToolPolicy::Disabled | mcp_agent_defaults::ToolPolicy::Manual
        ) {
            self.agent_mcp_flow = None;
            self.status = format!(
                "MCP call not authorized for model invocation: {}::{} is {}",
                proposal.provider,
                proposal.tool,
                policy.name(),
            );
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.agent_mcp_flow = None;
            self.status = "MCP call cannot proceed without durable journal".to_owned();
            return;
        };
        let turn_id = active.turn_id;
        let result = tx.send(PersistCommand::ProposeAgentMcpCall {
            turn_id,
            source_conversation_id: self.local_conversation_id,
            proposal,
            policy,
        });
        if let Err(error) = result {
            self.agent_mcp_flow = None;
            self.status = format!("MCP proposal couldn't enter durable queue: {error}");
            return;
        }
        self.agent_mcp_flow = Some(AgentMcpFlow {
            turn_id,
            conversation_id: self.local_conversation_id,
            command: active.command.clone(),
            proposal_text: final_text.to_owned(),
            call_count: count,
            pending_call: None,
            pending_route: None,
            policy,
        });
        self.tool_command_pending = true;
        self.status = "MCP tool proposal queued for durable policy validation".to_owned();
    }

    fn coordination_store_path(&self) -> PathBuf {
        self.journal_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(coordination_workspace::FILE_NAME)
    }

    /// Resolve the sender from the active, authenticated local conversation.
    /// The model can supply a destination but never a source or permission.
    fn execute_coordination_proposal(
        &mut self,
        active: &ActiveRemoteTurn,
        final_text: &str,
        proposal: &mcp_agent_defaults::ModelToolRequest,
    ) {
        if active.conversation_id != self.local_conversation_id
            && !self.is_authorized_background_wake_turn(active.conversation_id, active.turn_id)
        {
            self.status =
                "coordination call denied: no valid background wake provenance".to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        let limit = self.mcp_agent_defaults.max_calls;
        if !self.coordination_tool_budget.admit(active.turn_id, limit) {
            self.status = "coordination call limit reached for this turn".to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        let source = active.conversation_id.to_string();
        let authorization = mcp_agent_defaults::tool_policy(
            &self.mcp_agent_defaults.policy_rules,
            &proposal.provider,
            &proposal.tool,
        );
        let result: Result<Value, String> = if authorization
            != Ok(mcp_agent_defaults::ToolPolicy::Automatic)
        {
            Err("coordination operation denied by exact MCP tool policy".to_owned())
        } else {
            match proposal.tool.as_str() {
                "coordination.identity" => {
                    if proposal
                        .arguments
                        .as_object()
                        .is_none_or(|args| !args.is_empty())
                    {
                        Err("coordination.identity accepts no arguments".to_owned())
                    } else {
                        self.coordination_workspace.identity_tool(&source)
                    }
                }
                "coordination.send" => {
                    let Some(args) = proposal.arguments.as_object() else {
                        self.status = "coordination.send requires object arguments".to_owned();
                        return;
                    };
                    let allowed_keys = args
                        .keys()
                        .all(|key| matches!(key.as_str(), "to" | "kind" | "body"));
                    if !allowed_keys || args.len() != 3 {
                        Err("coordination.send requires exactly to, kind and body".to_owned())
                    } else {
                        match (
                            args.get("to").and_then(Value::as_u64),
                            args.get("kind").and_then(Value::as_str),
                            args.get("body").and_then(Value::as_str),
                        ) {
                            (Some(to), Some(kind), Some(body)) => {
                                let before = self.coordination_workspace.clone();
                                match self.coordination_workspace.send(
                                    &source,
                                    to,
                                    kind,
                                    body,
                                    unix_now_ms(),
                                ) {
                                    Ok((id, status)) => {
                                        match self
                                            .coordination_workspace
                                            .save_atomic(&self.coordination_store_path())
                                        {
                                            Ok(()) => Ok(serde_json::json!({
                                                "message_id":id,"status":status,
                                                "source_conversation":source,
                                                "target_worker":to,
                                                "verified_by":"Chatarium local coordination authority",
                                            })),
                                            Err(error) => {
                                                self.coordination_workspace = before;
                                                Err(format!(
                                                    "coordination message persistence failed: {error}"
                                                ))
                                            }
                                        }
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            _ => Err("coordination.send arguments have invalid types".to_owned()),
                        }
                    }
                }
                _ => Err("unknown coordination operation".to_owned()),
            }
        };
        let tool_result = match result {
            Ok(value) => serde_json::json!({"ok":true,"result":value}),
            Err(error) => serde_json::json!({"ok":false,"error":error}),
        };
        // This result is derived from the saved workspace and is distinguishable
        // from user text and journaled shell MCP tool results.
        let siwc_bridge::BridgeCommand::StreamResponse {
            model,
            mut input,
            instructions,
            request_patch,
            ..
        } = active.command.clone()
        else {
            self.status = "coordination continuation requires a Responses stream".to_owned();
            return;
        };
        let Some(messages) = input.as_array_mut() else {
            self.status = "coordination continuation requires typed messages".to_owned();
            return;
        };
        messages.push(serde_json::json!({
            "role":"assistant","content":final_text
        }));
        messages.push(serde_json::json!({
            "role":"user",
            "content":format!(
                "CHATARIUM VERIFIED COORDINATION TOOL RESULT (not a human message)\n{}",
                tool_result
            )
        }));
        let request_id = format!(
            "{}-coordination-{}",
            active.turn_id,
            self.coordination_tool_budget.used()
        );
        let next = PendingRemoteTurn {
            conversation_id: active.conversation_id,
            turn_id: active.turn_id,
            request_id: request_id.clone(),
            model,
            input,
            instructions,
            request_patch,
        };
        let journal = remote_turn_payload(
            active.turn_id,
            &request_id,
            Some(&next.model),
            None,
            Some("Chatarium verified local coordination operation; resuming same turn"),
        );
        if !self.queue_turn_event(active.turn_id, EventKind::DispatchAttempted, journal) {
            self.status =
                "coordination result recorded; remote continuation could not be journaled"
                    .to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        self.delivery
            .begin(active.conversation_id.to_string(), request_id);
        self.pending_remote_turn = Some(next);
        self.agent_mcp_flow = None;
        self.status = "coordination operation recorded; assistant continuation queued".to_owned();
    }

    /// Exactly the admitted result from the durable journal is returned to the
    /// model; no manual or model-authored result string can be substituted.
    fn continue_agent_after_admitted_result(&mut self, call_id: ToolCallId) {
        let Some(mut flow) = self.agent_mcp_flow.take() else {
            return;
        };
        if flow.pending_call != Some(call_id) || flow.conversation_id != self.local_conversation_id
        {
            self.agent_mcp_flow = Some(flow);
            return;
        }
        let admitted = match replay_admitted_tool_results(&self.events, flow.conversation_id) {
            Ok(results) => results,
            Err(error) => {
                self.status = format!("MCP continuation blocked by result audit: {error}");
                return;
            }
        };
        let Some(outcome) = admitted
            .into_iter()
            .find(|result| result.record.call_id == call_id)
        else {
            self.status = "MCP continuation stopped: no exact admitted tool result".to_owned();
            return;
        };
        let record = outcome.record;
        let tool_context = context_composer::TranscriptMessage::tool_result(
            &outcome.text,
            record.call_id.get(),
            record.route_id.get(),
            record.provider_id.get(),
            record.source_session_id.get(),
            record.outcome_sequence,
            record.last_decision_sequence,
            record.outcome_kind.stable_name(),
        );
        let siwc_bridge::BridgeCommand::StreamResponse {
            model,
            mut input,
            instructions,
            request_patch,
            ..
        } = flow.command.clone()
        else {
            self.status = "MCP continuation requires a Responses stream command".to_owned();
            return;
        };
        let Some(messages) = input.as_array_mut() else {
            self.status = "MCP continuation requires a typed input-message array".to_owned();
            return;
        };
        messages.push(serde_json::json!({
            "role": "assistant",
            "content": flow.proposal_text,
        }));
        messages.push(serde_json::json!({
            "role": "user",
            "content": tool_context.text,
        }));
        let request_id = format!("{}-mcp-{}", flow.turn_id, flow.call_count);
        self.delivery
            .begin(flow.conversation_id.to_string(), request_id.clone());
        let pending = PendingRemoteTurn {
            conversation_id: flow.conversation_id,
            turn_id: flow.turn_id,
            request_id: request_id.clone(),
            model,
            input,
            instructions,
            request_patch,
        };
        let journal_entry = remote_turn_payload(
            flow.turn_id,
            &request_id,
            Some(&pending.model),
            None,
            Some("same user turn resumed after exact journaled MCP outcome admission"),
        );
        if !self.queue_turn_event(flow.turn_id, EventKind::DispatchAttempted, journal_entry) {
            self.status = "MCP continuation blocked: could not journal dispatch".to_owned();
            return;
        }
        flow.pending_call = None;
        flow.pending_route = None;
        self.pending_remote_turn = Some(pending);
        self.agent_mcp_flow = Some(flow);
        self.status =
            "MCP result admitted; resuming assistant without a new user message".to_owned();
    }

    fn check_sandbox_host_readiness(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot check host confinement: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::CheckSandboxHostReadiness) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.last_sandbox_host_check = None;
                self.status =
                    "checking real Linux namespace sandbox with a fixed harmless process…"
                        .to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue host-readiness check: {error}");
            }
        }
    }

    fn execute_approved_tool(&mut self, call_id: ToolCallId) {
        let is_http = replay_tool_call_audit(&self.events)
            .ok()
            .and_then(|records| records.into_iter().find(|call| call.call_id == call_id))
            .and_then(|call| {
                replay_tool_provider_audit(&self.events)
                    .ok()
                    .and_then(|providers| {
                        providers
                            .into_iter()
                            .find(|provider| provider.provider_id == call.provider_id)
                            .map(|provider| (provider.name.as_str().to_owned(), call.route_id))
                    })
            });
        if let Some((provider, Some(route))) = is_http {
            if local_http_mcp::known_provider(&provider) {
                self.dispatch_local_http_mcp(route);
                return;
            }
        }
        self.execute_approved_stdio_tool(call_id);
    }

    fn dispatch_local_http_mcp(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot invoke localhost MCP: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchLocalHttpMcp { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating and reserving fixed localhost MCP route {}",
                    route_id.get()
                );
            }
            Err(error) => self.status = format!("cannot queue localhost MCP: {error}"),
        }
    }

    fn execute_approved_stdio_tool(&mut self, call_id: ToolCallId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot execute external tool: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ExecuteApprovedStdioTool { call_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "checking Linux namespace confinement before reserving one-shot tool call {}…",
                    call_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue external tool execution: {error}");
            }
        }
    }

    fn dispatch_host_shell(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot run host shell: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchHostShell { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating host shell settings and durable route {} before one-shot execution",
                    route_id.get()
                );
            }
            Err(error) => self.status = format!("cannot queue host shell: {error}"),
        }
    }

    fn save_host_shell_settings(&mut self) {
        if let Err(error) = self.host_shell_settings.validate() {
            self.status = format!("host shell settings invalid: {error}");
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.status = "cannot save shell settings: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = tx.send(PersistCommand::SaveHostShellSettings {
            settings: self.host_shell_settings.clone(),
        }) {
            self.status = format!("cannot queue shell settings save: {error}");
        } else {
            self.status = "shell choice and command blacklist queued for persistence".to_owned();
        }
    }

    fn dispatch_builtin_hello(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot execute local builtin: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchBuiltinHello { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating approved builtin route {} and consuming one-shot dispatch authority…",
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue builtin dispatch: {error}");
            }
        }
    }

    fn decide_tool_call_route(&mut self, route_id: RouteId, decision: RouteUserDecision) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update tool route policy: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolCallRoute { route_id, decision }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for tool route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool route decision: {error}");
            }
        }
    }

    fn decide_tool_result_context(
        &mut self,
        call_id: ToolCallId,
        decision: ToolResultContextDecision,
    ) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update tool result context: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolResultContext {
            call_id,
            conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording tool call {} context {} decision…",
                    call_id.get(),
                    decision.stable_name()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool result context decision: {error}");
            }
        }
    }

    fn record_local_memory(&mut self) {
        if self.local_memory_command_pending || self.local_memory_draft.trim().is_empty() {
            return;
        }
        let memory_id = match next_available_local_memory_id(&self.events) {
            Ok(memory_id) => memory_id,
            Err(error) => {
                self.status = format!("cannot allocate local memory identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot record local memory: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordLocalMemory {
            memory_id,
            source_conversation_id: native_conversation_search::recording_source_conversation(
                self.local_conversation_id,
                self.staged_local_memory_origin,
            ),
            text: self.local_memory_draft.clone(),
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("recording immutable local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory: {error}");
            }
        }
    }

    fn select_local_memory_for_next_request(&mut self, memory_id: LocalMemoryId) {
        if self.commit_in_flight.is_some() {
            return;
        }
        let selected = self
            .local_memory_one_shot_selections
            .entry(self.local_conversation_id)
            .or_default();
        if selected.contains(&memory_id) {
            return;
        }
        if selected.len() >= MAX_ONE_SHOT_MEMORIES_PER_TURN {
            self.status = format!(
                "cannot select more than {} one-shot memories for one request",
                MAX_ONE_SHOT_MEMORIES_PER_TURN
            );
            return;
        }
        selected.insert(memory_id);
        self.status = format!(
            "local memory {} selected for the next authored request only",
            memory_id.get()
        );
    }

    fn deselect_local_memory_for_next_request(&mut self, memory_id: LocalMemoryId) {
        if self.commit_in_flight.is_some() {
            return;
        }
        if let Some(selected) = self
            .local_memory_one_shot_selections
            .get_mut(&self.local_conversation_id)
        {
            selected.remove(&memory_id);
        }
        self.local_memory_one_shot_selections
            .retain(|_, selected| !selected.is_empty());
        self.status = format!(
            "local memory {} removed from next-request-only selection",
            memory_id.get()
        );
    }

    fn decide_local_memory_context(
        &mut self,
        memory_id: LocalMemoryId,
        decision: LocalMemoryContextDecision,
    ) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update local memory context eligibility: persistence unavailable"
                .to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalMemoryContext {
            memory_id,
            destination_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!(
                    "recording local memory {} context {} decision…",
                    memory_id.get(),
                    local_memory_context_decision_label(decision),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local memory context decision: {error}");
            }
        }
    }

    fn supersede_local_memory(
        &mut self,
        predecessor_memory_id: LocalMemoryId,
        successor_memory_id: LocalMemoryId,
    ) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot supersede local memory: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::SupersedeLocalMemory {
            predecessor_memory_id,
            successor_memory_id,
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!(
                    "recording local memory {} → {} supersession…",
                    predecessor_memory_id.get(),
                    successor_memory_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local memory supersession: {error}");
            }
        }
    }

    fn add_local_memory_label(&mut self, memory_id: LocalMemoryId) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(draft) = self.local_memory_label_drafts.get(&memory_id) else {
            return;
        };
        let label = match LocalMemoryLabel::new(draft.as_str()) {
            Ok(label) => label,
            Err(error) => {
                self.status = format!(
                    "cannot add label to local memory {}: {error:?}",
                    memory_id.get()
                );
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot add local memory label: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AddLocalMemoryLabel { memory_id, label }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("adding label to local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory label addition: {error}");
            }
        }
    }

    fn remove_local_memory_label(&mut self, memory_id: LocalMemoryId, label: LocalMemoryLabel) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot remove local memory label: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RemoveLocalMemoryLabel { memory_id, label }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("removing label from local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory label removal: {error}");
            }
        }
    }

    fn enable_worker_lifecycle(&mut self) {
        if self.lifecycle_command_pending {
            return;
        }
        let worker_id = match next_available_worker_id(&self.events) {
            Ok(worker_id) => worker_id,
            Err(error) => {
                self.status = format!("cannot allocate worker identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot enable worker lifecycle: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindLocalConversationWorker {
            conversation_id: self.local_conversation_id,
            worker_id,
        }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!("binding local conversation to worker {}…", worker_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue worker binding: {error}");
            }
        }
    }

    fn bind_worker_to_current_session(&mut self, worker_id: WorkerId) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind worker session: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindCurrentSessionWorker {
            conversation_id: self.local_conversation_id,
            worker_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = format!(
                    "binding worker {} to this conversation's current session…",
                    worker_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker-session binding: {error}");
            }
        }
    }

    fn designate_current_session_controller(&mut self) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot designate controller: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DesignateCurrentSessionController {
            conversation_id: self.local_conversation_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = "designating current session as controller…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller designation: {error}");
            }
        }
    }

    fn supervise_local_conversation(&mut self, worker_conversation_id: LocalConversationId) {
        if self.supervision_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind controller supervision: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindControllerToLocalWorker {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
        }) {
            Ok(()) => {
                self.supervision_command_pending = true;
                self.status = "binding current controller to local worker session…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller-worker binding: {error}");
            }
        }
    }

    fn create_controller_continuation_lease(
        &mut self,
        worker_conversation_id: LocalConversationId,
        allowance: u32,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot create continuation lease: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::CreateControllerContinuationLease {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
            allowance,
        }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status =
                    format!("creating bounded continuation lease with allowance {allowance}…");
            }
            Err(error) => {
                self.status = format!("failed to queue continuation lease creation: {error}");
            }
        }
    }

    fn propose_controller_worker_control(
        &mut self,
        worker_conversation_id: LocalConversationId,
        action: ControllerControlAction,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose controller control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeControllerWorkerControl {
            controller_conversation_id: self.local_conversation_id,
            worker_conversation_id,
            action,
        }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!("proposing controller {} control…", action.label());
            }
            Err(error) => {
                self.status = format!("failed to queue controller control proposal: {error}");
            }
        }
    }

    fn decide_controller_worker_control_route(
        &mut self,
        route_id: RouteId,
        decision: RouteUserDecision,
    ) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot decide controller control route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerWorkerControlRoute { route_id, decision })
        {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for controller control route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue controller control decision: {error}");
            }
        }
    }

    fn dispatch_controller_worker_control_route(&mut self, route_id: RouteId) {
        if self.controller_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot dispatch controller control route: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchControllerWorkerControlRoute { route_id }) {
            Ok(()) => {
                self.controller_control_command_pending = true;
                self.status = format!(
                    "revalidating and dispatching controller control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue controller control dispatch: {error}");
            }
        }
    }

    fn decide_controller_worker_result_context(
        &mut self,
        route_id: RouteId,
        decision: ControllerWorkerResultContextDecision,
    ) {
        if self.controller_result_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update controller result context: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerWorkerResultContext {
            route_id,
            controller_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.controller_result_context_command_pending = true;
                self.status = format!(
                    "recording controller result context {} decision for route {}…",
                    controller_worker_result_context_decision_label(decision),
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status =
                    format!("failed to queue controller result context decision: {error}");
            }
        }
    }

    fn decide_controller_coordination_result_context(
        &mut self,
        coordination_turn_id: LocalTurnId,
        decision: ControllerCoordinationResultContextDecision,
    ) {
        if self.controller_coordination_result_context_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot update controller coordination result context: persistence unavailable"
                    .to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideControllerCoordinationResultContext {
            coordination_turn_id,
            controller_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.controller_coordination_result_context_command_pending = true;
                self.status = format!(
                    "recording coordination result context {} decision for turn {}…",
                    controller_coordination_result_context_decision_label(decision),
                    coordination_turn_id,
                );
            }
            Err(error) => {
                self.status =
                    format!("failed to queue coordination result context decision: {error}");
            }
        }
    }

    fn record_controller_coordination_suggestion(
        &mut self,
        coordination_turn_id: LocalTurnId,
        basis_result_route_id: RouteId,
        action: CoordinationSuggestionAction,
    ) {
        if self.controller_coordination_suggestion_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot record coordination suggestion: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordControllerCoordinationSuggestion {
            controller_conversation_id: self.local_conversation_id,
            coordination_turn_id,
            basis_result_route_id,
            action,
        }) {
            Ok(()) => {
                self.controller_coordination_suggestion_command_pending = true;
                self.status = format!(
                    "recording powerless coordination suggestion {} from route {}…",
                    coordination_suggestion_action_label(action),
                    basis_result_route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue coordination suggestion: {error}");
            }
        }
    }

    fn promote_controller_coordination_suggestion(
        &mut self,
        suggestion_id: CoordinationSuggestionId,
    ) {
        if self.controller_coordination_suggestion_command_pending
            || self.controller_control_command_pending
        {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot promote coordination suggestion: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::PromoteControllerCoordinationSuggestion {
            controller_conversation_id: self.local_conversation_id,
            suggestion_id,
        }) {
            Ok(()) => {
                self.controller_coordination_suggestion_command_pending = true;
                self.controller_control_command_pending = true;
                self.status = format!(
                    "promoting coordination suggestion {} through ordinary control admission…",
                    suggestion_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue suggestion promotion: {error}");
            }
        }
    }

    fn start_controller_coordination(&mut self) {
        if self.controller_coordination_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot start controller coordination: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::StartControllerCoordination {
            controller_conversation_id: self.local_conversation_id,
        }) {
            Ok(()) => {
                self.controller_coordination_command_pending = true;
                self.status =
                    "starting durable non-authored controller coordination turn…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue controller coordination start: {error}");
            }
        }
    }

    fn dispatch_controller_coordination(&mut self, turn_id: LocalTurnId) {
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.status =
                "cannot dispatch controller coordination while another remote turn is active"
                    .to_owned();
            return;
        }
        if !self.remote_connected() {
            self.status =
                "cannot dispatch controller coordination: ChatGPT is not connected".to_owned();
            return;
        }
        if self.capability_probe.running() {
            self.status =
                "cannot dispatch controller coordination while capability probes are running"
                    .to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.status = "cannot dispatch controller coordination: no model selected".to_owned();
            return;
        };

        let coordination = match replay_controller_coordination_audit(&self.events) {
            Ok(records) => records.into_iter().find(|record| {
                record.coordination_turn_id == turn_id
                    && record.controller_conversation_id == self.local_conversation_id
            }),
            Err(error) => {
                self.status = format!("cannot replay controller coordination: {error}");
                return;
            }
        };
        let Some(coordination) = coordination else {
            self.status = format!(
                "controller coordination turn {} does not belong to this conversation",
                turn_id
            );
            return;
        };
        if coordination.result_sequence.is_some() {
            self.status = format!(
                "controller coordination turn {} is already terminal",
                turn_id
            );
            return;
        }

        let transport = match controller_coordination_transport_state(&self.events, turn_id) {
            Ok(state) => state,
            Err(error) => {
                self.status = format!("cannot replay controller coordination transport: {error}");
                return;
            }
        };
        if transport.was_dispatched() {
            self.status = format!(
                "controller coordination turn {} already has durable dispatch evidence",
                turn_id
            );
            return;
        }

        let request_patch = match self.current_behavior_request_patch() {
            Ok(patch) => patch,
            Err(error) => {
                self.status = format!("cannot coordinate with current behavior profile: {error}");
                return;
            }
        };

        let coordination_prefix = self
            .events
            .iter()
            .take_while(|event| event.sequence <= coordination.started_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let mut transcript = context_transcript(&projected_local_display_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ));
        match admitted_routed_context_messages(&coordination_prefix, self.local_conversation_id) {
            Ok(routed) => transcript.extend(routed),
            Err(error) => {
                self.status =
                    format!("cannot compose controller coordination routed context: {error}");
                return;
            }
        }
        match admitted_controller_worker_result_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => transcript.extend(results),
            Err(error) => {
                self.status = format!("cannot compose admitted controller results: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        transcript.push(
            context_composer::TranscriptMessage::controller_coordination(
                coordination.controller_session_id.get(),
                coordination.coordination_turn_id.to_string(),
                coordination.started_sequence,
            ),
        );

        let context_plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            self.conversation_instructions.as_str(),
            self.conversation_developer_context.as_str(),
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("controller coordination context budget rejected: {error}");
                return;
            }
        };
        let frozen_tool_sources = match admitted_tool_result_context_messages(
            &coordination_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => results
                .into_iter()
                .map(|item| item.source)
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = format!("cannot audit coordination tool eligibility: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            self.local_conversation_id,
            "controller_coordination",
            &frozen_tool_sources,
            &context_plan,
        ) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = format!("cannot record coordination context evidence: {error}");
                return;
            }
        };
        let request_id = coordination.coordination_turn_id.to_string();
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id: self.local_conversation_id,
            turn_id: coordination.coordination_turn_id,
            request_id: request_id.clone(),
            model: model.clone(),
            input: context_plan.input_json(),
            instructions: context_plan.instructions.clone(),
            request_patch,
        });

        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                coordination.coordination_turn_id,
                &request_id,
                Some(&model),
                None,
                Some("non-authored controller coordination remote dispatch"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.pending_remote_turn = None;
                self.status = format!("cannot journal coordination context evidence: {error}");
                return;
            }
        };
        if self.queue_turn_event(
            coordination.coordination_turn_id,
            EventKind::DispatchAttempted,
            payload,
        ) {
            self.status = format!(
                "controller coordination turn {} durably dispatching through ChatGPT…",
                coordination.coordination_turn_id
            );
        } else {
            self.pending_remote_turn = None;
        }
    }

    fn acknowledge_worker_control(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot acknowledge worker control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AcknowledgeWorkerControl {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "acknowledging delivered worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control acknowledgement: {error}");
            }
        }
    }

    fn record_worker_control_status_result(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot record worker control status result: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordWorkerControlStatusResult {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "recording durable status result for worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control status result: {error}");
            }
        }
    }

    fn apply_worker_control(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot apply worker control: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ApplyWorkerControl {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "applying acknowledged worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker control action: {error}");
            }
        }
    }

    fn start_worker_continuation_execution(&mut self, route_id: RouteId) {
        if self.worker_control_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot start continuation execution: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::StartWorkerContinuationExecution {
            worker_conversation_id: self.local_conversation_id,
            route_id,
        }) {
            Ok(()) => {
                self.worker_control_command_pending = true;
                self.status = format!(
                    "starting durable continuation execution for worker control route {}…",
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue continuation execution start: {error}");
            }
        }
    }

    fn dispatch_worker_continuation_execution(&mut self, route_id: RouteId) {
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.status =
                "cannot dispatch continuation while another remote turn is active".to_owned();
            return;
        }
        if !self.remote_connected() {
            self.status = "cannot dispatch continuation: ChatGPT is not connected".to_owned();
            return;
        }
        if self.capability_probe.running() {
            self.status =
                "cannot dispatch continuation while capability probes are running".to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.status = "cannot dispatch continuation: no model selected".to_owned();
            return;
        };

        let execution = match replay_worker_continuation_execution_audit(&self.events) {
            Ok(records) => records.into_iter().find(|record| {
                record.route_id == route_id
                    && record.worker_conversation_id == self.local_conversation_id
            }),
            Err(error) => {
                self.status = format!("cannot replay continuation execution: {error}");
                return;
            }
        };
        let Some(execution) = execution else {
            self.status = format!(
                "worker control route {} has no continuation execution start for this conversation",
                route_id.get()
            );
            return;
        };

        let transport =
            match continuation_execution_transport_state(&self.events, execution.execution_turn_id)
            {
                Ok(state) => state,
                Err(error) => {
                    self.status = format!("cannot replay continuation transport: {error}");
                    return;
                }
            };
        if transport.was_dispatched() {
            self.status = format!(
                "continuation execution turn {} already has durable dispatch evidence",
                execution.execution_turn_id
            );
            return;
        }

        let request_patch = match self.current_behavior_request_patch() {
            Ok(patch) => patch,
            Err(error) => {
                self.status = format!("cannot continue with current behavior profile: {error}");
                return;
            }
        };

        let execution_prefix = self
            .events
            .iter()
            .take_while(|event| event.sequence <= execution.started_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let mut transcript = context_transcript(&projected_local_display_messages(
            &execution_prefix,
            self.local_conversation_id,
        ));
        match admitted_routed_context_messages(&execution_prefix, self.local_conversation_id) {
            Ok(routed) => transcript.extend(routed),
            Err(error) => {
                self.status = format!("cannot compose continuation routed context: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        transcript.push(
            context_composer::TranscriptMessage::controller_continuation(
                execution.control_id.get(),
                execution.route_id.get(),
                execution.worker_id.get(),
                execution.goal_id.get(),
                execution.lease_id.get(),
                execution.permit_ordinal,
                execution.started_sequence,
            ),
        );

        let context_plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            self.conversation_instructions.as_str(),
            self.conversation_developer_context.as_str(),
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("worker continuation context budget rejected: {error}");
                return;
            }
        };
        let frozen_tool_sources = match admitted_tool_result_context_messages(
            &execution_prefix,
            self.local_conversation_id,
        ) {
            Ok(results) => results
                .into_iter()
                .map(|item| item.source)
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = format!("cannot audit continuation tool eligibility: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            self.local_conversation_id,
            "worker_continuation",
            &frozen_tool_sources,
            &context_plan,
        ) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = format!("cannot record continuation context evidence: {error}");
                return;
            }
        };
        let request_id = execution.execution_turn_id.to_string();
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id: self.local_conversation_id,
            turn_id: execution.execution_turn_id,
            request_id: request_id.clone(),
            model: model.clone(),
            input: context_plan.input_json(),
            instructions: context_plan.instructions.clone(),
            request_patch,
        });

        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                execution.execution_turn_id,
                &request_id,
                Some(&model),
                None,
                Some("bounded controller continuation remote dispatch"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.pending_remote_turn = None;
                self.status = format!("cannot journal continuation context evidence: {error}");
                return;
            }
        };
        if self.queue_turn_event(
            execution.execution_turn_id,
            EventKind::DispatchAttempted,
            payload,
        ) {
            self.status = format!(
                "continuation execution turn {} durably dispatching through ChatGPT…",
                execution.execution_turn_id
            );
        } else {
            self.pending_remote_turn = None;
        }
    }

    fn assign_next_worker_goal(&mut self, worker_id: WorkerId) {
        if self.lifecycle_command_pending {
            return;
        }
        let current = match worker_record(&self.events, worker_id) {
            Ok(record) => record,
            Err(error) => {
                self.status = format!("cannot project worker lifecycle: {error}");
                return;
            }
        };
        let goal_id = WorkerGoalId::new(
            current
                .and_then(|record| record.lifecycle.goal_id())
                .map_or(1, |goal_id| goal_id.get().saturating_add(1)),
        );
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot assign worker goal: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AssignWorkerGoal { worker_id, goal_id }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!(
                    "assigning worker {} goal {}…",
                    worker_id.get(),
                    goal_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker goal assignment: {error}");
            }
        }
    }

    fn transition_worker(
        &mut self,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        action: WorkerAction,
    ) {
        if self.lifecycle_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update worker lifecycle: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::TransitionWorker {
            worker_id,
            goal_id,
            action,
        }) {
            Ok(()) => {
                self.lifecycle_command_pending = true;
                self.status = format!(
                    "recording worker {} lifecycle action {action:?}…",
                    worker_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue worker lifecycle action: {error}");
            }
        }
    }

    fn persist_local_conversation_catalog(&mut self) -> bool {
        let Some(sender) = &self.persist_tx else {
            self.status =
                "cannot persist local conversation workspace: persistence unavailable".to_owned();
            return false;
        };
        if let Err(error) = sender.send(PersistCommand::SaveLocalConversationCatalog {
            catalog: self.local_conversation_catalog.clone(),
        }) {
            self.status = format!("failed to queue local conversation workspace: {error}");
            return false;
        }
        true
    }

    fn reader_conversation_key(&self) -> String {
        self.selected_historical_conversation
            .map(|conversation| format!("mirror:{conversation}"))
            .unwrap_or_else(|| format!("local:{}", self.local_conversation_id))
    }
}

/// Count persisted Chatarium data files, without following directory symlinks.
/// Cached for a few seconds so UI frames do not traverse storage repeatedly.
fn local_storage_bytes_cached(ctx: &egui::Context, journal_path: &std::path::Path) -> u64 {
    let id = egui::Id::new("chatarium-local-data-usage");
    if let Some((last, bytes)) = ctx.data_mut(|data| data.get_temp::<(Instant, u64)>(id)) {
        if last.elapsed() < Duration::from_secs(8) {
            return bytes;
        }
    }
    let mut total = 0_u64;
    let mut scanned = 0_usize;
    if let Some(root) = journal_path.parent() {
        let mut folders = vec![(root.to_path_buf(), 0_usize)];
        while let Some((folder, depth)) = folders.pop() {
            if depth > 8 || scanned >= 20_000 {
                break;
            }
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            for entry in entries.flatten() {
                scanned += 1;
                if scanned >= 20_000 {
                    break;
                }
                let Ok(metadata) = entry.path().symlink_metadata() else {
                    continue;
                };
                if metadata.is_file() {
                    total = total.saturating_add(metadata.len());
                } else if metadata.is_dir() {
                    folders.push((entry.path(), depth + 1));
                }
            }
        }
    }
    ctx.data_mut(|data| data.insert_temp(id, (Instant::now(), total)));
    total
}

fn readable_byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn delivery_status_color(
    attempt: &delivery_telemetry::Attempt,
    slow_warning_seconds: u32,
) -> egui::Color32 {
    use delivery_telemetry::Phase;
    if attempt.slow(Duration::from_secs(u64::from(slow_warning_seconds)))
        || attempt.phase == Phase::RetryScheduled
    {
        egui::Color32::from_rgb(242, 167, 79)
    } else {
        match attempt.phase {
            Phase::Complete => egui::Color32::from_rgb(126, 210, 156),
            Phase::Failed | Phase::Unconfirmed => egui::Color32::from_rgb(235, 105, 107),
            _ => egui::Color32::from_rgb(238, 207, 103),
        }
    }
}

fn draw_delivery_timeline(
    ui: &mut egui::Ui,
    timeline: &delivery_telemetry::Timeline,
    conversation_id: &str,
    slow_warning_seconds: u32,
) {
    let attempts = timeline
        .for_conversation(conversation_id)
        .collect::<Vec<_>>();
    if attempts.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.separator();

    // Never expand the native chat transcript just because another response
    // finished. The per-conversation fold state is controlled only by the user.
    let summary = if let Some(mean) = timeline.mean_response_seconds(conversation_id) {
        format!(
            "Delivery history · {} requests · avg {mean:.1}s",
            attempts.len()
        )
    } else {
        format!("Delivery history · {} requests", attempts.len())
    };
    egui::CollapsingHeader::new(summary)
        .id_salt(("delivery-history", conversation_id))
        .default_open(false)
        .show(ui, |ui| {
            // Explicit expansion still cannot grow without bound. Preserve the
            // eight most recent exact per-response records inside this viewport.
            egui::ScrollArea::vertical()
                .id_salt(("delivery-history-scroll", conversation_id))
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    draw_delivery_timeline_details(
                        ui,
                        timeline,
                        conversation_id,
                        slow_warning_seconds,
                        &attempts,
                    );
                });
        });
}

fn draw_delivery_timeline_details(
    ui: &mut egui::Ui,
    timeline: &delivery_telemetry::Timeline,
    conversation_id: &str,
    slow_warning_seconds: u32,
    attempts: &[&delivery_telemetry::Attempt],
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("DELIVERY HISTORY · ChatGPT Responses")
                .size(11.0)
                .strong(),
        );
        if let Some(mean) = timeline.mean_response_seconds(conversation_id) {
            ui.label(
                egui::RichText::new(format!("session average {:.1}s", mean))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(151, 154, 163)),
            );
        }
        if let Some(mean) = timeline.mean_first_output_seconds(conversation_id) {
            ui.label(
                egui::RichText::new(format!("average first output {:.1}s", mean))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(151, 154, 163)),
            );
        }
    });
    ui.label(egui::RichText::new(
        "Timings: local queue → HTTP ack → waiting for first output → streaming. HTTP ack is transport evidence, not proof of model activity. Timing history resets on restart; turns remain durable.",
    ).size(9.0).color(egui::Color32::from_rgb(125, 129, 139)));
    // Completed rows are individual requests, not repeated copies of the
    // current green "ready for input" status shown in the header.
    for (index, attempt) in attempts.iter().rev().take(8).rev().enumerate() {
        let response_number = attempts.len().saturating_sub(8) + index + 1;
        let color = delivery_status_color(attempt, slow_warning_seconds);
        let label = if attempt.slow(Duration::from_secs(u64::from(slow_warning_seconds))) {
            "SLOW · waiting for first response".to_owned()
        } else if attempt.phase == delivery_telemetry::Phase::Complete {
            format!("Response #{response_number} · complete")
        } else {
            attempt.label().to_owned()
        };
        egui::Frame::default()
            .fill(egui::Color32::from_rgb(29, 31, 36))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("● {label}"))
                            .strong()
                            .size(10.0)
                            .color(color),
                    );
                    ui.label(
                        egui::RichText::new(format!("elapsed {:.1}s", attempt.seconds()))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(195, 198, 205)),
                    );
                    // Show disjoint phases; cumulative ack/first-output times
                    // obscured whether delay was local, network, or remote.
                    for (name, duration) in [
                        ("queue", attempt.queued_seconds()),
                        ("HTTP ack", attempt.network_ack_seconds()),
                        ("reply startup", attempt.post_ack_wait_seconds()),
                        ("stream", attempt.streaming_seconds()),
                    ] {
                        if let Some(seconds) = duration {
                            ui.label(
                                egui::RichText::new(format!("{name} {seconds:.1}s"))
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(151, 154, 163)),
                            );
                        }
                    }
                    if attempt.retries > 0 {
                        ui.label(
                            egui::RichText::new(format!("retry {}", attempt.retries))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(242, 167, 79)),
                        );
                    }
                    if let Some(at) = attempt.retry_at {
                        ui.label(
                            egui::RichText::new(format!(
                                "next attempt in {:.1}s",
                                at.saturating_duration_since(Instant::now()).as_secs_f32()
                            ))
                            .size(10.0)
                            .color(color),
                        );
                    }
                });
                if let Some(error) = &attempt.error {
                    ui.label(
                        egui::RichText::new(error)
                            .size(9.0)
                            .color(egui::Color32::from_rgb(170, 173, 183)),
                    );
                }
            });
        ui.add_space(4.0);
    }
}

/// Non-authored, durable coordination evidence rendered as its own distinct
/// bubble. It never masquerades as an authored user message or enters the transcript context.
fn draw_consumed_coordination_bubble(
    ui: &mut egui::Ui,
    message: &coordination_workspace::ConsumedDisplay,
    preferences: &display_preferences::DisplayPreferences,
    ctx: &egui::Context,
) {
    let rgb = preferences.coordination_color(&message.kind);
    let fill = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    // Reserve room for the frame's margins and stroke, even in a narrow reader.
    // A horizontal parent layout with an inner width equal to the whole bubble
    // previously pushed the body off the right edge of the conversation.
    let available = ui.available_width().max(1.0);
    let frame_width = (available * 0.88).min(900.0);
    let inset: i8 = if frame_width < 300.0 { 10 } else { 16 };
    let text_width = (frame_width - f32::from(inset) * 2.0 - 4.0).max(1.0);
    ui.push_id(("consumed-coordination", message.id), |ui| {
        egui::Frame::default()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, fill.gamma_multiply(1.7)))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(inset, 12))
            .show(ui, |ui| {
                ui.set_width(text_width);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Coordination {} · #{}",
                            message.kind, message.id
                        ))
                        .strong()
                        .color(egui::Color32::WHITE),
                    );
                    if ui.small_button("Copy").clicked() {
                        ctx.copy_text(message.body.clone());
                    }
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&message.body)
                            .color(egui::Color32::from_rgb(244, 246, 248)),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "From {} · handed to local bridge {} · model processing or reply not implied",
                            message.from,
                            timestamp_display::utc_minute(message.consumed_ms as f64 / 1000.0)
                                .unwrap_or_else(|| "time unknown".to_owned()),
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 222, 226)),
                    )
                    .wrap(),
                );
            });
    });
}

fn draw_reader_bubble(
    ui: &mut egui::Ui,
    message: &DisplayMessage,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) -> egui::Response {
    let preferences = ctx
        .data_mut(|data| {
            data.get_temp::<display_preferences::DisplayPreferences>(egui::Id::new(
                "chatarium-display-preferences",
            ))
        })
        .unwrap_or_default();
    let palette = &preferences.message_colors;
    let bubble_fill = |index: usize| {
        let [r, g, b] = palette[index];
        egui::Color32::from_rgb(r, g, b)
    };
    match message.role {
        DisplayRole::User => {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(0),
                    "You",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Assistant => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(1),
                    "Agent",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Mcp => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(2),
                    "MCP tool",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Activity => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(3),
                    "Transport trace",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
    }
}

fn transcript_bubble(
    ui: &mut egui::Ui,
    message: &DisplayMessage,
    fill: egui::Color32,
    label: &str,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) -> egui::Response {
    // The bubble is placed from an outer horizontal layout for left/right message
    // alignment, but its own contents must be vertical. If the inner UI inherits
    // that horizontal layout, the header consumes the row and the body is forced
    // into a tiny residual strip (often one character wide after a resize).
    //
    // Recompute the width from the current parent allocation every frame so a
    // maximized/restored window immediately reflows the transcript.
    // A single width rule shared by all roles; preserve contrasting side
    // alignment, but avoid different card widths based on short message text.
    // Keep a real gutter inside the outline even for narrow windows.
    // Account for the entire frame inset before fixing the inner width.
    let available_width = ui.available_width();
    let content_width = (available_width * 0.80)
        .clamp(200.0, 950.0)
        .min((available_width - 47.0).max(1.0));

    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(
            1.3_f32,
            if label == "You" {
                egui::Color32::from_rgb(66, 139, 228)
            } else if label == "Agent" {
                egui::Color32::from_rgb(65, 166, 108)
            } else if label == "Transport trace" {
                egui::Color32::from_rgb(213, 154, 73)
            } else {
                egui::Color32::from_rgb(155, 109, 215)
            },
        ))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(22, 13))
        .show(ui, |ui| {
            ui.set_width(content_width);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0).strong().color(
                        if label == "You" {
                            egui::Color32::from_rgb(147, 191, 238)
                        } else if label == "Agent" {
                            egui::Color32::from_rgb(163, 221, 178)
                        } else if label == "Transport trace" {
                            egui::Color32::from_rgb(239, 199, 133)
                        } else {
                            egui::Color32::from_rgb(203, 166, 247)
                        },
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Copy").clicked() {
                            ctx.copy_text(offline_reader::copy_payload(&message.text));
                        }
                    });
                });
                ui.add_space(4.0);
                if matches!(message.role, DisplayRole::Mcp | DisplayRole::Activity) {
                    // Keep tool bytes and activity metadata non-executable.
                    // Neither surface may inject author-role markdown.
                    let activity = message.role == DisplayRole::Activity;
                    if activity {
                        // Show useful timing, not a wall of individually journaled deltas.
                        let summary = message.text.lines().nth(1).unwrap_or("No transport events");
                        ui.label(
                            egui::RichText::new(summary.trim())
                                .size(11.0)
                                .color(egui::Color32::from_rgb(231, 202, 159)),
                        );
                        ui.label(
                            egui::RichText::new("Bridge timing only · not model thinking")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(178, 161, 139)),
                        );
                    }
                    egui::CollapsingHeader::new(if activity {
                        "Raw transport events · expand to inspect"
                    } else {
                        "Exact tool activity · expand to inspect"
                    })
                    .id_salt(("non-authored-activity", message.sequence))
                    .default_open(!activity && message.text.len() < 720)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(&message.text).monospace().size(11.0));
                            });
                    });
                } else {
                    let show_markdown = ctx
                        .data_mut(|data| {
                            data.get_temp::<display_preferences::DisplayPreferences>(egui::Id::new(
                                "chatarium-display-preferences",
                            ))
                        })
                        .unwrap_or_default()
                        .render_markdown;
                    if show_markdown {
                        for block in offline_reader::parse_markdown(&message.text) {
                            render_markdown_block(ui, &block, search_query, active_hit, ctx);
                            ui.add_space(6.0);
                        }
                    } else {
                        // Literal mode must keep Markdown markers, including
                        // inline backticks, exactly as they were authored.
                        ui.label(
                            egui::RichText::new(&message.text)
                                .color(egui::Color32::from_rgb(232, 234, 239)),
                        );
                    }
                }
                ui.add_space(5.0);
                let origin = message
                    .provenance_label
                    .as_deref()
                    .unwrap_or("source not specified");
                let metadata = message
                    .timestamp
                    .and_then(timestamp_display::utc_minute)
                    .map(|timestamp| format!("{label} · {timestamp} · {origin}"))
                    .unwrap_or_else(|| format!("{label} · {origin} · event #{}", message.sequence));
                ui.label(
                    egui::RichText::new(metadata)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(116, 121, 133)),
                );
            });
        })
        .response
}

fn render_markdown_block(
    ui: &mut egui::Ui,
    block: &offline_reader::MarkdownBlock,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) {
    match block {
        offline_reader::MarkdownBlock::Paragraph(text) => {
            render_markdown_lines(ui, text, search_query, active_hit, None);
        }
        offline_reader::MarkdownBlock::Heading { level, text } => {
            render_markdown_lines(ui, text, search_query, active_hit, Some(*level));
        }
        offline_reader::MarkdownBlock::UnorderedList(items) => {
            for item in items {
                render_markdown_list_item(ui, "•", item, search_query, active_hit);
            }
        }
        offline_reader::MarkdownBlock::OrderedList(items) => {
            for (index, item) in items.iter().enumerate() {
                render_markdown_list_item(
                    ui,
                    &format!("{}.", index + 1),
                    item,
                    search_query,
                    active_hit,
                );
            }
        }
        offline_reader::MarkdownBlock::BlockQuote(lines) => {
            egui::Frame::default()
                .fill(egui::Color32::from_rgb(36, 39, 47))
                .stroke(egui::Stroke::new(
                    2.0_f32,
                    egui::Color32::from_rgb(118, 151, 190),
                ))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    for line in lines {
                        render_markdown_lines(ui, line, search_query, active_hit, None);
                    }
                });
        }
        offline_reader::MarkdownBlock::CodeBlock { language, code } => {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(language.as_deref().unwrap_or("code"))
                        .monospace()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(169, 180, 201)),
                );
                if ui.small_button("Copy code").clicked() {
                    ctx.copy_text(offline_reader::copy_payload(code));
                }
            });
            egui::Frame::default()
                .fill(egui::Color32::from_rgb(15, 17, 21))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        render_reader_lines(ui, code, search_query, true, active_hit);
                    });
                });
        }
    }
}

/// Keep list markers beside a vertical text lane. An outer
/// horizontal_wrapped() shifts continuation rows to the bubble's outer left
/// edge because egui places wrapped label rows at the horizontal UI origin.
/// A nonwrapping row with its own vertical text lane preserves the inset and
/// the hanging indent, including long source URLs from web-search results.
fn render_markdown_list_item(
    ui: &mut egui::Ui,
    marker: &str,
    text: &str,
    search_query: &str,
    active_hit: bool,
) -> egui::Rect {
    ui.horizontal_top(|ui| {
        ui.label(egui::RichText::new(marker).strong());
        ui.vertical(|ui| {
            render_markdown_lines(ui, text, search_query, active_hit, None);
        })
        .response
        .rect
    })
    .inner
}

/// Build one styled paragraph instead of one widget for every Markdown fragment.
/// egui's horizontal wrapping can wrap *between* widgets but cannot keep a
/// long URL/inline-code widget inside the enclosing message bubble. A single
/// LayoutJob preserves emphasis AND lets the text shaper break long words at
/// the maximum available width. Keep the durable source unchanged.
fn markdown_layout_job(
    text: &str,
    search_query: &str,
    active_hit: bool,
    heading_level: Option<u8>,
    body_font: egui::FontId,
    has_bold_face: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for span in markdown_inline::parse(text) {
        let hits = offline_reader::search_hits(&span.text, search_query);
        let mut pieces = Vec::new();
        if hits.is_empty() {
            pieces.push((span.text.as_str(), false));
        } else {
            let mut at = 0;
            for (start, end) in hits {
                if start > at {
                    pieces.push((&span.text[at..start], false));
                }
                pieces.push((&span.text[start..end], true));
                at = end;
            }
            if at < span.text.len() {
                pieces.push((&span.text[at..], false));
            }
        }
        for (fragment, highlighted) in pieces {
            let mut font_id = body_font.clone();
            let mut color = egui::Color32::from_rgb(232, 234, 239);
            if span.code {
                font_id.family = egui::FontFamily::Monospace;
            }
            if let Some(level) = heading_level {
                font_id.size = match level {
                    1 => 22.0,
                    2 => 19.0,
                    3 => 17.0,
                    _ => 15.0,
                };
            }
            if span.bold || heading_level.is_some() {
                color = egui::Color32::WHITE;
                if !span.code && has_bold_face {
                    font_id.family =
                        egui::FontFamily::Name(system_fonts::MARKDOWN_BOLD_FAMILY.into());
                } else if !span.code && heading_level.is_none() {
                    font_id.size = 15.0;
                }
            }
            let mut format = egui::TextFormat {
                font_id,
                color,
                italics: span.italic,
                ..Default::default()
            };
            if span.strike {
                format.strikethrough = egui::Stroke::new(1.0, color);
            }
            if span.code {
                format.background = egui::Color32::from_rgb(20, 23, 30);
            }
            if highlighted {
                format.background = if active_hit {
                    egui::Color32::from_rgb(161, 120, 43)
                } else {
                    egui::Color32::from_rgb(91, 78, 38)
                };
            }
            job.append(fragment, 0.0, format);
        }
    }
    job
}

/// Render authored inline Markdown as a single width-constrained paragraph.
/// Code fences remain independently horizontally scrollable.
fn render_markdown_lines(
    ui: &mut egui::Ui,
    text: &str,
    search_query: &str,
    active_hit: bool,
    heading_level: Option<u8>,
) {
    let has_bold_face = system_fonts::markdown_bold_available(ui.ctx());
    let body_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(14.0));
    for line in text.split('\n') {
        let job = markdown_layout_job(
            line,
            search_query,
            active_hit,
            heading_level,
            body_font.clone(),
            has_bold_face,
        );
        // One galley uses the actual remaining bubble width. In particular,
        // a URL with no spaces is broken inside the card, never painted beyond
        // the message stroke or over adjacent UI.
        ui.add(egui::Label::new(job).wrap());
    }
}

#[cfg(test)]
mod markdown_bubble_wrap_regressions {
    use super::*;

    #[test]
    fn smithsonian_web_search_long_urls_are_one_styled_paragraph() {
        let input = "Read **Ask Smithsonian** at https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai — for the history.";
        let job = markdown_layout_job(
            input,
            "",
            false,
            None,
            egui::FontId::proportional(14.0),
            true,
        );
        assert_eq!(
            job.text,
            "Read Ask Smithsonian at https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai — for the history."
        );
        assert!(job.sections.iter().any(|section| {
            section.format.font_id.family
                == egui::FontFamily::Name(system_fonts::MARKDOWN_BOLD_FAMILY.into())
        }));
    }

    #[test]
    fn web_search_list_continuations_keep_the_inner_gutter() {
        // Regresses the real agent response: the old nested
        // horizontal_wrapped() placed URL continuation rows flush against
        // the bubble outline rather than inside the content lane.
        let context = egui::Context::default();
        let mut measured = None;
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.set_width(310.0);
                        let content_left = ui.cursor().left();
                        let content_right = content_left + ui.available_width();
                        let body = render_markdown_list_item(
                            ui,
                            "•",
                            "**Ask Smithsonian: Are Cats Domesticated?** — https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai",
                            "",
                            false,
                        );
                        measured = Some((content_left, content_right, body));
                    });
            });
        });
        let (left, right, body) = measured.expect("native layout ran");
        assert!(
            body.left() >= left + 8.0,
            "list continuation lost its gutter: body={} parent={left}",
            body.left()
        );
        assert!(
            body.right() <= right + 1.0,
            "list text exceeds its content lane: {} > {right}",
            body.right()
        );
    }

    #[test]
    fn narrow_web_search_paragraph_wraps_within_its_width() {
        let context = egui::Context::default();
        let mut observed = None;
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut job = markdown_layout_job(
                    "https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai",
                    "",
                    false,
                    None,
                    egui::FontId::proportional(14.0),
                    false,
                );
                job.wrap.max_width = 230.0;
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                observed = Some((galley.rows.len(), galley.size().x));
            });
        });
        let (rows, width) = observed.expect("layout ran");
        assert!(rows > 1, "unbroken URL must wrap over multiple rows");
        assert!(width <= 231.0, "URL escaped the bubble: {width}");
    }
}

fn render_reader_lines(
    ui: &mut egui::Ui,
    text: &str,
    search_query: &str,
    monospace: bool,
    active_hit: bool,
) {
    // Count once, not once per rendered line: the latter is quadratic
    // for large multiline pasted messages and code blocks.
    let line_count = text.lines().count();
    for (line_index, line) in text.split('\n').enumerate() {
        ui.horizontal_wrapped(|ui| {
            for (segment, inline_code) in offline_reader::inline_segments(line) {
                let segments = offline_reader::search_hits(&segment, search_query);
                if segments.is_empty() {
                    let mut rich = egui::RichText::new(segment);
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                    continue;
                }
                let mut cursor = 0;
                for (start, end) in segments {
                    if start > cursor {
                        let mut rich = egui::RichText::new(segment[cursor..start].to_owned());
                        if monospace || inline_code {
                            rich = rich.monospace();
                        }
                        ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                    }
                    let mut rich = egui::RichText::new(segment[start..end].to_owned())
                        .background_color(if active_hit {
                            egui::Color32::from_rgb(161, 120, 43)
                        } else {
                            egui::Color32::from_rgb(91, 78, 38)
                        });
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(255, 244, 190)));
                    cursor = end;
                }
                if cursor < segment.len() {
                    let mut rich = egui::RichText::new(segment[cursor..].to_owned());
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                }
            }
        });
        if line_index + 1 < line_count {
            ui.add_space(2.0);
        }
    }
}

fn adjust_reader_scroll(ctx: &egui::Context, id: egui::Id, delta: f32) {
    let mut state = egui::scroll_area::State::load(ctx, id).unwrap_or_default();
    state.offset.y = (state.offset.y + delta).max(0.0);
    state.store(ctx, id);
}

fn set_reader_scroll(ctx: &egui::Context, id: egui::Id, offset: f32) {
    let mut state = egui::scroll_area::State::load(ctx, id).unwrap_or_default();
    state.offset.y = offset.max(0.0);
    state.store(ctx, id);
}

fn should_run_fresh_tab_history_recovery(
    primary_current_pass_items: usize,
    durable_catalog_items: usize,
) -> bool {
    primary_current_pass_items == 0 && durable_catalog_items == 0
}

fn history_observed_label(observed: usize, proven_total: Option<u64>) -> String {
    match proven_total {
        Some(total) => format!("CHATGPT HISTORY · {observed}/{total}"),
        None => format!("CHATGPT HISTORY · {observed} OBSERVED"),
    }
}

fn remote_history_entry_state_label(
    mirrored: bool,
    partial: bool,
    imported: bool,
    pending: bool,
    rate_limited: bool,
    failed: bool,
) -> &'static str {
    if mirrored {
        if partial {
            "remote · mirrored locally · partial"
        } else {
            "remote · fully mirrored locally"
        }
    } else if imported {
        "remote · historical backup available"
    } else if pending {
        "remote · mirroring…"
    } else if rate_limited {
        "remote · rate limited · cooling down"
    } else if failed {
        "remote · mirror failed · click to retry"
    } else {
        "remote · discovered · click to mirror"
    }
}

fn status_row(ui: &mut egui::Ui, label: &str, value: &str, healthy: bool) {
    ui.horizontal(|ui| {
        let dot = if healthy {
            egui::Color32::from_rgb(102, 190, 132)
        } else {
            egui::Color32::from_rgb(153, 157, 166)
        };
        ui.colored_label(dot, "●");
        ui.label(
            egui::RichText::new(label)
                .size(11.0)
                .color(egui::Color32::from_rgb(186, 189, 197)),
        );
    });
    ui.add(
        egui::Label::new(
            egui::RichText::new(value)
                .size(10.0)
                .color(egui::Color32::from_rgb(126, 130, 139)),
        )
        .wrap(),
    );
    ui.add_space(5.0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryListSemanticVerdict {
    Valid,
    UnconfirmedZero,
    Contradiction,
}

fn classify_history_list_semantics(
    remote_total: u64,
    local_history_exists: bool,
) -> HistoryListSemanticVerdict {
    if remote_total > 0 {
        HistoryListSemanticVerdict::Valid
    } else if local_history_exists {
        HistoryListSemanticVerdict::Contradiction
    } else {
        HistoryListSemanticVerdict::UnconfirmedZero
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn browser_proof_label(proof: &account_bridge::BrowserProof) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · MAIN={} · debugger={} · network={} · capture-tab={} · navigation={} · exact-response={} · account-context={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.main_world_execution),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.capture_tab_created),
        yes_no(proof.navigation_started),
        yes_no(proof.exact_response_seen),
        yes_no(proof.account_context),
        proof.request_profile,
    )
}

fn history_discovery_proof_label(proof: &account_bridge::HistoryDiscoveryProof) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · debugger={} · network={} · auto-reload={} · account-context={} · responses={} · backend-200={} · json-candidates={} · body-read-failures={} · body-too-large={} · invalid-json={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.reload_started),
        yes_no(proof.account_context),
        proof.responses_seen,
        proof.backend_http_200_seen,
        proof.json_candidates_seen,
        proof.body_read_failures,
        proof.body_too_large,
        proof.invalid_json,
        proof.request_profile,
    )
}

fn fresh_tab_history_discovery_proof_label(
    proof: &account_bridge::FreshTabHistoryDiscoveryProof,
) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · temp-tab={} · debugger={} · network={} · navigate={} · account-context={} · responses={} · backend-200={} · json-candidates={} · body-read-failures={} · body-too-large={} · invalid-json={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.capture_tab_created),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.navigation_started),
        yes_no(proof.account_context),
        proof.responses_seen,
        proof.backend_http_200_seen,
        proof.json_candidates_seen,
        proof.body_read_failures,
        proof.body_too_large,
        proof.invalid_json,
        proof.request_profile,
    )
}

fn history_probe_failure_status(error: &account_bridge::BrowserBridgeError) -> String {
    match error {
        account_bridge::BrowserBridgeError::Timeout => {
            "listener ready · Edge extension did not complete a typed roundtrip before timeout"
                .to_owned()
        }
        _ => format!("listener ready · extension proof failed: {error}"),
    }
}

impl Drop for ChatariumApp {
    fn drop(&mut self) {
        if let Some(sender) = self.mirror_controller_tx.take() {
            let _ = sender.send(MirrorControllerCommand::Shutdown);
        }
        if let Some(worker) = self.mirror_controller_worker.take() {
            let _ = worker.join();
        }
        if let Some(sender) = self.persist_tx.take() {
            if let Some(active) = self.active_remote_turn.take() {
                let payload = remote_turn_payload(
                    active.turn_id,
                    &active.request_id,
                    None,
                    (!active.cumulative_text.is_empty()).then_some(active.cumulative_text.as_str()),
                    Some("Chatarium closed while the remote turn was active"),
                );
                let _ = sender.send(PersistCommand::AppendTurnEvent {
                    turn_id: active.turn_id,
                    kind: EventKind::TransportInterrupted,
                    payload,
                });
            } else if let Some(pending) = self.pending_remote_turn.take() {
                let payload = remote_turn_payload(
                    pending.turn_id,
                    &pending.request_id,
                    Some(&pending.model),
                    None,
                    Some("Chatarium closed after dispatch evidence but before provider outcome"),
                );
                let _ = sender.send(PersistCommand::AppendTurnEvent {
                    turn_id: pending.turn_id,
                    kind: EventKind::TransportInterrupted,
                    payload,
                });
            }
            let _ = sender.send(PersistCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn append_local_orchestration_topology_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    container_id: ChatContainerId,
    root_session_id: SessionId,
) -> Result<Vec<EventEnvelope>, String> {
    let bindings = replay_local_conversation_chat_container_bindings(store.events())?;
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.conversation_id == conversation_id)
    {
        return Err(format!(
            "local conversation {conversation_id} already owns chat container {}",
            existing.container_id.get()
        ));
    }
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.container_id == container_id)
    {
        return Err(format!(
            "chat container {} already belongs to local conversation {}",
            container_id.get(),
            existing.conversation_id
        ));
    }
    if replay_session_audit(store.events())?
        .iter()
        .any(|record| record.session_id == root_session_id)
    {
        return Err(format!(
            "local session {} is already registered",
            root_session_id.get()
        ));
    }
    if replay_chat_container_audit(store.events())?
        .iter()
        .any(|record| record.container_id == container_id)
    {
        return Err(format!(
            "chat container {} already exists",
            container_id.get()
        ));
    }

    let before = store.events().len();
    record_local_session_registered(store, root_session_id).map_err(|error| error.to_string())?;
    record_chat_container_created(store, container_id, root_session_id)
        .map_err(|error| error.to_string())?;
    record_local_conversation_chat_container_bound(store, conversation_id, container_id)
        .map_err(|error| error.to_string())?;

    let topology = replay_local_conversation_topologies(store.events())?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id)
        .ok_or_else(|| {
            "local orchestration topology append did not replay for its conversation".to_owned()
        })?;
    if topology.container_id != container_id || topology.current_session_id != root_session_id {
        return Err(
            "local orchestration topology replay disagrees with appended identities".to_owned(),
        );
    }

    Ok(store.events()[before..].to_vec())
}

fn append_current_session_route_endpoint_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    session_id: SessionId,
    endpoint_id: RouteEndpointId,
) -> Result<EventEnvelope, String> {
    let topology = replay_local_conversation_topologies(store.events())?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id)
        .ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    if topology.current_session_id != session_id {
        return Err(format!(
            "session {} is not current for local conversation {conversation_id}; current session is {}",
            session_id.get(),
            topology.current_session_id.get()
        ));
    }

    let sessions = replay_session_audit(store.events())?;
    let session = sessions
        .iter()
        .find(|record| record.session_id == session_id)
        .ok_or_else(|| format!("current session {} is not registered", session_id.get()))?;
    if let Some(existing) = session.endpoint_binding {
        return Err(format!(
            "current session {} is already bound to routing endpoint {}",
            session_id.get(),
            existing.endpoint_id().get()
        ));
    }
    if let Some(existing) = sessions.iter().find_map(|record| {
        record
            .endpoint_binding
            .filter(|binding| binding.endpoint_id() == endpoint_id)
            .map(|binding| binding.session_id())
    }) {
        return Err(format!(
            "routing endpoint {} is already bound to session {}",
            endpoint_id.get(),
            existing.get()
        ));
    }

    if let Some(provider) = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| {
            provider
                .endpoint_binding
                .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
        })
    {
        return Err(format!(
            "routing endpoint {} is already bound to tool provider {}",
            endpoint_id.get(),
            provider.provider_id.get()
        ));
    }

    if replay_routing_audit(store.events())?.iter().any(|route| {
        route.request.source == endpoint_id || route.request.destination == endpoint_id
    }) {
        return Err(format!(
            "routing endpoint {} already appears in durable route history and cannot be claimed by a new session",
            endpoint_id.get()
        ));
    }

    record_session_endpoint_bound(store, SessionEndpointBinding::new(session_id, endpoint_id))
        .map_err(|error| error.to_string())?;

    let replayed = replay_session_audit(store.events())?
        .into_iter()
        .find(|record| record.session_id == session_id)
        .and_then(|record| record.endpoint_binding)
        .ok_or_else(|| {
            "session endpoint append did not replay for the current session".to_owned()
        })?;
    if replayed.endpoint_id() != endpoint_id {
        return Err("session endpoint replay disagrees with appended endpoint".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "session endpoint append produced no durable event".to_owned())
}

fn append_local_session_route_proposal_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    source_conversation_id: LocalConversationId,
    destination_conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    if source_conversation_id == destination_conversation_id {
        return Err("a local conversation cannot route a session message to itself".to_owned());
    }
    replay_local_route_payload_audit(store.events())?;
    if replay_routing_audit(store.events())?
        .iter()
        .any(|route| route.request.id == route_id)
    {
        return Err(format!("route {} already exists", route_id.get()));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.conversation_id == source_conversation_id)
        .ok_or_else(|| {
            format!(
                "source local conversation {source_conversation_id} is not currently addressable"
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.conversation_id == destination_conversation_id)
        .ok_or_else(|| {
            format!(
                "destination local conversation {destination_conversation_id} is not currently addressable"
            )
        })?;

    if !source.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "source conversation {source_conversation_id} current session {} is {} and cannot accept ordinary session-message routing",
            source.current_session_id.get(),
            session_lifecycle_phase_label(source.current_session_phase),
        ));
    }
    if !destination.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "destination conversation {destination_conversation_id} current session {} is {} and cannot accept ordinary session-message routing",
            destination.current_session_id.get(),
            session_lifecycle_phase_label(destination.current_session_phase),
        ));
    }

    let request = RouteRequest {
        id: route_id,
        source: source.endpoint_id,
        destination: destination.endpoint_id,
        class: RouteClass::SessionMessage,
    };
    record_route_proposed(store, request, RoutePolicy::RequireApproval)
        .map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| "local route proposal append did not replay".to_owned())?;
    if replayed.request != request
        || replayed.initial_policy != RoutePolicy::RequireApproval
        || replayed.gate_state != RouteGateState::PendingApproval
    {
        return Err("local route proposal replay disagrees with appended policy".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route proposal append produced no durable event".to_owned())
}

fn append_local_route_payload_checked(
    store: &mut impl EventStore,
    payload_id: RoutePayloadId,
    route_id: RouteId,
    text: String,
) -> Result<EventEnvelope, String> {
    if text.trim().is_empty() {
        return Err("route payload text cannot be empty".to_owned());
    }

    let payloads = replay_local_route_payload_audit(store.events())?;
    if let Some(existing) = payloads.iter().find(|payload| payload.route_id == route_id) {
        return Err(format!(
            "route {} already has immutable payload {}",
            route_id.get(),
            existing.payload_id.get()
        ));
    }
    if let Some(existing) = payloads
        .iter()
        .find(|payload| payload.payload_id == payload_id)
    {
        return Err(format!(
            "route payload identity {} already belongs to route {}",
            payload_id.get(),
            existing.route_id.get()
        ));
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;
    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "route {} has already dispatched and cannot acquire a new payload",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;

    if source.conversation_id == destination.conversation_id {
        return Err(format!(
            "route {} resolves to the same local conversation on both ends",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} is stale because one of its current local session leaves cannot accept ordinary turns",
            route_id.get()
        ));
    }

    record_local_route_payload_attached(
        store,
        payload_id,
        route_id,
        source.conversation_id,
        destination.conversation_id,
        text.clone(),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_local_route_payload_audit(store.events())?
        .into_iter()
        .find(|payload| payload.route_id == route_id)
        .ok_or_else(|| "local route payload append did not replay".to_owned())?;
    if replayed.payload_id != payload_id
        || replayed.source_conversation_id != source.conversation_id
        || replayed.destination_conversation_id != destination.conversation_id
        || replayed.text != text
    {
        return Err("local route payload replay disagrees with appended payload".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route payload append produced no durable event".to_owned())
}

fn append_local_route_user_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let payloads = replay_local_route_payload_audit(store.events())?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "route {} has already dispatched and its policy history is immutable",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision),
        ));
    }
    if decision == RouteUserDecision::Allow
        && !payloads.iter().any(|payload| payload.route_id == route_id)
    {
        return Err(format!(
            "route {} cannot be allowed before an immutable payload is durably attached",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer a current local conversation leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;

    if source.conversation_id == destination.conversation_id {
        return Err(format!(
            "route {} resolves to the same local conversation on both ends",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} is stale because one of its current local session leaves cannot accept ordinary turns",
            route_id.get()
        ));
    }

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "local route decision append did not replay".to_owned())?;
    let expected_state = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.latest_user_decision != Some(decision) || replayed.gate_state != expected_state {
        return Err("local route decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local route decision append produced no durable event".to_owned())
}

fn route_gate_before_dispatch(route: &RouteAuditRecord) -> Result<RouteGate, String> {
    let mut gate = RouteGate::new(route.request, route.initial_policy);
    if let Some(decision) = route.latest_user_decision {
        match decision {
            RouteUserDecision::Allow => gate.user_allow(),
            RouteUserDecision::Deny => gate.user_deny(),
        }
        .map_err(|error| {
            format!(
                "cannot reconstruct route {} gate: {error:?}",
                route.request.id.get()
            )
        })?;
    }
    if gate.state() != route.gate_state {
        return Err(format!(
            "route {} gate reconstruction disagrees with durable routing audit",
            route.request.id.get()
        ));
    }
    Ok(gate)
}

fn append_local_route_dispatch_and_delivery_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let deliveries = replay_local_route_delivery_audit(store.events())?;
    if let Some(existing) = deliveries
        .iter()
        .find(|delivery| delivery.route_id == route_id)
    {
        return Err(format!(
            "local route {} is already delivered at event #{}",
            route_id.get(),
            existing.delivered_sequence
        ));
    }

    let payload = replay_local_route_payload_audit(store.events())?
        .into_iter()
        .find(|payload| payload.route_id == route_id)
        .ok_or_else(|| {
            format!(
                "local route {} cannot dispatch before an immutable payload is attached",
                route_id.get()
            )
        })?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("local route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::SessionMessage {
        return Err(format!(
            "route {} is not a local session-message route",
            route_id.get()
        ));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.source)
        .ok_or_else(|| {
            format!(
                "route {} source endpoint {} is no longer the current local leaf",
                route_id.get(),
                route.request.source.get()
            )
        })?;
    let destination = directory
        .iter()
        .find(|entry| entry.endpoint_id == route.request.destination)
        .ok_or_else(|| {
            format!(
                "route {} destination endpoint {} is no longer the current local leaf",
                route_id.get(),
                route.request.destination.get()
            )
        })?;
    if source.conversation_id != payload.source_conversation_id
        || destination.conversation_id != payload.destination_conversation_id
    {
        return Err(format!(
            "route {} current endpoint ownership disagrees with immutable payload provenance",
            route_id.get()
        ));
    }
    if !source.current_session_phase.accepts_ordinary_turns()
        || !destination.current_session_phase.accepts_ordinary_turns()
    {
        return Err(format!(
            "route {} cannot deliver because one current session leaf no longer accepts ordinary turns",
            route_id.get()
        ));
    }

    let before = store.events().len();
    let dispatch_sequence = match route.gate_state {
        RouteGateState::Allowed { .. } => {
            let mut gate = route_gate_before_dispatch(&route)?;
            let permit = gate.authorize_dispatch(route_id).map_err(|error| {
                format!("route {} dispatch gate rejected: {error:?}", route_id.get())
            })?;
            record_route_dispatched(store, permit).map_err(|error| error.to_string())?
        }
        RouteGateState::Dispatched { .. } => route.dispatch_sequence.ok_or_else(|| {
            format!(
                "route {} is marked dispatched without a durable dispatch sequence",
                route_id.get()
            )
        })?,
        RouteGateState::PendingApproval => {
            return Err(format!(
                "route {} is still awaiting explicit user approval",
                route_id.get()
            ));
        }
        RouteGateState::Denied { .. } => {
            return Err(format!(
                "route {} is denied and cannot dispatch",
                route_id.get()
            ));
        }
    };

    let dispatched = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "local route disappeared after dispatch append".to_owned())?;
    if !dispatched.gate_state.is_dispatched()
        || dispatched.dispatch_sequence != Some(dispatch_sequence)
    {
        return Err("local route dispatch replay disagrees with durable dispatch".to_owned());
    }

    record_local_route_delivered(
        store,
        route_id,
        payload.payload_id,
        payload.source_conversation_id,
        payload.destination_conversation_id,
        source.current_session_id,
        destination.current_session_id,
        dispatch_sequence,
    )
    .map_err(|error| error.to_string())?;

    let delivery = replay_local_route_delivery_audit(store.events())?
        .into_iter()
        .find(|delivery| delivery.route_id == route_id)
        .ok_or_else(|| "local route delivery append did not replay".to_owned())?;
    if delivery.payload_id != payload.payload_id
        || delivery.source_conversation_id != payload.source_conversation_id
        || delivery.destination_conversation_id != payload.destination_conversation_id
        || delivery.source_session_id != source.current_session_id
        || delivery.destination_session_id != destination.current_session_id
        || delivery.dispatch_sequence != dispatch_sequence
    {
        return Err("local route delivery replay disagrees with appended delivery".to_owned());
    }

    Ok(store.events()[before..].to_vec())
}

fn append_local_route_context_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    destination_conversation_id: LocalConversationId,
    decision: LocalRouteContextDecision,
) -> Result<EventEnvelope, String> {
    let inbox_item =
        replay_local_routed_inbox_for_conversation(store.events(), destination_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "local route {} is not a delivered routed inbox item for conversation {}",
                    route_id.get(),
                    destination_conversation_id,
                )
            })?;

    if let Some(existing) = replay_local_route_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        if existing.destination_conversation_id != destination_conversation_id
            || existing.payload_id != inbox_item.payload_id
        {
            return Err(format!(
                "local route {} context provenance conflicts with delivered inbox item",
                route_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "local route {} already has routed context decision {}",
                route_id.get(),
                local_route_context_decision_label(decision),
            ));
        }
    }

    record_local_route_context_decision(
        store,
        route_id,
        inbox_item.payload_id,
        destination_conversation_id,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_local_route_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "routed context decision append did not replay".to_owned())?;
    if replayed.payload_id != inbox_item.payload_id
        || replayed.destination_conversation_id != destination_conversation_id
        || replayed.decision != decision
    {
        return Err("routed context decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "routed context decision append produced no durable event".to_owned())
}

fn append_tool_provider_registration_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    name: ToolProviderName,
) -> Result<EventEnvelope, String> {
    if replay_tool_provider_audit(store.events())?
        .iter()
        .any(|record| record.provider_id == provider_id)
    {
        return Err(format!(
            "tool provider {} already exists",
            provider_id.get()
        ));
    }

    record_tool_provider_registered(store, provider_id, &name)
        .map_err(|error| error.to_string())?;

    let replayed = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| "tool provider registration did not replay".to_owned())?;
    if replayed.name != name || replayed.endpoint_binding.is_some() {
        return Err("tool provider replay disagrees with registration".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool provider registration produced no durable event".to_owned())
}

fn append_tool_provider_endpoint_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    endpoint_id: RouteEndpointId,
) -> Result<EventEnvelope, String> {
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| format!("tool provider {} does not exist", provider_id.get()))?;
    if let Some(existing) = provider.endpoint_binding {
        return Err(format!(
            "tool provider {} is already bound to routing endpoint {}",
            provider_id.get(),
            existing.endpoint_id().get()
        ));
    }

    if replay_session_audit(store.events())?.iter().any(|record| {
        record
            .endpoint_binding
            .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
    }) {
        return Err(format!(
            "routing endpoint {} is already bound to a local session",
            endpoint_id.get()
        ));
    }
    if replay_tool_provider_audit(store.events())?
        .iter()
        .any(|record| {
            record
                .endpoint_binding
                .is_some_and(|binding| binding.endpoint_id() == endpoint_id)
        })
    {
        return Err(format!(
            "routing endpoint {} is already bound to another tool provider",
            endpoint_id.get()
        ));
    }
    if replay_routing_audit(store.events())?.iter().any(|route| {
        route.request.source == endpoint_id || route.request.destination == endpoint_id
    }) {
        return Err(format!(
            "routing endpoint {} already appears in durable route history",
            endpoint_id.get()
        ));
    }

    record_tool_provider_endpoint_bound(
        store,
        ToolProviderEndpointBinding::new(provider_id, endpoint_id),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .and_then(|record| record.endpoint_binding)
        .ok_or_else(|| "tool provider endpoint binding did not replay".to_owned())?;
    if replayed.endpoint_id() != endpoint_id {
        return Err("tool provider endpoint replay disagrees with appended endpoint".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool provider endpoint binding produced no durable event".to_owned())
}

fn append_tool_call_proposal_checked(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
    source_conversation_id: LocalConversationId,
    provider_id: ToolProviderId,
    operation: ToolOperationName,
    arguments_text: String,
    policy: RoutePolicy,
) -> Result<Vec<EventEnvelope>, String> {
    if policy == RoutePolicy::Deny {
        return Err("disabled tool cannot be proposed for execution".to_owned());
    }
    if arguments_text.trim_start().starts_with("<tool_call") {
        let parsed = parse_legacy_tool_call(&arguments_text)
            .map_err(|error| format!("invalid legacy tool envelope: {error:?}"))?;
        if parsed.name != operation.as_str() {
            return Err(format!(
                "legacy envelope operation {} disagrees with selected operation {}",
                parsed.name,
                operation.as_str(),
            ));
        }
    }
    if replay_tool_call_audit(store.events())?
        .iter()
        .any(|record| record.call_id == call_id)
    {
        return Err(format!("tool call {} already exists", call_id.get()));
    }
    if replay_routing_audit(store.events())?
        .iter()
        .any(|route| route.request.id == route_id)
    {
        return Err(format!("route {} already exists", route_id.get()));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let source = directory
        .iter()
        .find(|entry| entry.conversation_id == source_conversation_id)
        .ok_or_else(|| {
            format!(
                "source local conversation {source_conversation_id} is not currently addressable"
            )
        })?;
    if !source.current_session_phase.accepts_ordinary_turns() {
        return Err(format!(
            "source conversation {source_conversation_id} current session {} is {} and cannot originate a tool call",
            source.current_session_id.get(),
            session_lifecycle_phase_label(source.current_session_phase),
        ));
    }

    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.provider_id == provider_id)
        .ok_or_else(|| format!("tool provider {} does not exist", provider_id.get()))?;
    let provider_endpoint = provider.endpoint_binding.ok_or_else(|| {
        format!(
            "tool provider {} is not routing-addressable",
            provider_id.get()
        )
    })?;

    let before = store.events().len();
    record_tool_call(
        store,
        call_id,
        source.current_session_id,
        provider_id,
        &operation,
        arguments_text.clone(),
    )
    .map_err(|error| error.to_string())?;

    let request = RouteRequest {
        id: route_id,
        source: source.endpoint_id,
        destination: provider_endpoint.endpoint_id(),
        class: RouteClass::ToolCall,
    };
    record_route_proposed(store, request, policy).map_err(|error| error.to_string())?;
    record_tool_call_route_bound(store, call_id, route_id).map_err(|error| error.to_string())?;

    let replayed = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|record| record.call_id == call_id)
        .ok_or_else(|| "tool call proposal did not replay".to_owned())?;
    if replayed.source_session_id != source.current_session_id
        || replayed.provider_id != provider_id
        || replayed.operation != operation
        || replayed.arguments_text != arguments_text
        || replayed.route_id != Some(route_id)
    {
        return Err("tool call replay disagrees with appended proposal".to_owned());
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| "tool call route proposal did not replay".to_owned())?;
    let expected_gate = match policy {
        RoutePolicy::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::Policy,
        },
        RoutePolicy::RequireApproval => RouteGateState::PendingApproval,
        RoutePolicy::Deny => unreachable!(),
    };
    if route.request != request
        || route.initial_policy != policy
        || route.gate_state != expected_gate
    {
        return Err("tool call route replay disagrees with recorded policy".to_owned());
    }

    Ok(store.events()[before..].to_vec())
}

/// Prepare only addressable, checked, journaled routing identities for a
/// model-authored MCP request. The model never chooses a provider ID, route
/// ID, executable, policy authority or a dispatch permit.
fn append_agent_mcp_call_checked(
    store: &mut impl EventStore,
    source_conversation_id: LocalConversationId,
    proposal: &mcp_agent_defaults::ModelToolRequest,
    policy: mcp_agent_defaults::ToolPolicy,
) -> Result<(ToolCallId, RouteId, Vec<EventEnvelope>), String> {
    if !local_http_mcp::allowed_model_operation(&proposal.provider, &proposal.tool)
        && proposal.provider == local_http_mcp::OPENVIKING
        && proposal.tool == "write"
    {
        // Reject before creating a route, even for an old saved Allow policy.
        // The host-owned outbox remains the only checkpoint write authority.
        return Err(
            "OpenViking writes use Chatarium's automatic verified checkpoint lifecycle".to_owned(),
        );
    }
    if matches!(
        policy,
        mcp_agent_defaults::ToolPolicy::Disabled | mcp_agent_defaults::ToolPolicy::Manual
    ) {
        return Err("this MCP tool cannot be invoked automatically by the model".to_owned());
    }
    let starting = store.events().len();
    if local_conversation_topology(store.events(), source_conversation_id)?.is_none() {
        let (container, session) = next_available_local_orchestration_ids(store.events())?;
        append_local_orchestration_topology_checked(
            store,
            source_conversation_id,
            container,
            session,
        )?;
    }
    let topology = local_conversation_topology(store.events(), source_conversation_id)?
        .ok_or("agent MCP source has no local topology")?;
    if session_endpoint_binding(store.events(), topology.current_session_id)?.is_none() {
        let endpoint = next_available_route_endpoint_id(store.events())?;
        append_current_session_route_endpoint_checked(
            store,
            source_conversation_id,
            topology.current_session_id,
            endpoint,
        )?;
    }

    let provider_name = ToolProviderName::new(proposal.provider.clone())
        .map_err(|error| format!("agent MCP provider name: {error:?}"))?;
    let mut registered = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|record| record.name == provider_name);
    if registered.is_none()
        && (proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME
            || local_http_mcp::known_provider(&proposal.provider))
    {
        let provider_id = next_available_tool_provider_id(store.events())?;
        append_tool_provider_registration_checked(store, provider_id, provider_name.clone())?;
        let endpoint = next_available_route_endpoint_id(store.events())?;
        append_tool_provider_endpoint_checked(store, provider_id, endpoint)?;
        registered = replay_tool_provider_audit(store.events())?
            .into_iter()
            .find(|record| record.provider_id == provider_id);
    }
    let provider = registered.ok_or("agent requested an unregistered MCP provider")?;
    if provider.endpoint_binding.is_none() {
        return Err("agent MCP provider has no bound routing endpoint".to_owned());
    }
    if proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME {
        if !local_tool_adapter::supports_builtin(&proposal.provider, &proposal.tool) {
            return Err("unrecognized builtin MCP operation".to_owned());
        }
    } else if local_http_mcp::known_provider(&proposal.provider) {
        if !local_http_mcp::allowed_operation(&proposal.provider, &proposal.tool) {
            return Err("unrecognized fixed localhost MCP operation".to_owned());
        }
    } else {
        let config = replay_tool_transport_config_audit(store.events())?
            .into_iter()
            .find(|record| record.provider_id == provider.provider_id)
            .ok_or("agent MCP external provider has no immutable stdio configuration")?;
        if !config
            .config
            .allowed_operations()
            .iter()
            .any(|op| op.as_str() == proposal.tool)
        {
            return Err("agent MCP operation is outside the provider allowlist".to_owned());
        }
        if policy == mcp_agent_defaults::ToolPolicy::Automatic
            && !replay_tool_provider_activation_audit(store.events())?
                .into_iter()
                .any(|record| record.provider_id == provider.provider_id && record.active)
        {
            return Err("agent cannot automatically call an inactive MCP provider".to_owned());
        }
    }
    let call_id = next_available_tool_call_id(store.events())?;
    let route_id = next_available_route_id(store.events())?;
    let operation = ToolOperationName::new(proposal.tool.clone())
        .map_err(|error| format!("agent MCP tool name: {error:?}"))?;
    let route_policy = match policy {
        mcp_agent_defaults::ToolPolicy::Automatic => RoutePolicy::Allow,
        mcp_agent_defaults::ToolPolicy::Ask => RoutePolicy::RequireApproval,
        _ => unreachable!(),
    };
    append_tool_call_proposal_checked(
        store,
        call_id,
        route_id,
        source_conversation_id,
        provider.provider_id,
        operation,
        proposal.arguments.to_string(),
        route_policy,
    )?;
    Ok((call_id, route_id, store.events()[starting..].to_vec()))
}

fn append_tool_call_route_user_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == Some(route_id))
        .ok_or_else(|| format!("route {} is not bound to a tool call", route_id.get()))?;
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;

    if route.request.class != RouteClass::ToolCall {
        return Err(format!("route {} is not a tool-call route", route_id.get()));
    }
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "tool route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "tool route {} has already dispatched and its policy history is immutable",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "tool route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision),
        ));
    }
    if call.route_bound_sequence.is_none() {
        return Err(format!(
            "tool call {} has no durable route correlation",
            call.call_id.get()
        ));
    }

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.request.id == route_id)
        .ok_or_else(|| "tool route decision append did not replay".to_owned())?;
    let expected_state = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.latest_user_decision != Some(decision) || replayed.gate_state != expected_state {
        return Err("tool route decision replay disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool route decision append produced no durable event".to_owned())
}

fn append_builtin_hello_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "route {} is not bound to a durable tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or_else(|| {
            format!(
                "tool call {} references missing provider {}",
                call.call_id.get(),
                call.provider_id.get()
            )
        })?;
    if !local_tool_adapter::supports_builtin(provider.name.as_str(), call.operation.as_str()) {
        return Err(format!(
            "tool call {} is not supported by the restricted local builtin adapter",
            call.call_id.get()
        ));
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                })
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                })
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(format!(
            "tool route {} requires a fresh explicit user Allow or saved exact policy before builtin execution",
            route_id.get()
        ));
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err(format!(
            "tool route {} already has a durable terminal outcome",
            route_id.get()
        ));
    }

    let source_fresh = replay_local_routing_directory(store.events())?
        .into_iter()
        .any(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        });
    if !source_fresh {
        return Err(format!(
            "tool route {} source session is no longer the current addressable local leaf",
            route_id.get()
        ));
    }
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err(format!(
            "tool route {} destination no longer matches the registered provider endpoint",
            route_id.get()
        ));
    }

    // Prepare and validate the pure hello output before consuming any permit.
    // This function performs no filesystem, process, network, or tool side effects.
    let result_text = local_tool_adapter::prepare_builtin_response(&call, store.events())?;
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("tool route {} dispatch denied: {error:?}", route_id.get()))?;

    let before = store.events().len();
    record_route_dispatched(store, permit).map_err(|error| error.to_string())?;
    // A crash after durable dispatch and before outcome remains unresolved.
    // Never automatically re-run a possibly state-changing adapter.
    append_tool_call_outcome_checked(
        store,
        call.call_id,
        route_id,
        ToolCallOutcomeKind::Result,
        result_text,
    )?;
    Ok(store.events()[before..].to_vec())
}

/// Reserve exactly one already authorized localhost HTTP MCP invocation.
/// The route is durably dispatched before any network access, with no retry
/// after uncertainty. This is distinct from confined stdio configuration.
fn reserve_local_http_mcp_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<(ToolCallId, String, String, String, Value, EventEnvelope), String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "localhost MCP route {} has no recorded tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or("localhost MCP provider missing")?;
    let name = provider.name.as_str().to_owned();
    let operation = call.operation.as_str().to_owned();
    if !local_http_mcp::allowed_model_operation(&name, &operation) {
        return Err(
            "OpenViking model-authored checkpoint writes are blocked; host outbox only".to_owned(),
        );
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or("localhost MCP tool route missing")?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                }
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                }
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(
            "localhost MCP call needs a persisted exact standing policy or user Allow".to_owned(),
        );
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err("localhost MCP call already has a terminal observation".to_owned());
    }
    let source = replay_local_routing_directory(store.events())?
        .into_iter()
        .find(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        })
        .ok_or("localhost MCP source conversation is no longer addressable")?;
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err("localhost MCP route destination differs from provider endpoint".to_owned());
    }
    let args: Value = serde_json::from_str(&call.arguments_text)
        .map_err(|e| format!("invalid localhost MCP tool arguments: {e}"))?;
    if !args.is_object() || call.arguments_text.len() > 256 * 1024 {
        return Err("localhost MCP arguments must be a bounded JSON object".to_owned());
    }
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("localhost MCP dispatch denied: {error:?}"))?;
    record_route_dispatched(store, permit).map_err(|error| error.to_string())?;
    let event = store
        .events()
        .last()
        .cloned()
        .ok_or("localhost MCP dispatch event missing")?;
    Ok((
        call.call_id,
        source.conversation_id.to_string(),
        name,
        operation,
        args,
        event,
    ))
}

fn reserve_builtin_host_shell_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    settings: &host_shell_mcp::Settings,
) -> Result<(ToolCallId, Value, EventEnvelope), String> {
    let call = replay_tool_call_audit(store.events())?
        .into_iter()
        .find(|call| call.route_id == Some(route_id))
        .ok_or_else(|| {
            format!(
                "route {} is not bound to a durable tool call",
                route_id.get()
            )
        })?;
    let provider = replay_tool_provider_audit(store.events())?
        .into_iter()
        .find(|provider| provider.provider_id == call.provider_id)
        .ok_or_else(|| {
            format!(
                "tool call {} references missing provider {}",
                call.call_id.get(),
                call.provider_id.get()
            )
        })?;
    if provider.name.as_str() != local_tool_adapter::BUILTIN_PROVIDER_NAME
        || call.operation.as_str() != host_shell_mcp::OPERATION
    {
        return Err("route is not an authorized builtin host shell operation".to_owned());
    }
    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|route| route.request.id == route_id)
        .ok_or_else(|| format!("tool route {} does not exist", route_id.get()))?;
    let allowed = match route.initial_policy {
        RoutePolicy::Allow => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::Policy,
                })
                && route.latest_user_decision.is_none()
        }
        RoutePolicy::RequireApproval => {
            route.gate_state
                == (RouteGateState::Allowed {
                    by: DecisionAuthority::User,
                })
                && route.latest_user_decision == Some(RouteUserDecision::Allow)
        }
        RoutePolicy::Deny => false,
    };
    if route.request.class != RouteClass::ToolCall || !allowed {
        return Err(format!(
            "tool route {} requires a fresh explicit user Allow or saved exact policy before builtin execution",
            route_id.get()
        ));
    }
    if replay_tool_call_outcome_audit(store.events())?
        .iter()
        .any(|outcome| outcome.call_id == call.call_id || outcome.route_id == route_id)
    {
        return Err(format!(
            "tool route {} already has a durable terminal outcome",
            route_id.get()
        ));
    }

    let source_fresh = replay_local_routing_directory(store.events())?
        .into_iter()
        .any(|entry| {
            entry.current_session_id == call.source_session_id
                && entry.endpoint_id == route.request.source
                && entry.current_session_phase.accepts_ordinary_turns()
        });
    if !source_fresh {
        return Err(format!(
            "tool route {} source session is no longer the current addressable local leaf",
            route_id.get()
        ));
    }
    if !provider
        .endpoint_binding
        .is_some_and(|binding| binding.endpoint_id() == route.request.destination)
    {
        return Err(format!(
            "tool route {} destination no longer matches the registered provider endpoint",
            route_id.get()
        ));
    }

    // Parse and validate the complete command BEFORE consuming the route
    // permit. Execution itself occurs only on a dedicated subprocess worker.
    let args: Value = serde_json::from_str(&call.arguments_text)
        .map_err(|e| format!("invalid host shell argument object: {e}"))?;
    settings.validate_request(&args)?;
    if !settings.shell_path().is_file() || !std::path::Path::new("/usr/bin/timeout").is_file() {
        return Err("selected shell or /usr/bin/timeout is unavailable".to_owned());
    }
    let mut gate = route_gate_before_dispatch(&route)?;
    let permit = gate
        .authorize_dispatch(route_id)
        .map_err(|error| format!("host shell route denied: {error:?}"))?;
    record_route_dispatched(store, permit).map_err(|e| e.to_string())?;
    let event = store
        .events()
        .last()
        .cloned()
        .ok_or("host shell dispatch journal event missing")?;
    Ok((call.call_id, args, event))
}

fn commit_message_with_one_shot_memory_checked(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
    one_shot_memory_ids: &[LocalMemoryId],
    one_shot_memory_snapshot_after_sequence: Option<u64>,
) -> Result<(EventEnvelope, Option<EventEnvelope>), String> {
    commit_message_with_attachment_and_one_shot_memory_checked(
        store,
        message,
        None,
        one_shot_memory_ids,
        one_shot_memory_snapshot_after_sequence,
    )
}

fn commit_message_with_attachment_and_one_shot_memory_checked(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
    attachment: Option<&Value>,
    one_shot_memory_ids: &[LocalMemoryId],
    one_shot_memory_snapshot_after_sequence: Option<u64>,
) -> Result<(EventEnvelope, Option<EventEnvelope>), String> {
    match (
        one_shot_memory_ids.is_empty(),
        one_shot_memory_snapshot_after_sequence,
    ) {
        (true, None) => {}
        (true, Some(_)) => {
            return Err(
                "empty one-shot local memory set cannot carry a snapshot high-water".to_owned(),
            );
        }
        (false, None) => {
            return Err(
                "non-empty one-shot local memory set requires a snapshot high-water".to_owned(),
            );
        }
        (false, Some(snapshot_after_sequence)) => {
            validate_local_memory_one_shot_snapshot(
                store.events(),
                message.conversation_id,
                one_shot_memory_ids,
                snapshot_after_sequence,
            )?;
        }
    }

    commit_user_message_with_attachment(store, message, attachment)
        .map_err(|error| error.to_string())?;
    let commit_event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "user-message commit produced no durable event".to_owned())?;

    let one_shot_event = if let Some(snapshot_after_sequence) =
        one_shot_memory_snapshot_after_sequence
    {
        record_local_memory_turn_selection(
            store,
            message.conversation_id,
            message.turn_id,
            one_shot_memory_ids,
            snapshot_after_sequence,
        )
        .map_err(|error| error.to_string())?;

        let event = store.events().last().cloned().ok_or_else(|| {
            "one-shot local memory selection produced no durable event".to_owned()
        })?;

        let replayed = replay_local_memory_turn_selection_audit(store.events())?
            .into_iter()
            .find(|record| record.turn_id == message.turn_id)
            .ok_or_else(|| "one-shot local memory selection append did not replay".to_owned())?;
        if replayed.conversation_id != message.conversation_id
            || replayed.memory_ids != one_shot_memory_ids
            || replayed.selection_snapshot_after_sequence != snapshot_after_sequence
            || replayed.user_message_sequence != commit_event.sequence
            || replayed.recorded_sequence != event.sequence
        {
            return Err(
                "one-shot local memory selection replay disagrees with appended selection"
                    .to_owned(),
            );
        }
        Some(event)
    } else {
        None
    };

    Ok((commit_event, one_shot_event))
}

fn append_local_memory_artifact_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    source_conversation_id: LocalConversationId,
    text: String,
) -> Result<EventEnvelope, String> {
    if text.trim().is_empty() {
        return Err("local memory text cannot be empty".to_owned());
    }
    if replay_local_memory_audit(store.events())?
        .iter()
        .any(|record| record.memory_id == memory_id)
    {
        return Err(format!("local memory {} already exists", memory_id.get()));
    }

    record_local_memory_artifact(store, memory_id, source_conversation_id, text.clone())
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id)
        .ok_or_else(|| "local memory append did not replay".to_owned())?;
    if replayed.source_conversation_id != source_conversation_id || replayed.text != text {
        return Err("local memory replay disagrees with appended artifact".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory append produced no durable event".to_owned())
}

fn append_local_memory_context_decision_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    destination_conversation_id: LocalConversationId,
    decision: LocalMemoryContextDecision,
) -> Result<EventEnvelope, String> {
    let artifact = replay_local_memory_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id)
        .ok_or_else(|| format!("local memory {} does not exist", memory_id.get()))?;

    if decision == LocalMemoryContextDecision::Admit {
        let supersessions = replay_local_memory_supersession_audit(store.events())?;
        if let Some(lineage) = supersessions
            .iter()
            .find(|record| record.predecessor_memory_id == memory_id)
        {
            return Err(format!(
                "local memory {} is superseded by {} and cannot be admitted",
                memory_id.get(),
                lineage.successor_memory_id.get(),
            ));
        }
    }

    if let Some(existing) = replay_local_memory_context_audit(store.events())?
        .into_iter()
        .find(|record| {
            record.memory_id == memory_id
                && record.destination_conversation_id == destination_conversation_id
        })
    {
        if existing.source_conversation_id != artifact.source_conversation_id
            || existing.artifact_sequence != artifact.recorded_sequence
        {
            return Err(format!(
                "local memory {} context provenance conflicts with artifact",
                memory_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "local memory {} already has context decision {} for conversation {}",
                memory_id.get(),
                local_memory_context_decision_label(decision),
                destination_conversation_id,
            ));
        }
    }

    record_local_memory_context_decision(store, memory_id, destination_conversation_id, decision)
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_context_audit(store.events())?
        .into_iter()
        .find(|record| {
            record.memory_id == memory_id
                && record.destination_conversation_id == destination_conversation_id
        })
        .ok_or_else(|| "local memory context decision append did not replay".to_owned())?;
    if replayed.source_conversation_id != artifact.source_conversation_id
        || replayed.artifact_sequence != artifact.recorded_sequence
        || replayed.decision != decision
    {
        return Err(
            "local memory context decision replay disagrees with appended decision".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory context decision append produced no durable event".to_owned())
}

fn append_local_memory_supersession_checked(
    store: &mut impl EventStore,
    predecessor_memory_id: LocalMemoryId,
    successor_memory_id: LocalMemoryId,
) -> Result<EventEnvelope, String> {
    if predecessor_memory_id == successor_memory_id {
        return Err(format!(
            "local memory {} cannot supersede itself",
            predecessor_memory_id.get()
        ));
    }

    let artifacts = replay_local_memory_audit(store.events())?;
    let predecessor = artifacts
        .iter()
        .find(|record| record.memory_id == predecessor_memory_id)
        .ok_or_else(|| {
            format!(
                "predecessor local memory {} does not exist",
                predecessor_memory_id.get()
            )
        })?;
    let successor = artifacts
        .iter()
        .find(|record| record.memory_id == successor_memory_id)
        .ok_or_else(|| {
            format!(
                "successor local memory {} does not exist",
                successor_memory_id.get()
            )
        })?;

    if predecessor.source_conversation_id != successor.source_conversation_id {
        return Err(format!(
            "local memory {} and {} have different source conversations",
            predecessor_memory_id.get(),
            successor_memory_id.get(),
        ));
    }
    if successor.recorded_sequence <= predecessor.recorded_sequence {
        return Err(format!(
            "successor local memory {} must be recorded after predecessor {}",
            successor_memory_id.get(),
            predecessor_memory_id.get(),
        ));
    }

    let existing = replay_local_memory_supersession_audit(store.events())?;
    if let Some(record) = existing
        .iter()
        .find(|record| record.predecessor_memory_id == predecessor_memory_id)
    {
        return Err(format!(
            "local memory {} is already superseded by {}",
            predecessor_memory_id.get(),
            record.successor_memory_id.get(),
        ));
    }
    if let Some(record) = existing
        .iter()
        .find(|record| record.successor_memory_id == successor_memory_id)
    {
        return Err(format!(
            "local memory {} already succeeds memory {}",
            successor_memory_id.get(),
            record.predecessor_memory_id.get(),
        ));
    }

    record_local_memory_superseded(store, predecessor_memory_id, successor_memory_id)
        .map_err(|error| error.to_string())?;

    let replayed = replay_local_memory_supersession_audit(store.events())?
        .into_iter()
        .find(|record| record.predecessor_memory_id == predecessor_memory_id)
        .ok_or_else(|| "local memory supersession append did not replay".to_owned())?;
    if replayed.successor_memory_id != successor_memory_id {
        return Err("local memory supersession replay disagrees with appended lineage".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory supersession append produced no durable event".to_owned())
}

fn append_local_memory_label_change_checked(
    store: &mut impl EventStore,
    memory_id: LocalMemoryId,
    label: LocalMemoryLabel,
    add: bool,
) -> Result<EventEnvelope, String> {
    if !replay_local_memory_audit(store.events())?
        .iter()
        .any(|artifact| artifact.memory_id == memory_id)
    {
        return Err(format!("local memory {} does not exist", memory_id.get()));
    }

    let existing = replay_local_memory_label_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id && record.label == label);

    if add {
        if existing.as_ref().is_some_and(|record| record.active) {
            return Err(format!(
                "local memory {} already has active label '{}'",
                memory_id.get(),
                label,
            ));
        }
        record_local_memory_label_added(store, memory_id, &label)
            .map_err(|error| error.to_string())?;
    } else {
        if !existing.as_ref().is_some_and(|record| record.active) {
            return Err(format!(
                "local memory {} does not have active label '{}'",
                memory_id.get(),
                label,
            ));
        }
        record_local_memory_label_removed(store, memory_id, &label)
            .map_err(|error| error.to_string())?;
    }

    let replayed = replay_local_memory_label_audit(store.events())?
        .into_iter()
        .find(|record| record.memory_id == memory_id && record.label == label)
        .ok_or_else(|| "local memory label append did not replay".to_owned())?;
    if replayed.active != add {
        return Err("local memory label replay disagrees with appended mutation".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "local memory label append produced no durable event".to_owned())
}

fn append_local_worker_binding_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<EventEnvelope, String> {
    let bindings = replay_local_conversation_worker_bindings(store.events())?;
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.conversation_id == conversation_id)
    {
        return Err(format!(
            "local conversation {conversation_id} is already bound to worker {}",
            existing.worker_id.get()
        ));
    }
    if let Some(existing) = bindings
        .iter()
        .find(|binding| binding.worker_id == worker_id)
    {
        return Err(format!(
            "worker {} is already bound to local conversation {}",
            worker_id.get(),
            existing.conversation_id
        ));
    }
    if replay_worker_audit(store.events())?
        .iter()
        .any(|record| record.worker_id == worker_id)
    {
        return Err(format!(
            "worker {} already has lifecycle history and cannot be rebound implicitly",
            worker_id.get()
        ));
    }
    if replay_session_audit(store.events())?.iter().any(|record| {
        record
            .worker_binding
            .is_some_and(|binding| binding.worker_id() == worker_id)
    }) {
        return Err(format!(
            "worker {} is already bound to a local session",
            worker_id.get()
        ));
    }

    record_local_conversation_worker_bound(store, conversation_id, worker_id)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker binding append produced no durable event".to_owned())
}

fn append_current_session_worker_binding_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<EventEnvelope, String> {
    let conversation_worker = require_local_worker_binding(store.events(), worker_id)?;
    if conversation_worker.conversation_id != conversation_id {
        return Err(format!(
            "worker {} belongs to local conversation {}, not {}",
            worker_id.get(),
            conversation_worker.conversation_id,
            conversation_id,
        ));
    }

    let topology =
        local_conversation_topology(store.events(), conversation_id)?.ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    let sessions = replay_session_audit(store.events())?;
    let current = sessions
        .iter()
        .find(|record| record.session_id == topology.current_session_id)
        .copied()
        .ok_or_else(|| {
            format!(
                "current session {} for local conversation {conversation_id} is not registered",
                topology.current_session_id.get()
            )
        })?;

    if replay_supervision_audit(store.events())?
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "current session {} is controller-designated and cannot become worker-bound",
            topology.current_session_id.get()
        ));
    }

    match active_worker_session(&sessions, worker_id) {
        None => {
            if let Some(existing) = current.worker_binding {
                return Err(format!(
                    "current session {} is already bound to worker {}",
                    topology.current_session_id.get(),
                    existing.worker_id().get()
                ));
            }
            record_worker_session_bound(
                store,
                WorkerSessionBinding::new(worker_id, topology.current_session_id),
            )
            .map_err(|error| error.to_string())?;
        }
        Some(active) if active.session_id == topology.current_session_id => {
            return Err(format!(
                "worker {} is already active on current session {}",
                worker_id.get(),
                topology.current_session_id.get()
            ));
        }
        Some(active) => {
            if let Some(existing) = current.worker_binding {
                return Err(format!(
                    "current session {} is already bound to worker {}",
                    topology.current_session_id.get(),
                    existing.worker_id().get()
                ));
            }

            let container = replay_chat_container_audit(store.events())?
                .into_iter()
                .find(|record| record.container_id == topology.container_id)
                .ok_or_else(|| {
                    format!(
                        "chat container {} for local conversation {conversation_id} disappeared",
                        topology.container_id.get()
                    )
                })?;
            let current_leaf = container
                .sessions
                .iter()
                .find(|session| session.session_id == topology.current_session_id)
                .ok_or_else(|| {
                    format!(
                        "chat container {} lacks current session {}",
                        topology.container_id.get(),
                        topology.current_session_id.get()
                    )
                })?;
            if current_leaf.predecessor_session_id != Some(active.session_id) {
                return Err(format!(
                    "worker {} is active on session {}, but current session {} is not its direct chat-container successor",
                    worker_id.get(),
                    active.session_id.get(),
                    topology.current_session_id.get()
                ));
            }

            record_worker_session_successor_bound(
                store,
                WorkerSessionSuccessorBinding::new(
                    worker_id,
                    active.session_id,
                    topology.current_session_id,
                )
                .map_err(|error| format!("invalid worker-session successor: {error:?}"))?,
            )
            .map_err(|error| error.to_string())?;
        }
    }

    let replayed = replay_session_audit(store.events())?;
    let active = active_worker_session(&replayed, worker_id)
        .ok_or_else(|| "worker-session append did not replay".to_owned())?;
    if active.session_id != topology.current_session_id {
        return Err("worker-session replay did not advance to current session".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker-session append produced no durable event".to_owned())
}

fn append_current_session_controller_designation_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    let topology =
        local_conversation_topology(store.events(), conversation_id)?.ok_or_else(|| {
            format!("local conversation {conversation_id} has no orchestration topology")
        })?;
    let session = replay_session_audit(store.events())?
        .into_iter()
        .find(|record| record.session_id == topology.current_session_id)
        .ok_or_else(|| {
            format!(
                "current session {} for local conversation {conversation_id} is not registered",
                topology.current_session_id.get()
            )
        })?;
    if let Some(binding) = session.worker_binding {
        return Err(format!(
            "current session {} is worker-bound to worker {} and cannot become a controller",
            topology.current_session_id.get(),
            binding.worker_id().get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "current session {} is already controller-designated",
            topology.current_session_id.get()
        ));
    }

    record_controller_session_designated(
        store,
        ControllerDesignation::new(topology.current_session_id),
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_supervision_audit(store.events())?;
    if !replayed
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err("controller designation append did not replay".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller designation append produced no durable event".to_owned())
}

fn append_local_controller_worker_binding_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
) -> Result<EventEnvelope, String> {
    if controller_conversation_id == worker_conversation_id {
        return Err("a local conversation cannot supervise itself".to_owned());
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if let Some(existing) = supervision
        .bindings
        .iter()
        .find(|record| record.binding.worker_session_id() == active_worker.session_id)
    {
        return Err(format!(
            "worker session {} is already supervised by controller session {}",
            active_worker.session_id.get(),
            existing.binding.controller_session_id().get()
        ));
    }

    let binding = ControllerWorkerBinding::new(
        controller_topology.current_session_id,
        active_worker.session_id,
    )
    .map_err(|error| format!("invalid controller-worker binding: {error:?}"))?;
    record_controller_worker_bound(store, binding).map_err(|error| error.to_string())?;

    let replayed = replay_supervision_audit(store.events())?;
    if !replayed
        .bindings
        .iter()
        .any(|record| record.binding == binding)
    {
        return Err("controller-worker binding append did not replay".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller-worker binding append produced no durable event".to_owned())
}

fn append_controller_continuation_lease_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
    allowance: u32,
) -> Result<(WorkerId, EventEnvelope), String> {
    if allowance == 0 {
        return Err("continuation lease allowance must be greater than zero".to_owned());
    }
    if controller_conversation_id == worker_conversation_id {
        return Err(
            "a controller conversation cannot authorize continuation for itself".to_owned(),
        );
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if !supervision.bindings.iter().any(|record| {
        record.binding.controller_session_id() == controller_topology.current_session_id
            && record.binding.worker_session_id() == active_worker.session_id
    }) {
        return Err(format!(
            "controller session {} does not supervise worker {} active session {}",
            controller_topology.current_session_id.get(),
            worker_binding.worker_id.get(),
            active_worker.session_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_binding.worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} has no assigned goal",
                worker_binding.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no assigned goal",
            worker_binding.worker_id.get()
        )
    })?;
    if worker.lifecycle.phase().is_terminal() {
        return Err(format!(
            "cannot create continuation lease for worker {} terminal phase {}",
            worker_binding.worker_id.get(),
            worker_phase_label(worker.lifecycle.phase())
        ));
    }

    let existing = replay_continuation_audit(store.events())?
        .into_iter()
        .filter(|record| record.worker_id == worker_binding.worker_id && record.goal_id == goal_id)
        .filter(|record| {
            record.remaining > 0
                || record
                    .permits
                    .iter()
                    .any(|permit| permit.consumed_by.is_none())
        })
        .collect::<Vec<_>>();
    if !existing.is_empty() {
        return Err(format!(
            "worker {} goal {} already has live continuation authority",
            worker_binding.worker_id.get(),
            goal_id.get()
        ));
    }

    let lease_id = next_available_continuation_lease_id(store.events())?;
    let lease = ContinuationLease::new(lease_id, worker_binding.worker_id, goal_id, allowance);
    record_continuation_lease_created(store, &lease).map_err(|error| error.to_string())?;

    let replayed = replay_continuation_audit(store.events())?
        .into_iter()
        .find(|record| record.lease_id == lease_id)
        .ok_or_else(|| "continuation lease append did not replay".to_owned())?;
    if replayed.worker_id != worker_binding.worker_id
        || replayed.goal_id != goal_id
        || replayed.allowance != allowance
        || replayed.issued != 0
        || replayed.remaining != allowance
    {
        return Err("continuation lease replay disagrees with requested authority".to_owned());
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "continuation lease append produced no durable event".to_owned())?;
    Ok((worker_binding.worker_id, event))
}

fn append_controller_worker_control_proposal_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    worker_conversation_id: LocalConversationId,
    action: ControllerControlAction,
) -> Result<(ControlId, RouteId, Vec<EventEnvelope>), String> {
    if controller_conversation_id == worker_conversation_id {
        return Err("a controller conversation cannot issue a worker control to itself".to_owned());
    }

    let controller_topology = local_conversation_topology(
        store.events(),
        controller_conversation_id,
    )?
    .ok_or_else(|| {
        format!(
            "controller conversation {controller_conversation_id} has no orchestration topology"
        )
    })?;
    let worker_topology = local_conversation_topology(store.events(), worker_conversation_id)?
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no orchestration topology")
        })?;
    let worker_binding = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!("worker conversation {worker_conversation_id} has no local WorkerId binding")
        })?;

    let sessions = replay_session_audit(store.events())?;
    let active_worker =
        active_worker_session(&sessions, worker_binding.worker_id).ok_or_else(|| {
            format!(
                "worker {} for local conversation {} has no active session binding",
                worker_binding.worker_id.get(),
                worker_conversation_id
            )
        })?;
    if active_worker.session_id != worker_topology.current_session_id {
        return Err(format!(
            "worker {} is active on session {}, but local conversation {} current session is {}; advance the worker session first",
            worker_binding.worker_id.get(),
            active_worker.session_id.get(),
            worker_conversation_id,
            worker_topology.current_session_id.get()
        ));
    }

    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == controller_topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            controller_topology.current_session_id.get()
        ));
    }
    if !supervision.bindings.iter().any(|record| {
        record.binding.controller_session_id() == controller_topology.current_session_id
            && record.binding.worker_session_id() == active_worker.session_id
    }) {
        return Err(format!(
            "controller session {} does not supervise worker {} active session {}",
            controller_topology.current_session_id.get(),
            worker_binding.worker_id.get(),
            active_worker.session_id.get()
        ));
    }

    let directory = replay_local_routing_directory(store.events())?;
    let controller_endpoint = directory
        .iter()
        .find(|entry| entry.conversation_id == controller_conversation_id)
        .ok_or_else(|| {
            format!(
                "controller conversation {controller_conversation_id} current session is not routing-addressable"
            )
        })?;
    let worker_endpoint = directory
        .iter()
        .find(|entry| entry.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {worker_conversation_id} current session is not routing-addressable"
            )
        })?;
    if controller_endpoint.current_session_id != controller_topology.current_session_id
        || worker_endpoint.current_session_id != worker_topology.current_session_id
    {
        return Err(
            "local routing directory disagrees with current controller/worker topology".to_owned(),
        );
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_binding.worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} has no assigned goal",
                worker_binding.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no assigned goal",
            worker_binding.worker_id.get()
        )
    })?;

    let control_id = next_available_control_id(store.events())?;
    let route_id = next_available_route_id(store.events())?;
    let before = store.events().len();
    let control = match action {
        ControllerControlAction::StartOrResume => WorkerControl::start_or_resume(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
        ControllerControlAction::Continue => {
            let leases = replay_continuation_audit(store.events())?
                .into_iter()
                .filter(|record| {
                    record.worker_id == worker_binding.worker_id && record.goal_id == goal_id
                })
                .collect::<Vec<_>>();
            let unconsumed = leases
                .iter()
                .flat_map(|record| {
                    record
                        .permits
                        .iter()
                        .filter(|permit| permit.consumed_by.is_none())
                        .map(|permit| (record.lease_id, permit.ordinal))
                })
                .collect::<Vec<_>>();

            let permit = match unconsumed.as_slice() {
                [(lease_id, ordinal)] => {
                    reconstruct_unconsumed_continuation_permit(store.events(), *lease_id, *ordinal)?
                }
                [] => {
                    let live = leases
                        .iter()
                        .filter(|record| record.remaining > 0)
                        .map(|record| record.lease_id)
                        .collect::<Vec<_>>();
                    let lease_id = match live.as_slice() {
                        [lease_id] => *lease_id,
                        [] => {
                            return Err(format!(
                                "worker {} goal {} has no live continuation lease",
                                worker_binding.worker_id.get(),
                                goal_id.get()
                            ));
                        }
                        _ => {
                            return Err(format!(
                                "worker {} goal {} has ambiguous live continuation leases",
                                worker_binding.worker_id.get(),
                                goal_id.get()
                            ));
                        }
                    };
                    let mut lease =
                        reconstruct_continuation_lease_for_next_issue(store.events(), lease_id)?;
                    let permit = lease.authorize(&worker.lifecycle).map_err(|error| {
                        format!(
                            "continuation lease {} cannot issue its next permit: {error:?}",
                            lease_id.get()
                        )
                    })?;
                    record_continuation_permit_issued(store, &permit)
                        .map_err(|error| error.to_string())?;
                    permit
                }
                _ => {
                    return Err(format!(
                        "worker {} goal {} has multiple issued unconsumed continuation permits",
                        worker_binding.worker_id.get(),
                        goal_id.get()
                    ));
                }
            };

            WorkerControl::continue_work(
                control_id,
                worker_binding.worker_id,
                &worker.lifecycle,
                permit,
            )
        }
        ControllerControlAction::Stop => WorkerControl::stop(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
        ControllerControlAction::StatusRequest => WorkerControl::status_request(
            control_id,
            worker_binding.worker_id,
            goal_id,
            &worker.lifecycle,
        ),
    }
    .map_err(|error| {
        format!(
            "controller {} control is not admissible: {error}",
            action.label()
        )
    })?;

    let request = RouteRequest {
        id: route_id,
        source: controller_endpoint.endpoint_id,
        destination: worker_endpoint.endpoint_id,
        class: RouteClass::OrchestrationControl,
    };
    let route_binding = ControlRouteBinding::new(control_id, &request)
        .map_err(|error| format!("invalid controller control route binding: {error:?}"))?;

    record_worker_control_admitted(store, &control).map_err(|error| error.to_string())?;
    record_worker_control_issuer_bound(
        store,
        ControlProvenance::new(
            control_id,
            ControlIssuer::ControllerSession(controller_topology.current_session_id),
        ),
    )
    .map_err(|error| error.to_string())?;
    record_route_proposed(store, request, RoutePolicy::RequireApproval)
        .map_err(|error| error.to_string())?;
    record_control_route_bound(store, route_binding).map_err(|error| error.to_string())?;

    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.control_id == control_id && record.route.id == route_id)
        .ok_or_else(|| {
            "controller control proposal did not replay as a validated route".to_owned()
        })?;
    if validated.issuer != ControlIssuer::ControllerSession(controller_topology.current_session_id)
        || validated.worker_id != worker_binding.worker_id
        || validated.worker_session_id != Some(active_worker.session_id)
    {
        return Err(
            "validated controller control route disagrees with requested provenance".to_owned(),
        );
    }

    Ok((control_id, route_id, store.events()[before..].to_vec()))
}

fn append_controller_control_route_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    decision: RouteUserDecision,
) -> Result<EventEnvelope, String> {
    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| {
            format!(
                "route {} is not a validated orchestration control",
                route_id.get()
            )
        })?;
    if !matches!(validated.issuer, ControlIssuer::ControllerSession(_)) {
        return Err(format!(
            "route {} is not controller-issued and cannot use the controller control UI",
            route_id.get()
        ));
    }

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("controller control route {} does not exist", route_id.get()))?;
    if route.initial_policy != RoutePolicy::RequireApproval {
        return Err(format!(
            "controller control route {} was not created under explicit-approval policy",
            route_id.get()
        ));
    }
    if route.gate_state.is_dispatched() {
        return Err(format!(
            "controller control route {} has already dispatched",
            route_id.get()
        ));
    }
    if route.latest_user_decision == Some(decision) {
        return Err(format!(
            "controller control route {} already has explicit user decision {}",
            route_id.get(),
            route_user_decision_label(decision)
        ));
    }

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == validated.control_id)
        .ok_or_else(|| {
            format!(
                "controller control {} disappeared",
                validated.control_id.get()
            )
        })?;
    let next_sequence = store
        .events()
        .last()
        .map_or(1, |event| event.sequence.saturating_add(1));
    validate_control_freshness_before(store.events(), &control, next_sequence)?;

    record_route_user_decision(store, route_id, decision).map_err(|error| error.to_string())?;

    let replayed = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| "controller control route decision did not replay".to_owned())?;
    let expected = match decision {
        RouteUserDecision::Allow => RouteGateState::Allowed {
            by: DecisionAuthority::User,
        },
        RouteUserDecision::Deny => RouteGateState::Denied {
            by: DecisionAuthority::User,
        },
    };
    if replayed.gate_state != expected {
        return Err("controller control route gate disagrees with appended decision".to_owned());
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller control decision append produced no durable event".to_owned())
}

fn append_controller_control_dispatch_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    if let Some(existing) = replay_worker_control_delivery_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        return Err(format!(
            "controller control route {} is already delivered at event #{}",
            route_id.get(),
            existing.delivered_sequence
        ));
    }

    let validated = replay_validated_orchestration_routes(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| {
            format!(
                "route {} is not a validated orchestration control",
                route_id.get()
            )
        })?;
    let controller_session_id = match validated.issuer {
        ControlIssuer::ControllerSession(session_id) => session_id,
        ControlIssuer::User => {
            return Err(format!(
                "route {} is not controller-issued and cannot use the controller control UI",
                route_id.get()
            ));
        }
    };

    let route = replay_routing_audit(store.events())?
        .into_iter()
        .find(|record| record.request.id == route_id)
        .ok_or_else(|| format!("controller control route {} does not exist", route_id.get()))?;
    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == validated.control_id)
        .ok_or_else(|| {
            format!(
                "controller control {} disappeared",
                validated.control_id.get()
            )
        })?;

    let before = store.events().len();
    let dispatch_sequence = match route.gate_state {
        RouteGateState::Allowed { .. } => {
            let next_sequence = store
                .events()
                .last()
                .map_or(1, |event| event.sequence.saturating_add(1));
            validate_control_freshness_before(store.events(), &control, next_sequence)?;

            let mut gate = route_gate_before_dispatch(&route)?;
            let permit = gate.authorize_dispatch(route_id).map_err(|error| {
                format!(
                    "controller control route {} dispatch gate rejected: {error:?}",
                    route_id.get()
                )
            })?;
            record_route_dispatched(store, permit).map_err(|error| error.to_string())?
        }
        RouteGateState::Dispatched { .. } => route.dispatch_sequence.ok_or_else(|| {
            format!(
                "controller control route {} is marked dispatched without a durable dispatch sequence",
                route_id.get()
            )
        })?,
        RouteGateState::PendingApproval => {
            return Err(format!(
                "controller control route {} is still awaiting explicit user approval",
                route_id.get()
            ));
        }
        RouteGateState::Denied { .. } => {
            return Err(format!(
                "controller control route {} is denied and cannot dispatch",
                route_id.get()
            ));
        }
    };

    let dispatched = replay_validated_control_dispatches(store.events())?
        .into_iter()
        .find(|record| record.route.id == route_id)
        .ok_or_else(|| "controller control dispatch did not replay as validated".to_owned())?;
    if dispatched.control_id != validated.control_id
        || dispatched.dispatch_sequence != dispatch_sequence
        || dispatched.authorized_by != DecisionAuthority::User
    {
        return Err(
            "validated controller control dispatch disagrees with durable dispatch".to_owned(),
        );
    }

    let worker_session_id = dispatched.worker_session_id.ok_or_else(|| {
        format!(
            "controller control {} has no resolved worker session at dispatch",
            dispatched.control_id.get()
        )
    })?;
    if dispatched.controller_session_id != Some(controller_session_id) {
        return Err(
            "validated controller control dispatch controller provenance disagrees".to_owned(),
        );
    }

    let worker_owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.worker_id == dispatched.worker_id)
        .ok_or_else(|| {
            format!(
                "controller control {} targets worker {} without local conversation ownership",
                dispatched.control_id.get(),
                dispatched.worker_id.get()
            )
        })?;

    record_worker_control_delivered(
        store,
        dispatched.control_id,
        route_id,
        dispatched.worker_id,
        worker_owner.conversation_id,
        worker_session_id,
        controller_session_id,
        dispatch_sequence,
    )
    .map_err(|error| error.to_string())?;

    let delivery = replay_worker_control_delivery_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "controller control delivery did not replay".to_owned())?;
    if delivery.control_id != dispatched.control_id
        || delivery.worker_id != dispatched.worker_id
        || delivery.worker_conversation_id != worker_owner.conversation_id
        || delivery.worker_session_id != worker_session_id
        || delivery.controller_session_id != controller_session_id
        || delivery.dispatch_sequence != dispatch_sequence
    {
        return Err(
            "worker control delivery replay disagrees with dispatched control provenance"
                .to_owned(),
        );
    }

    Ok(store.events()[before..].to_vec())
}

fn append_controller_worker_result_context_decision_checked(
    store: &mut impl EventStore,
    route_id: RouteId,
    controller_conversation_id: LocalConversationId,
    decision: ControllerWorkerResultContextDecision,
) -> Result<EventEnvelope, String> {
    let result = replay_controller_worker_results_for_conversation(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .find(|item| item.route_id == route_id)
    .ok_or_else(|| {
        format!(
            "route {} has no terminal worker result owned by controller conversation {}",
            route_id.get(),
            controller_conversation_id,
        )
    })?;

    if let Some(existing) = replay_controller_worker_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
    {
        if existing.control_id != result.control_id
            || existing.controller_conversation_id != controller_conversation_id
            || existing.result_sequence != result.result_sequence
        {
            return Err(format!(
                "controller result route {} context provenance conflicts with terminal result",
                route_id.get()
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "controller result route {} already has context decision {}",
                route_id.get(),
                controller_worker_result_context_decision_label(decision),
            ));
        }
    }

    record_controller_worker_result_context_decision(
        store,
        result.control_id,
        route_id,
        controller_conversation_id,
        result.result_sequence,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_worker_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "controller result context decision append did not replay".to_owned())?;
    if replayed.control_id != result.control_id
        || replayed.controller_conversation_id != controller_conversation_id
        || replayed.result_sequence != result.result_sequence
        || replayed.decision != decision
    {
        return Err(
            "controller result context decision replay disagrees with appended decision".to_owned(),
        );
    }

    store.events().last().cloned().ok_or_else(|| {
        "controller result context decision append produced no durable event".to_owned()
    })
}

fn append_controller_coordination_start_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
) -> Result<(LocalTurnId, EventEnvelope), String> {
    if replay_controller_coordination_audit(store.events())?
        .into_iter()
        .any(|record| {
            record.controller_conversation_id == controller_conversation_id
                && record.result_sequence.is_none()
        })
    {
        return Err(format!(
            "controller conversation {} already has an unfinished coordination turn",
            controller_conversation_id
        ));
    }

    let topology = local_conversation_topology(store.events(), controller_conversation_id)?
        .ok_or_else(|| {
            format!(
                "controller conversation {controller_conversation_id} has no orchestration topology"
            )
        })?;
    let supervision = replay_supervision_audit(store.events())?;
    if !supervision
        .controllers
        .iter()
        .any(|record| record.designation.session_id() == topology.current_session_id)
    {
        return Err(format!(
            "controller conversation {} current session {} is not controller-designated",
            controller_conversation_id,
            topology.current_session_id.get()
        ));
    }

    let mut admitted_routes = replay_admitted_controller_worker_result_context(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .map(|record| record.route_id)
    .collect::<Vec<_>>();
    admitted_routes.sort_by_key(|route_id| route_id.get());
    if admitted_routes.is_empty() {
        return Err(
            "controller coordination requires at least one admitted terminal worker result"
                .to_owned(),
        );
    }

    let turn_id = LocalTurnId::new();
    record_controller_coordination_started(
        store,
        controller_conversation_id,
        topology.current_session_id,
        turn_id,
        &admitted_routes,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == turn_id)
        .ok_or_else(|| "controller coordination start append did not replay".to_owned())?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.controller_session_id != topology.current_session_id
        || replayed.admitted_result_routes != admitted_routes
        || replayed.result_sequence.is_some()
    {
        return Err(
            "controller coordination replay disagrees with durable start provenance".to_owned(),
        );
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "controller coordination start produced no durable event".to_owned())?;
    Ok((turn_id, event))
}

fn append_controller_coordination_result_if_terminal(
    store: &mut impl EventStore,
    coordination_turn_id: LocalTurnId,
) -> Result<Option<EventEnvelope>, String> {
    let record = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id);
    let Some(record) = record else {
        return Ok(None);
    };
    if record.result_sequence.is_some() {
        return Ok(None);
    }

    let transport = controller_coordination_transport_state(store.events(), coordination_turn_id)?;
    let Some((outcome, terminal_sequence)) = coordination_terminal_outcome(transport) else {
        return Ok(None);
    };

    record_controller_coordination_result(store, &record, outcome, terminal_sequence)
        .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| "controller coordination terminal result did not replay".to_owned())?;
    if replayed.outcome != Some(outcome)
        || replayed.terminal_sequence != Some(terminal_sequence)
        || replayed.result_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "controller coordination result replay disagrees with terminal transport evidence"
                .to_owned(),
        );
    }

    Ok(store.events().last().cloned())
}

fn append_controller_coordination_result_context_decision_checked(
    store: &mut impl EventStore,
    coordination_turn_id: LocalTurnId,
    controller_conversation_id: LocalConversationId,
    decision: ControllerCoordinationResultContextDecision,
) -> Result<EventEnvelope, String> {
    let coordination = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            format!(
                "controller coordination turn {} does not exist",
                coordination_turn_id
            )
        })?;
    if coordination.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "controller coordination turn {} belongs to conversation {}, not {}",
            coordination_turn_id,
            coordination.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    let result_sequence = coordination.result_sequence.ok_or_else(|| {
        format!(
            "controller coordination turn {} has no terminal result to admit",
            coordination_turn_id
        )
    })?;

    if let Some(existing) = replay_controller_coordination_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
    {
        if existing.controller_conversation_id != controller_conversation_id
            || existing.result_sequence != result_sequence
        {
            return Err(format!(
                "controller coordination turn {} result context provenance conflicts with terminal result",
                coordination_turn_id
            ));
        }
        if existing.decision == decision {
            return Err(format!(
                "controller coordination turn {} already has result context decision {}",
                coordination_turn_id,
                controller_coordination_result_context_decision_label(decision),
            ));
        }
    }

    record_controller_coordination_result_context_decision(
        store,
        controller_conversation_id,
        coordination_turn_id,
        result_sequence,
        decision,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_result_context_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            "controller coordination result context decision append did not replay".to_owned()
        })?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.result_sequence != result_sequence
        || replayed.decision != decision
    {
        return Err(
            "controller coordination result context replay disagrees with appended decision"
                .to_owned(),
        );
    }

    store.events().last().cloned().ok_or_else(|| {
        "controller coordination result context decision append produced no durable event"
            .to_owned()
    })
}

fn append_controller_coordination_suggestion_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    coordination_turn_id: LocalTurnId,
    basis_result_route_id: RouteId,
    action: CoordinationSuggestionAction,
) -> Result<(CoordinationSuggestionId, EventEnvelope), String> {
    let coordination = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .find(|record| record.coordination_turn_id == coordination_turn_id)
        .ok_or_else(|| {
            format!(
                "controller coordination turn {} does not exist",
                coordination_turn_id
            )
        })?;
    if coordination.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "controller coordination turn {} belongs to conversation {}, not {}",
            coordination_turn_id,
            coordination.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    if coordination.outcome != Some(ControllerCoordinationOutcome::Completed) {
        return Err(format!(
            "controller coordination turn {} is not completed and cannot produce suggestions",
            coordination_turn_id
        ));
    }
    let result_sequence = coordination.result_sequence.ok_or_else(|| {
        format!(
            "controller coordination turn {} has no terminal result",
            coordination_turn_id
        )
    })?;
    if !coordination
        .admitted_result_routes
        .contains(&basis_result_route_id)
    {
        return Err(format!(
            "worker result route {} was not frozen into coordination turn {}",
            basis_result_route_id.get(),
            coordination_turn_id
        ));
    }

    let basis = replay_controller_worker_results_for_conversation(
        store.events(),
        controller_conversation_id,
    )?
    .into_iter()
    .find(|item| item.route_id == basis_result_route_id)
    .ok_or_else(|| {
        format!(
            "worker result route {} is not visible to controller conversation {}",
            basis_result_route_id.get(),
            controller_conversation_id
        )
    })?;

    if replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .any(|record| {
            record.coordination_turn_id == coordination_turn_id
                && record.basis_result_route_id == basis_result_route_id
                && record.suggestion.action() == action
        })
    {
        return Err(format!(
            "coordination turn {} already has {} suggestion for basis route {}",
            coordination_turn_id,
            coordination_suggestion_action_label(action),
            basis_result_route_id.get()
        ));
    }

    let suggestion_id = next_available_coordination_suggestion_id(store.events())?;
    let record = ControllerCoordinationSuggestionRecord {
        suggestion: CoordinationSuggestion::new(
            suggestion_id,
            basis.worker_id,
            basis.goal_id,
            action,
        ),
        controller_conversation_id,
        coordination_turn_id,
        coordination_result_sequence: result_sequence,
        basis_result_route_id,
        worker_conversation_id: basis.worker_conversation_id,
        recorded_sequence: 0,
        promoted_control_id: None,
        promoted_route_id: None,
        promoted_sequence: None,
    };
    record_controller_coordination_suggestion(store, &record).map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|candidate| candidate.suggestion.id() == suggestion_id)
        .ok_or_else(|| "coordination suggestion append did not replay".to_owned())?;
    if replayed.controller_conversation_id != controller_conversation_id
        || replayed.coordination_turn_id != coordination_turn_id
        || replayed.coordination_result_sequence != result_sequence
        || replayed.basis_result_route_id != basis_result_route_id
        || replayed.worker_conversation_id != basis.worker_conversation_id
        || replayed.suggestion.worker_id() != basis.worker_id
        || replayed.suggestion.goal_id() != basis.goal_id
        || replayed.suggestion.action() != action
    {
        return Err(
            "coordination suggestion replay disagrees with authoritative result provenance"
                .to_owned(),
        );
    }

    let event = store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "coordination suggestion append produced no durable event".to_owned())?;
    Ok((suggestion_id, event))
}

fn append_controller_coordination_suggestion_promotion_checked(
    store: &mut impl EventStore,
    controller_conversation_id: LocalConversationId,
    suggestion_id: CoordinationSuggestionId,
) -> Result<(ControlId, RouteId, Vec<EventEnvelope>), String> {
    let suggestion = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|record| record.suggestion.id() == suggestion_id)
        .ok_or_else(|| {
            format!(
                "coordination suggestion {} does not exist",
                suggestion_id.get()
            )
        })?;
    if suggestion.controller_conversation_id != controller_conversation_id {
        return Err(format!(
            "coordination suggestion {} belongs to controller conversation {}, not {}",
            suggestion_id.get(),
            suggestion.controller_conversation_id,
            controller_conversation_id,
        ));
    }
    if suggestion.promoted_sequence.is_some() {
        return Err(format!(
            "coordination suggestion {} is already promoted to control {} route {}",
            suggestion_id.get(),
            suggestion.promoted_control_id.map_or(0, ControlId::get),
            suggestion.promoted_route_id.map_or(0, RouteId::get),
        ));
    }

    let worker_owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == suggestion.worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "suggestion {} target conversation {} no longer has a WorkerId binding",
                suggestion_id.get(),
                suggestion.worker_conversation_id
            )
        })?;
    if worker_owner.worker_id != suggestion.suggestion.worker_id() {
        return Err(format!(
            "suggestion {} target conversation now owns worker {}, expected worker {}",
            suggestion_id.get(),
            worker_owner.worker_id.get(),
            suggestion.suggestion.worker_id().get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == suggestion.suggestion.worker_id())
        .ok_or_else(|| {
            format!(
                "suggestion {} target worker {} has no lifecycle state",
                suggestion_id.get(),
                suggestion.suggestion.worker_id().get()
            )
        })?;
    let current_goal = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "suggestion {} target worker {} no longer has an assigned goal",
            suggestion_id.get(),
            suggestion.suggestion.worker_id().get()
        )
    })?;
    if current_goal != suggestion.suggestion.goal_id() {
        return Err(format!(
            "coordination suggestion {} is stale: worker {} current goal is {}, suggestion observed goal {}",
            suggestion_id.get(),
            suggestion.suggestion.worker_id().get(),
            current_goal.get(),
            suggestion.suggestion.goal_id().get()
        ));
    }

    let action = controller_control_action_from_suggestion(suggestion.suggestion.action());
    let before = store.events().len();
    let (control_id, route_id, _) = append_controller_worker_control_proposal_checked(
        store,
        controller_conversation_id,
        suggestion.worker_conversation_id,
        action,
    )?;

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == control_id)
        .ok_or_else(|| "promoted suggestion control disappeared after proposal".to_owned())?;
    if control.worker_id != suggestion.suggestion.worker_id()
        || control.goal_id != suggestion.suggestion.goal_id()
        || !coordination_suggestion_matches_control_kind(
            suggestion.suggestion.action(),
            control.kind,
        )
    {
        return Err(
            "promoted control does not match coordination suggestion provenance".to_owned(),
        );
    }

    record_controller_coordination_suggestion_promoted(
        store,
        controller_conversation_id,
        suggestion_id,
        control_id,
        route_id,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_controller_coordination_suggestion_audit(store.events())?
        .into_iter()
        .find(|record| record.suggestion.id() == suggestion_id)
        .ok_or_else(|| "promoted coordination suggestion did not replay".to_owned())?;
    if replayed.promoted_control_id != Some(control_id)
        || replayed.promoted_route_id != Some(route_id)
        || replayed.promoted_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "coordination suggestion promotion replay disagrees with control proposal".to_owned(),
        );
    }

    Ok((control_id, route_id, store.events()[before..].to_vec()))
}

fn controller_control_action_from_suggestion(
    action: CoordinationSuggestionAction,
) -> ControllerControlAction {
    match action {
        CoordinationSuggestionAction::StartOrResume => ControllerControlAction::StartOrResume,
        CoordinationSuggestionAction::Continue => ControllerControlAction::Continue,
        CoordinationSuggestionAction::Stop => ControllerControlAction::Stop,
        CoordinationSuggestionAction::StatusRequest => ControllerControlAction::StatusRequest,
    }
}

fn coordination_suggestion_matches_control_kind(
    action: CoordinationSuggestionAction,
    kind: WorkerControlKind,
) -> bool {
    matches!(
        (action, kind),
        (
            CoordinationSuggestionAction::StartOrResume,
            WorkerControlKind::StartOrResume
        ) | (
            CoordinationSuggestionAction::Continue,
            WorkerControlKind::Continue { .. }
        ) | (CoordinationSuggestionAction::Stop, WorkerControlKind::Stop)
            | (
                CoordinationSuggestionAction::StatusRequest,
                WorkerControlKind::StatusRequest
            )
    )
}

fn append_worker_control_acknowledgement_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    if item.acknowledged_sequence.is_some() {
        return Err(format!(
            "worker control route {} is already acknowledged",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    record_worker_control_acknowledged(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        item.delivered_sequence,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_control_acknowledgement_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker control acknowledgement append did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.delivered_sequence != item.delivered_sequence
    {
        return Err(
            "worker control acknowledgement replay disagrees with delivered inbox item".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker control acknowledgement append produced no durable event".to_owned())
}

fn append_worker_control_status_result_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    if item.kind != WorkerControlKind::StatusRequest {
        return Err(format!(
            "worker control route {} is {}, not a StatusRequest",
            route_id.get(),
            worker_control_kind_label(item.kind),
        ));
    }

    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before recording a status result",
            route_id.get()
        )
    })?;

    if replay_worker_control_status_results(store.events())?
        .into_iter()
        .any(|record| record.route_id == route_id || record.control_id == item.control_id)
    {
        return Err(format!(
            "worker control route {} already has a durable status result",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == item.worker_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references worker {} without lifecycle state",
                route_id.get(),
                item.worker_id.get()
            )
        })?;
    let goal_id = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "worker {} has no active goal to report for status route {}",
            item.worker_id.get(),
            route_id.get()
        )
    })?;
    if goal_id != item.goal_id {
        return Err(format!(
            "worker control route {} targets goal {}, but worker {} current goal is {}",
            route_id.get(),
            item.goal_id.get(),
            item.worker_id.get(),
            goal_id.get()
        ));
    }
    let phase = worker.lifecycle.phase();

    record_worker_control_status_result(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        goal_id,
        phase,
        acknowledged_sequence,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_control_status_results(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker control status result append did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.goal_id != goal_id
        || replayed.phase != phase
        || replayed.acknowledged_sequence != acknowledged_sequence
    {
        return Err(
            "worker control status result replay disagrees with acknowledged inbox item".to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker control status result append produced no durable event".to_owned())
}

fn append_worker_control_action_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<Vec<EventEnvelope>, String> {
    let before = store.events().len();
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before applying the control",
            route_id.get()
        )
    })?;
    let action = action_for_kind(item.kind)?;

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let mut action_record = replay_worker_control_action_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id);

    if let Some(existing) = action_record {
        if existing.control_id != item.control_id
            || existing.worker_id != item.worker_id
            || existing.worker_conversation_id != worker_conversation_id
            || existing.goal_id != item.goal_id
            || existing.kind != item.kind
            || existing.acknowledged_sequence != acknowledged_sequence
        {
            return Err(format!(
                "worker control route {} action provenance disagrees with delivered inbox item",
                route_id.get()
            ));
        }
        if existing.is_complete() {
            return Err(format!(
                "worker control route {} already has a complete action result",
                route_id.get()
            ));
        }
    } else {
        let worker = replay_worker_audit(store.events())?
            .into_iter()
            .find(|record| record.worker_id == item.worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} references worker {} without lifecycle state",
                    route_id.get(),
                    item.worker_id.get()
                )
            })?;
        let from_phase = validate_control_admission(item.goal_id, item.kind, &worker.lifecycle)
            .map_err(|error| {
                format!(
                    "worker control route {} is stale before action start: {error}",
                    route_id.get()
                )
            })?;

        record_worker_control_action_started(
            store,
            item.control_id,
            item.route_id,
            item.worker_id,
            worker_conversation_id,
            item.goal_id,
            item.kind,
            acknowledged_sequence,
            from_phase,
        )
        .map_err(|error| error.to_string())?;

        action_record = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id);
    }

    let mut action_record =
        action_record.ok_or_else(|| "worker control action start did not replay".to_owned())?;

    if action_record.lifecycle_sequence.is_none() {
        let worker = replay_worker_audit(store.events())?
            .into_iter()
            .find(|record| record.worker_id == item.worker_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} lost worker {} lifecycle state",
                    route_id.get(),
                    item.worker_id.get()
                )
            })?;

        if worker.last_sequence > action_record.started_sequence {
            return Err(format!(
                "worker {} lifecycle changed at event #{} after control route {} action started at event #{} without matching control correlation",
                item.worker_id.get(),
                worker.last_sequence,
                route_id.get(),
                action_record.started_sequence,
            ));
        }

        let replayed_from = validate_control_admission(item.goal_id, item.kind, &worker.lifecycle)
            .map_err(|error| {
                format!(
                    "worker control route {} became stale before lifecycle mutation: {error}",
                    route_id.get()
                )
            })?;
        if replayed_from != action_record.from_phase {
            return Err(format!(
                "worker control route {} source phase changed after action start",
                route_id.get()
            ));
        }

        let mut lifecycle = worker.lifecycle;
        match action {
            WorkerAction::StartOrResume => {
                lifecycle
                    .start_or_resume(item.goal_id)
                    .map_err(|error| error.to_string())?;
            }
            WorkerAction::Stop => {
                lifecycle
                    .stop(item.goal_id)
                    .map_err(|error| error.to_string())?;
            }
            _ => {
                return Err(format!(
                    "worker control route {} action {:?} is not implemented by the mutating control path",
                    route_id.get(),
                    action
                ));
            }
        }

        record_worker_control_transition(
            store,
            item.worker_id,
            item.goal_id,
            action,
            WorkerControlTransitionProvenance {
                control_id: item.control_id,
                route_id: item.route_id,
                action_started_sequence: action_record.started_sequence,
            },
        )
        .map_err(|error| error.to_string())?;

        action_record = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id)
            .ok_or_else(|| "worker control lifecycle transition did not replay".to_owned())?;

        if action_record.resulting_phase != Some(lifecycle.phase()) {
            return Err(format!(
                "worker control route {} replayed result phase disagrees with core transition",
                route_id.get()
            ));
        }
    }

    if action_record.result_sequence.is_none() {
        let lifecycle_sequence = action_record.lifecycle_sequence.ok_or_else(|| {
            format!(
                "worker control route {} has no correlated lifecycle transition",
                route_id.get()
            )
        })?;
        let resulting_phase = action_record.resulting_phase.ok_or_else(|| {
            format!(
                "worker control route {} has no correlated resulting phase",
                route_id.get()
            )
        })?;

        record_worker_control_action_result(
            store,
            action_record,
            lifecycle_sequence,
            resulting_phase,
        )
        .map_err(|error| error.to_string())?;

        let replayed = replay_worker_control_action_audit(store.events())?
            .into_iter()
            .find(|record| record.route_id == route_id)
            .ok_or_else(|| "worker control action result did not replay".to_owned())?;
        if !replayed.is_complete()
            || replayed.lifecycle_sequence != Some(lifecycle_sequence)
            || replayed.resulting_phase != Some(resulting_phase)
        {
            return Err(
                "worker control action result replay disagrees with correlated lifecycle transition"
                    .to_owned(),
            );
        }
    }

    Ok(store.events()[before..].to_vec())
}

fn append_worker_continuation_execution_start_checked(
    store: &mut impl EventStore,
    worker_conversation_id: LocalConversationId,
    route_id: RouteId,
) -> Result<EventEnvelope, String> {
    let item =
        replay_worker_control_inbox_for_conversation(store.events(), worker_conversation_id)?
            .into_iter()
            .find(|item| item.route_id == route_id)
            .ok_or_else(|| {
                format!(
                    "worker control route {} is not a delivered inbox item for conversation {}",
                    route_id.get(),
                    worker_conversation_id,
                )
            })?;

    let WorkerControlKind::Continue { permit_ordinal } = item.kind else {
        return Err(format!(
            "worker control route {} is {}, not CONTINUE",
            route_id.get(),
            worker_control_kind_label(item.kind),
        ));
    };
    let acknowledged_sequence = item.acknowledged_sequence.ok_or_else(|| {
        format!(
            "worker control route {} must be acknowledged before continuation execution starts",
            route_id.get()
        )
    })?;

    if replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .any(|record| record.route_id == route_id || record.control_id == item.control_id)
    {
        return Err(format!(
            "worker control route {} already has a continuation execution start",
            route_id.get()
        ));
    }

    let owner = replay_local_conversation_worker_bindings(store.events())?
        .into_iter()
        .find(|binding| binding.conversation_id == worker_conversation_id)
        .ok_or_else(|| {
            format!(
                "worker conversation {} has no durable WorkerId binding",
                worker_conversation_id
            )
        })?;
    if owner.worker_id != item.worker_id {
        return Err(format!(
            "worker control route {} inbox worker disagrees with durable conversation ownership",
            route_id.get()
        ));
    }

    let control = replay_control_audit(store.events())?
        .into_iter()
        .find(|record| record.control_id == item.control_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references missing admitted control {}",
                route_id.get(),
                item.control_id.get()
            )
        })?;
    let permit = control.continuation_permit.ok_or_else(|| {
        format!(
            "worker Continue control {} has no durable continuation permit provenance",
            item.control_id.get()
        )
    })?;
    if permit.ordinal != permit_ordinal {
        return Err(format!(
            "worker Continue control {} permit ordinal disagrees with inbox",
            item.control_id.get()
        ));
    }

    let worker = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == item.worker_id)
        .ok_or_else(|| {
            format!(
                "worker control route {} references worker {} without lifecycle state",
                route_id.get(),
                item.worker_id.get()
            )
        })?;
    if worker.lifecycle.goal_id() != Some(item.goal_id) {
        return Err(format!(
            "worker control route {} targets stale goal {}",
            route_id.get(),
            item.goal_id.get()
        ));
    }
    if !worker.lifecycle.phase().allows_continuation() {
        return Err(format!(
            "worker control route {} cannot continue while worker {} is {}",
            route_id.get(),
            item.worker_id.get(),
            worker_phase_label(worker.lifecycle.phase()),
        ));
    }

    let execution_turn_id = LocalTurnId::new();
    record_worker_continuation_execution_started(
        store,
        item.control_id,
        item.route_id,
        item.worker_id,
        worker_conversation_id,
        item.goal_id,
        permit.lease_id,
        permit.ordinal,
        acknowledged_sequence,
        execution_turn_id,
    )
    .map_err(|error| error.to_string())?;

    let replayed = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == route_id)
        .ok_or_else(|| "worker continuation execution start did not replay".to_owned())?;
    if replayed.control_id != item.control_id
        || replayed.worker_id != item.worker_id
        || replayed.worker_conversation_id != worker_conversation_id
        || replayed.goal_id != item.goal_id
        || replayed.lease_id != permit.lease_id
        || replayed.permit_ordinal != permit.ordinal
        || replayed.acknowledged_sequence != acknowledged_sequence
        || replayed.execution_turn_id != execution_turn_id
    {
        return Err(
            "worker continuation execution replay disagrees with acknowledged Continue provenance"
                .to_owned(),
        );
    }

    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker continuation execution append produced no durable event".to_owned())
}

fn append_worker_continuation_result_if_terminal(
    store: &mut impl EventStore,
    execution_turn_id: LocalTurnId,
) -> Result<Option<(RouteId, EventEnvelope)>, String> {
    let execution = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.execution_turn_id == execution_turn_id);
    let Some(execution) = execution else {
        return Ok(None);
    };
    if execution.result_sequence.is_some() {
        return Ok(None);
    }

    let transport =
        continuation_execution_transport_state(store.events(), execution.execution_turn_id)?;
    let Some((outcome, terminal_sequence)) = transport_terminal_outcome(transport) else {
        return Ok(None);
    };

    record_worker_continuation_execution_result(store, execution, outcome, terminal_sequence)
        .map_err(|error| error.to_string())?;

    let replayed = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .find(|record| record.route_id == execution.route_id)
        .ok_or_else(|| "worker continuation execution result did not replay".to_owned())?;
    if replayed.outcome != Some(outcome)
        || replayed.terminal_sequence != Some(terminal_sequence)
        || replayed.result_sequence != store.events().last().map(|event| event.sequence)
    {
        return Err(
            "worker continuation execution result replay disagrees with terminal transport evidence"
                .to_owned(),
        );
    }

    let event =
        store.events().last().cloned().ok_or_else(|| {
            "worker continuation result append produced no durable event".to_owned()
        })?;
    Ok(Some((execution.route_id, event)))
}

fn append_worker_goal_checked(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
) -> Result<EventEnvelope, String> {
    require_local_worker_binding(store.events(), worker_id)?;
    let mut lifecycle = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_id)
        .map_or_else(WorkerLifecycle::default, |record| record.lifecycle);
    lifecycle
        .assign_goal(goal_id)
        .map_err(|error| error.to_string())?;

    record_worker_goal_assigned(store, worker_id, goal_id).map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker goal append produced no durable event".to_owned())
}

fn append_worker_transition_checked(
    store: &mut impl EventStore,
    worker_id: WorkerId,
    goal_id: WorkerGoalId,
    action: WorkerAction,
) -> Result<EventEnvelope, String> {
    require_local_worker_binding(store.events(), worker_id)?;
    let mut lifecycle = replay_worker_audit(store.events())?
        .into_iter()
        .find(|record| record.worker_id == worker_id)
        .ok_or_else(|| format!("worker {} has no assigned goal", worker_id.get()))?
        .lifecycle;
    apply_worker_action(&mut lifecycle, goal_id, action)?;

    record_worker_transition(store, worker_id, goal_id, action)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "worker transition append produced no durable event".to_owned())
}

fn require_local_worker_binding(
    events: &[EventEnvelope],
    worker_id: WorkerId,
) -> Result<LocalConversationWorkerBindingRecord, String> {
    replay_local_conversation_worker_bindings(events)?
        .into_iter()
        .find(|binding| binding.worker_id == worker_id)
        .ok_or_else(|| {
            format!(
                "worker {} is not bound to a local Chatarium conversation",
                worker_id.get()
            )
        })
}

fn apply_worker_action(
    lifecycle: &mut WorkerLifecycle,
    goal_id: WorkerGoalId,
    action: WorkerAction,
) -> Result<(), String> {
    match action {
        WorkerAction::AssignGoal => {
            Err("AssignGoal must use the dedicated durable goal-assignment command".to_owned())
        }
        WorkerAction::StartOrResume => lifecycle
            .start_or_resume(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::ReportProgress => lifecycle
            .report_progress(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::RequestInput => lifecycle
            .request_input(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::MarkBlocked => lifecycle
            .mark_blocked(goal_id)
            .map_err(|error| error.to_string()),
        WorkerAction::Complete => lifecycle
            .complete(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        WorkerAction::Fail => lifecycle
            .fail(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        WorkerAction::Stop => lifecycle
            .stop(goal_id)
            .map(|_| ())
            .map_err(|error| error.to_string()),
    }
}

fn checkpoint_owner_for_turn(
    events: &[EventEnvelope],
    turn_id: LocalTurnId,
) -> Option<LocalConversationId> {
    let scope = local_turn_scope(turn_id);
    let mut owner = None;
    for event in events {
        if event.kind != EventKind::UserMessageCommitted {
            continue;
        }
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        if local_turn_scope(message.turn_id) != scope {
            continue;
        }
        if owner.is_some_and(|existing| existing != message.conversation_id) {
            return None;
        }
        owner = Some(message.conversation_id);
    }
    owner
}

fn checkpoint_completion_eligible(event: &EventEnvelope) -> bool {
    if event.kind != EventKind::AssistantCompletionObserved {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
        return false;
    };
    let Some(text) = payload["text"].as_str() else {
        return false;
    };
    text.trim().chars().count() >= 160
        && matches!(mcp_agent_defaults::parse_model_tool_request(text), Ok(None))
}

/// Recover committed assistant completions after the durable activation
/// boundary. A crash between journaling completion and creating the outbox
/// item must not permanently lose the checkpoint. Never ingest pre-existing
/// transcript history on first installation or assign an ambiguous owner.
fn reconcile_checkpoint_outbox(
    store: &JsonlEventStore,
    path: &std::path::Path,
) -> Result<usize, String> {
    let mut outbox = openviking_checkpoint_outbox::Outbox::load(path)?;
    let journal_high_water = store.events().last().map_or(0, |event| event.sequence);
    outbox.activate_durable(path, journal_high_water)?;
    let boundary = outbox
        .started_after_sequence()
        .unwrap_or(journal_high_water);
    // Build a unique-owner index once, and fail closed if two authored
    // conversations ever claim the same turn. No O(history^2) replay.
    let mut owners = BTreeMap::<String, Option<LocalConversationId>>::new();
    for event in store.events() {
        if event.kind != EventKind::UserMessageCommitted {
            continue;
        }
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        match owners.entry(local_turn_scope(message.turn_id)) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Some(message.conversation_id));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if slot.get() != &Some(message.conversation_id) {
                    slot.insert(None);
                }
            }
        }
    }
    let mut recovered = 0;
    for event in store.events() {
        if event.sequence <= boundary || !checkpoint_completion_eligible(event) {
            continue;
        }
        let Some(scope) = event.scope.as_deref() else {
            continue;
        };
        let Some(turn_string) = scope.strip_prefix("local-turn:") else {
            continue;
        };
        let Some(Some(owner)) = owners.get(scope) else {
            continue;
        };
        if outbox.enqueue_durable(path, &owner.to_string(), turn_string, event.sequence)? {
            recovered += 1;
        }
    }
    Ok(recovered)
}

fn controller_record_health_signal(
    notices: &Sender<MirrorControllerNotice>,
    signal: RemoteHealthSignal,
    health: &mut RemoteHealthController,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    notices
        .send(MirrorControllerNotice::HealthSignal {
            signal,
            now_ms: unix_now_ms(),
            reply: reply_tx,
        })
        .map_err(|error| error.to_string())?;
    *health = reply_rx.recv().map_err(|error| error.to_string())??;
    Ok(())
}

fn controller_record_health_intent(
    notices: &Sender<MirrorControllerNotice>,
    intent: MirrorIntent,
    health: &mut RemoteHealthController,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    notices
        .send(MirrorControllerNotice::HealthIntent {
            intent,
            now_ms: unix_now_ms(),
            reply: reply_tx,
        })
        .map_err(|error| error.to_string())?;
    *health = reply_rx.recv().map_err(|error| error.to_string())??;
    Ok(())
}

fn controller_check_remote_health(
    provider: &mut account_bridge::BrowserBridgeProvider,
    notices: &Sender<MirrorControllerNotice>,
    health: &mut RemoteHealthController,
) -> Result<bool, String> {
    let signal = match provider.probe_authentication() {
        Ok(observation) if observation.security_challenge => RemoteHealthSignal::ServerChallenge {
            http_status: observation.http_status,
        },
        Ok(observation)
            if matches!(
                observation.evidence,
                chatarium_core::authenticated_session::SessionAuthenticationEvidence::Authenticated
            ) =>
        {
            RemoteHealthSignal::Healthy {
                http_status: observation.http_status,
            }
        }
        Ok(_) => RemoteHealthSignal::AuthenticationRequired,
        Err(error) => remote_health_signal_from_error(&error),
    };
    controller_record_health_signal(notices, signal, health)?;
    Ok(health.state == chatarium_store::remote_health::RemoteHealthState::Healthy)
}

fn remote_health_signal_from_error(
    error: &account_bridge::BrowserBridgeError,
) -> RemoteHealthSignal {
    match error {
        account_bridge::BrowserBridgeError::RateLimited(_) => {
            RemoteHealthSignal::RateLimited { http_status: 429 }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status == 429 => {
            RemoteHealthSignal::RateLimited {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status == 403 => {
            RemoteHealthSignal::ServerChallenge {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status >= 500 => {
            RemoteHealthSignal::BackendUnavailable {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::Unauthenticated => {
            RemoteHealthSignal::AuthenticationRequired
        }
        account_bridge::BrowserBridgeError::Timeout
        | account_bridge::BrowserBridgeError::Unavailable(_) => RemoteHealthSignal::NetworkUnstable,
        _ => RemoteHealthSignal::Unknown,
    }
}

fn mirror_controller_worker(
    mut provider: account_bridge::BrowserBridgeProvider,
    catalog: Vec<ConversationListItem>,
    events: Vec<EventEnvelope>,
    max_items: Option<usize>,
    commands: Receiver<MirrorControllerCommand>,
    notices: Sender<MirrorControllerNotice>,
) {
    let identities = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    let selected = match derive_remote_mirror_queue(&identities, &events) {
        Ok(summary) => summary.eligible_items(max_items.unwrap_or(usize::MAX)),
        Err(error) => {
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Failed,
                current_catalog_index: None,
                detail: Some(format!("queue replay failed: {error}")),
            });
            return;
        }
    };

    let mut next_item = 0usize;
    let mut running = false;
    let mut paused = false;
    let mut health =
        RemoteHealthController::from_events(&events, unix_now_ms()).unwrap_or_default();
    let _ = notices.send(MirrorControllerNotice::State {
        state: MirrorControllerState::Stopped,
        current_catalog_index: None,
        detail: None,
    });

    loop {
        if !running {
            match commands.recv() {
                Ok(MirrorControllerCommand::Start | MirrorControllerCommand::Resume) => {
                    if let Err(error) = controller_record_health_intent(
                        &notices,
                        MirrorIntent::Enabled,
                        &mut health,
                    ) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: None,
                            detail: Some(format!("remote health intent was not durable: {error}")),
                        });
                        continue;
                    }
                    let now_ms = unix_now_ms();
                    if health.cooldown_active(now_ms) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::RateLimited,
                            current_catalog_index: None,
                            detail: Some("remote health cooldown active; explicit recheck is required after expiry".to_owned()),
                        });
                        continue;
                    }
                    if !health.capture_allowed(now_ms) {
                        match controller_check_remote_health(&mut provider, &notices, &mut health) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(error) => {
                                let _ = notices.send(MirrorControllerNotice::State {
                                    state: MirrorControllerState::Failed,
                                    current_catalog_index: None,
                                    detail: Some(format!("remote health check failed: {error}")),
                                });
                                continue;
                            }
                        }
                    }
                    running = true;
                    paused = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Running,
                        current_catalog_index: None,
                        detail: Some("serial production mirror worker running".to_owned()),
                    });
                }
                Ok(MirrorControllerCommand::Pause) => {
                    paused = true;
                    let _ = controller_record_health_intent(
                        &notices,
                        MirrorIntent::ManuallyPaused,
                        &mut health,
                    );
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Paused,
                        current_catalog_index: None,
                        detail: Some("paused before next item".to_owned()),
                    });
                }
                Ok(MirrorControllerCommand::RecheckHealth) => {
                    if health.cooldown_active(unix_now_ms()) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::RateLimited,
                            current_catalog_index: None,
                            detail: Some(
                                "remote health cooldown active; no request sent".to_owned(),
                            ),
                        });
                    } else if let Err(error) =
                        controller_check_remote_health(&mut provider, &notices, &mut health)
                    {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: None,
                            detail: Some(format!("remote health recheck failed: {error}")),
                        });
                    }
                }
                Ok(MirrorControllerCommand::Shutdown) | Err(_) => return,
            }
            continue;
        }

        while let Ok(command) = commands.try_recv() {
            match command {
                MirrorControllerCommand::Pause => {
                    paused = true;
                    let _ = controller_record_health_intent(
                        &notices,
                        MirrorIntent::ManuallyPaused,
                        &mut health,
                    );
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Paused,
                        current_catalog_index: None,
                        detail: Some("paused before next item".to_owned()),
                    });
                }
                MirrorControllerCommand::Start | MirrorControllerCommand::Resume => {
                    paused = false;
                }
                MirrorControllerCommand::RecheckHealth => {}
                MirrorControllerCommand::Shutdown => return,
            }
        }
        if paused {
            running = false;
            continue;
        }

        let Some(item) = selected.get(next_item).cloned() else {
            running = false;
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Completed,
                current_catalog_index: None,
                detail: Some("no eligible mirror items remain".to_owned()),
            });
            continue;
        };
        next_item += 1;
        let _ = notices.send(MirrorControllerNotice::State {
            state: MirrorControllerState::Running,
            current_catalog_index: Some(item.catalog_index),
            detail: Some("capturing one item serially".to_owned()),
        });

        if let Err(error) =
            controller_prepare_item(&notices, &item.remote_conversation_id, item.catalog_index)
        {
            running = false;
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Failed,
                current_catalog_index: Some(item.catalog_index),
                detail: Some(format!("queue preparation failed: {error}")),
            });
            continue;
        }

        match provider.fetch_authenticated_conversation(&item.remote_conversation_id) {
            Ok(fetched) => {
                match controller_capture_item(&notices, &item.remote_conversation_id, fetched.body)
                {
                    Ok(persisted) => {
                        diagnostics::info(
                            "mirror",
                            format!(
                                "production controller completed catalog item {} state={} snapshot=#{}",
                                item.catalog_index,
                                persisted.mirror_state,
                                persisted.snapshot_sequence
                            ),
                        );
                    }
                    Err(error) => {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: Some(item.catalog_index),
                            detail: Some(format!("local promotion failed: {error}")),
                        });
                    }
                }
            }
            Err(error) => {
                let _ = controller_record_health_signal(
                    &notices,
                    remote_health_signal_from_error(&error),
                    &mut health,
                );
                let class = mirror_failure_class(&error);
                let _ = controller_record_failure(&notices, &item.remote_conversation_id, class);
                if class == MirrorFailureClass::RateLimited {
                    running = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::RateLimited,
                        current_catalog_index: Some(item.catalog_index),
                        detail: Some("HTTP 429; worker stopped without retry loop".to_owned()),
                    });
                } else if matches!(error, account_bridge::BrowserBridgeError::Unauthenticated) {
                    running = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::AuthenticationRequired,
                        current_catalog_index: Some(item.catalog_index),
                        detail: Some("authentication required; worker stopped".to_owned()),
                    });
                }
            }
        }
    }
}

fn controller_prepare_item(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    catalog_index: usize,
) -> Result<(), String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Prepare {
            remote_conversation_id: remote_conversation_id.to_owned(),
            catalog_index,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

fn controller_capture_item(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    body: Value,
) -> Result<MirrorPersisted, String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Capture {
            remote_conversation_id: remote_conversation_id.to_owned(),
            body,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

fn controller_record_failure(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    failure_class: MirrorFailureClass,
) -> Result<(), String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Failure {
            remote_conversation_id: remote_conversation_id.to_owned(),
            failure_class,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

fn controller_reply_channel<T>() -> (Sender<Result<T, String>>, Receiver<Result<T, String>>) {
    mpsc::channel()
}

fn mirror_failure_class(error: &account_bridge::BrowserBridgeError) -> MirrorFailureClass {
    match error {
        account_bridge::BrowserBridgeError::RateLimited(_) => MirrorFailureClass::RateLimited,
        account_bridge::BrowserBridgeError::Protocol(_)
        | account_bridge::BrowserBridgeError::UnsupportedRevision(_) => {
            MirrorFailureClass::Structural
        }
        _ => MirrorFailureClass::Transient,
    }
}

fn start_account_bridge() -> (
    Option<account_bridge::AccountBridgeRuntime>,
    Option<account_bridge::BrowserBridgeProvider>,
    String,
) {
    if !ACCOUNT_MIRRORING_ENABLED {
        return (
            None,
            None,
            "ChatGPT history mirroring paused (local-only)".to_owned(),
        );
    }
    match account_bridge::AccountBridgeRuntime::start() {
        Ok(runtime) => {
            let provider = runtime.provider();
            (
                Some(runtime),
                Some(provider),
                "listener ready · waiting for Edge extension".to_owned(),
            )
        }
        Err(error) => (
            None,
            None,
            format!("browser history bridge unavailable: {error}"),
        ),
    }
}

fn latest_live_mirror_catalog(
    events: &[EventEnvelope],
) -> Result<Vec<LiveMirrorCatalogEntry>, String> {
    let records = replay_remote_conversation_snapshot_audit(events)?;
    let mut catalog = Vec::<LiveMirrorCatalogEntry>::new();

    for record in records {
        let truncated_before = project_remote_active_transcript(&record.envelope)
            .map(|projection| projection.truncated_before)
            .unwrap_or(true);
        let entry = LiveMirrorCatalogEntry {
            local_conversation_id: record.local_conversation_id,
            remote_conversation_id: record.remote_conversation_id.as_str().to_owned(),
            title: if record.envelope.title.trim().is_empty() {
                "Untitled ChatGPT conversation".to_owned()
            } else {
                record.envelope.title.clone()
            },
            snapshot_sequence: record.imported_sequence,
            truncated_before,
        };
        if let Some(existing) = catalog
            .iter_mut()
            .find(|existing| existing.local_conversation_id == entry.local_conversation_id)
        {
            *existing = entry;
        } else {
            catalog.push(entry);
        }
    }

    catalog.sort_by(|left, right| right.snapshot_sequence.cmp(&left.snapshot_sequence));
    Ok(catalog)
}

fn build_remote_catalog_view(
    catalog: &[ConversationListItem],
    events: &[EventEnvelope],
    live_mirrors: &[LiveMirrorCatalogEntry],
) -> Result<Vec<RemoteCatalogViewEntry>, String> {
    let identities = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    let queue = derive_remote_mirror_queue(&identities, events)?;

    Ok(queue
        .items
        .into_iter()
        .filter_map(|queue_item| {
            let item = catalog.get(queue_item.catalog_index)?.clone();
            let mirror = live_mirrors
                .iter()
                .find(|mirror| mirror.remote_conversation_id == item.id);
            Some(RemoteCatalogViewEntry {
                catalog_index: queue_item.catalog_index,
                item,
                status: queue_item.status,
                local_conversation_id: mirror.map(|mirror| mirror.local_conversation_id),
            })
        })
        .collect())
}

fn build_local_archive_search_index(
    catalog_view: &[RemoteCatalogViewEntry],
    events: &[EventEnvelope],
) -> local_archive_search::LocalArchiveSearchIndex {
    let documents = catalog_view
        .iter()
        .map(|entry| {
            let state = match entry.status {
                RemoteMirrorQueueStatus::MirroredFully => {
                    local_archive_search::ArchiveMirrorState::Mirrored
                }
                RemoteMirrorQueueStatus::MirroredPartial => {
                    local_archive_search::ArchiveMirrorState::Partial
                }
                RemoteMirrorQueueStatus::RateLimited => {
                    local_archive_search::ArchiveMirrorState::RateLimited
                }
                RemoteMirrorQueueStatus::TransientFailure => {
                    local_archive_search::ArchiveMirrorState::TransientFailure
                }
                RemoteMirrorQueueStatus::StructuralFailure => {
                    local_archive_search::ArchiveMirrorState::StructuralFailure
                }
                RemoteMirrorQueueStatus::Discovered
                | RemoteMirrorQueueStatus::Queued
                | RemoteMirrorQueueStatus::Capturing => {
                    local_archive_search::ArchiveMirrorState::NotMirrored
                }
            };
            let visible_messages = entry
                .local_conversation_id
                .and_then(|local_id| latest_live_transcript(events, local_id, None).ok())
                .map(|(_, projection)| {
                    projection
                        .messages
                        .into_iter()
                        .map(|message| message.text)
                        .collect()
                })
                .unwrap_or_default();
            local_archive_search::ArchiveSearchDocument {
                catalog_index: entry.catalog_index,
                title: entry
                    .item
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| "Untitled ChatGPT conversation".to_owned()),
                state,
                visible_messages,
            }
        })
        .collect();
    local_archive_search::LocalArchiveSearchIndex::new(documents)
}

fn remote_catalog_state_label(status: RemoteMirrorQueueStatus) -> &'static str {
    match status {
        RemoteMirrorQueueStatus::MirroredFully => "MIRRORED LOCALLY",
        RemoteMirrorQueueStatus::MirroredPartial => "MIRRORED LOCALLY · PARTIAL",
        RemoteMirrorQueueStatus::RateLimited => "RATE LIMITED",
        RemoteMirrorQueueStatus::TransientFailure => "TRANSIENT FAILURE",
        RemoteMirrorQueueStatus::StructuralFailure => "STRUCTURAL FAILURE",
        RemoteMirrorQueueStatus::Queued | RemoteMirrorQueueStatus::Capturing => {
            "REMOTE · MIRRORING"
        }
        RemoteMirrorQueueStatus::Discovered => "REMOTE · NOT MIRRORED",
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct RemoteQueueCounts {
    observed: usize,
    full: usize,
    partial: usize,
    pending: usize,
    transient: usize,
    rate_limited: usize,
    structural: usize,
}

fn remote_catalog_queue_counts(entries: &[RemoteCatalogViewEntry]) -> RemoteQueueCounts {
    let mut counts = RemoteQueueCounts {
        observed: entries.len(),
        ..RemoteQueueCounts::default()
    };
    for entry in entries {
        match entry.status {
            RemoteMirrorQueueStatus::MirroredFully => counts.full += 1,
            RemoteMirrorQueueStatus::MirroredPartial => counts.partial += 1,
            RemoteMirrorQueueStatus::TransientFailure => counts.transient += 1,
            RemoteMirrorQueueStatus::RateLimited => counts.rate_limited += 1,
            RemoteMirrorQueueStatus::StructuralFailure => counts.structural += 1,
            RemoteMirrorQueueStatus::Discovered
            | RemoteMirrorQueueStatus::Queued
            | RemoteMirrorQueueStatus::Capturing => counts.pending += 1,
        }
    }
    counts
}

fn mirror_controller_state_label(state: MirrorControllerState) -> &'static str {
    match state {
        MirrorControllerState::Stopped => "STOPPED",
        MirrorControllerState::Running => "RUNNING · CONCURRENCY=1",
        MirrorControllerState::Paused => "PAUSED",
        MirrorControllerState::RateLimited => "PAUSED · RATE LIMITED",
        MirrorControllerState::AuthenticationRequired => "PAUSED · AUTHENTICATION REQUIRED",
        MirrorControllerState::Completed => "COMPLETED",
        MirrorControllerState::Failed => "STOPPED · FAILURE",
    }
}

fn latest_live_transcript(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
    exact_snapshot_sequence: Option<u64>,
) -> Result<(u64, RemoteTranscriptProjection), String> {
    let records = replay_remote_conversation_snapshot_audit(events)?;
    let record = match exact_snapshot_sequence {
        Some(sequence) => records.iter().find(|record| {
            record.local_conversation_id == local_conversation_id
                && record.imported_sequence == sequence
        }),
        None => records
            .iter()
            .rev()
            .find(|record| record.local_conversation_id == local_conversation_id),
    }
    .ok_or_else(|| {
        format!("no durable live mirror snapshot exists for conversation {local_conversation_id}")
    })?;

    let projection = project_remote_active_transcript(&record.envelope)?;
    Ok((record.imported_sequence, projection))
}

fn load_capability_probe_state(journal_path: &Path) -> capability_probes::ProbeRun {
    let data_dir = journal_path.parent().unwrap_or_else(|| Path::new("."));
    let report_path = data_dir.join("siwc-capability-probes.json");
    let contract_path = data_dir.join("local-inference-contract.json");
    match capability_probes::load_report(&report_path) {
        Ok(Some(mut run)) => {
            let contract_status = match local_inference_contract::save_contract(
                &contract_path,
                &run,
                unix_now_ms(),
            ) {
                Ok(()) => format!(
                    "contract {}",
                    local_inference_contract::contract_state(&run)
                ),
                Err(error) => format!("contract error: {error}"),
            };
            run.status = format!(
                "loaded {} saved capability probe results · {contract_status}",
                run.results.len()
            );
            run
        }
        Ok(None) => capability_probes::ProbeRun::default(),
        Err(error) => capability_probes::ProbeRun {
            status: format!("saved capability probe report ignored · {error}"),
            ..capability_probes::ProbeRun::default()
        },
    }
}

fn load_local_inference_contract_state(
    journal_path: &Path,
) -> Option<local_inference_contract::LoadedContract> {
    let data_dir = journal_path.parent().unwrap_or_else(|| Path::new("."));
    let contract_path = data_dir.join("local-inference-contract.json");
    match local_inference_contract::load_contract(&contract_path) {
        Ok(contract) => contract,
        Err(error) => {
            diagnostics::warn(
                "inference",
                format!("local inference contract ignored: {error}"),
            );
            None
        }
    }
}

fn probe_report_age(generated_unix_ms: u64) -> String {
    let elapsed = unix_now_ms().saturating_sub(generated_unix_ms);
    let seconds = elapsed / 1_000;
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

fn probe_result_detail(result: &capability_probes::ProbeResult) -> String {
    let mut detail = match (&result.code, result.status_code) {
        (Some(code), Some(status)) => format!(" · {code} · HTTP {status}"),
        (Some(code), None) => format!(" · {code}"),
        (None, Some(status)) => format!(" · HTTP {status}"),
        (None, None) => String::new(),
    };
    if let Some(param) = result.param.as_deref() {
        detail.push_str(&format!(" · param {param}"));
    }
    if let Some(request_id) = result.upstream_request_id.as_deref() {
        detail.push_str(&format!(" · request {request_id}"));
    }
    if let Some(response_shape) = result.response_shape.as_deref() {
        detail.push_str(&format!(" · shape {response_shape}"));
    }
    detail
}

fn capability_probe_block_reason(
    remote_connected: bool,
    model_selected: bool,
    pending_remote_turn: bool,
    active_remote_turn: bool,
    probe_running: bool,
) -> Option<&'static str> {
    if !remote_connected {
        return Some("ChatGPT plan connection is not ready");
    }
    if !model_selected {
        return Some("choose an account-visible model");
    }
    if pending_remote_turn || active_remote_turn {
        return Some("finish or stop the current response first");
    }
    if probe_running {
        return Some("capability probe suite is already running");
    }
    None
}

fn model_retry_available(connected: bool, request_pending: bool, models_empty: bool) -> bool {
    connected && !request_pending && models_empty
}

fn should_request_models(
    was_connected: bool,
    account_changed: bool,
    models_empty: bool,
    model_list_pending: bool,
) -> bool {
    (!was_connected || account_changed || models_empty) && !model_list_pending
}

fn session_identity_changed(
    previous: &siwc_bridge::SessionState,
    next: &siwc_bridge::SessionState,
) -> bool {
    previous.profile_id != next.profile_id
        || previous.email != next.email
        || previous.profile_label != next.profile_label
}

fn sign_in_pending_after_session(was_pending: bool, status: &str) -> bool {
    match status {
        "connecting" => true,
        "connected" => false,
        // RefreshSession replies can be stale while a new OAuth request waits
        // on keyring preflight. The sign-in command's terminal result, not a
        // disconnected snapshot, is authoritative for stopping the spinner.
        _ => was_pending,
    }
}

fn failed_bridge_command_finishes_sign_in(request_id: Option<&str>) -> bool {
    request_id == Some("sign-in")
}

fn remote_error_is_observed_failure(error: &siwc_bridge::BridgeError) -> bool {
    error.status.is_some()
        || error.code.starts_with("subscription_sharing_")
        || error.code.starts_with("chatpass_")
        || matches!(
            error.code.as_str(),
            "invalid_request"
                | "invalid_request_error"
                | "model_not_found"
                | "invalid_token"
                | "invalid_api_key"
                | "invalid_client"
                | "sign_in_required"
                | "sharing_not_enabled"
                | "refresh_not_ready"
                | "connection_busy"
                | "response_incomplete"
        )
}

fn remote_turn_payload(
    turn_id: LocalTurnId,
    request_id: &str,
    model: Option<&str>,
    text: Option<&str>,
    detail: Option<&str>,
) -> String {
    serde_json::to_string(&serde_json::json!({
        "schema": "chatarium-responses-turn-observation",
        "version": 1,
        "text": text,
        "details": {
            "local_turn_id": turn_id.to_string(),
            "request_id": request_id,
            "model": model,
            "detail": detail,
        }
    }))
    .expect("remote turn observation is JSON-serializable")
}

fn interrupted_pending_remote_turn(
    pending_remote_turn: &mut Option<PendingRemoteTurn>,
) -> Option<(LocalTurnId, String)> {
    pending_remote_turn.take().map(|pending| {
        let payload = remote_turn_payload(
            pending.turn_id,
            &pending.request_id,
            Some(&pending.model),
            None,
            Some("Sign in with ChatGPT runtime became unavailable before stream dispatch"),
        );
        (pending.turn_id, payload)
    })
}

fn local_conversation_topology(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Option<LocalConversationTopologyRecord>, String> {
    Ok(replay_local_conversation_topologies(events)?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id))
}

fn next_available_local_orchestration_ids(
    events: &[EventEnvelope],
) -> Result<(ChatContainerId, SessionId), String> {
    let next_container = replay_chat_container_audit(events)?
        .into_iter()
        .map(|record| record.container_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "chat-container identity space exhausted".to_owned())?;
    let next_session = replay_session_audit(events)?
        .into_iter()
        .map(|record| record.session_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "session identity space exhausted".to_owned())?;
    Ok((
        ChatContainerId::new(next_container),
        SessionId::new(next_session),
    ))
}

fn next_available_continuation_lease_id(
    events: &[EventEnvelope],
) -> Result<ContinuationLeaseId, String> {
    let highest = replay_continuation_audit(events)?
        .into_iter()
        .map(|record| record.lease_id.get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "continuation lease identity space exhausted".to_owned())?;
    Ok(ContinuationLeaseId::new(next))
}

fn next_available_coordination_suggestion_id(
    events: &[EventEnvelope],
) -> Result<CoordinationSuggestionId, String> {
    let highest = replay_controller_coordination_suggestion_audit(events)?
        .into_iter()
        .map(|record| record.suggestion.id().get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "coordination suggestion identity space exhausted".to_owned())?;
    Ok(CoordinationSuggestionId::new(next))
}

fn next_available_control_id(events: &[EventEnvelope]) -> Result<ControlId, String> {
    let highest = replay_control_audit(events)?
        .into_iter()
        .map(|record| record.control_id.get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "control identity space exhausted".to_owned())?;
    Ok(ControlId::new(next))
}

fn next_available_tool_provider_id(events: &[EventEnvelope]) -> Result<ToolProviderId, String> {
    let next = replay_tool_provider_audit(events)?
        .into_iter()
        .map(|record| record.provider_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "tool provider identity space exhausted".to_owned())?;
    Ok(ToolProviderId::new(next))
}

fn next_available_tool_call_id(events: &[EventEnvelope]) -> Result<ToolCallId, String> {
    let next = replay_tool_call_audit(events)?
        .into_iter()
        .map(|record| record.call_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "tool call identity space exhausted".to_owned())?;
    Ok(ToolCallId::new(next))
}

fn next_available_local_memory_id(events: &[EventEnvelope]) -> Result<LocalMemoryId, String> {
    let next = replay_local_memory_audit(events)?
        .into_iter()
        .map(|record| record.memory_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "local memory identity space exhausted".to_owned())?;
    Ok(LocalMemoryId::new(next))
}

fn next_available_route_payload_id(events: &[EventEnvelope]) -> Result<RoutePayloadId, String> {
    let next = replay_local_route_payload_audit(events)?
        .into_iter()
        .map(|payload| payload.payload_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "route payload identity space exhausted".to_owned())?;
    Ok(RoutePayloadId::new(next))
}

fn next_available_route_id(events: &[EventEnvelope]) -> Result<RouteId, String> {
    let next = replay_routing_audit(events)?
        .into_iter()
        .map(|route| route.request.id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "route identity space exhausted".to_owned())?;
    Ok(RouteId::new(next))
}

fn next_available_route_endpoint_id(events: &[EventEnvelope]) -> Result<RouteEndpointId, String> {
    let mut highest = 0_u64;

    for record in replay_session_audit(events)? {
        if let Some(binding) = record.endpoint_binding {
            highest = highest.max(binding.endpoint_id().get());
        }
    }
    for provider in replay_tool_provider_audit(events)? {
        if let Some(binding) = provider.endpoint_binding {
            highest = highest.max(binding.endpoint_id().get());
        }
    }
    for route in replay_routing_audit(events)? {
        highest = highest
            .max(route.request.source.get())
            .max(route.request.destination.get());
    }

    let next = highest
        .checked_add(1)
        .ok_or_else(|| "routing endpoint identity space exhausted".to_owned())?;
    Ok(RouteEndpointId::new(next))
}

fn session_endpoint_binding(
    events: &[EventEnvelope],
    session_id: SessionId,
) -> Result<Option<SessionEndpointBinding>, String> {
    Ok(replay_session_audit(events)?
        .into_iter()
        .find(|record| record.session_id == session_id)
        .and_then(|record| record.endpoint_binding))
}

fn controller_worker_result_context_decision_label(
    decision: ControllerWorkerResultContextDecision,
) -> &'static str {
    match decision {
        ControllerWorkerResultContextDecision::Admit => "admit",
        ControllerWorkerResultContextDecision::Exclude => "exclude",
    }
}

fn coordination_suggestion_action_label(action: CoordinationSuggestionAction) -> &'static str {
    match action {
        CoordinationSuggestionAction::StartOrResume => "START/RESUME",
        CoordinationSuggestionAction::Continue => "CONTINUE",
        CoordinationSuggestionAction::Stop => "STOP",
        CoordinationSuggestionAction::StatusRequest => "STATUS",
    }
}

fn controller_coordination_result_context_decision_label(
    decision: ControllerCoordinationResultContextDecision,
) -> &'static str {
    match decision {
        ControllerCoordinationResultContextDecision::Admit => "admit",
        ControllerCoordinationResultContextDecision::Exclude => "exclude",
    }
}

fn local_memory_context_decision_label(decision: LocalMemoryContextDecision) -> &'static str {
    match decision {
        LocalMemoryContextDecision::Admit => "admit",
        LocalMemoryContextDecision::Exclude => "exclude",
    }
}

fn local_route_context_decision_label(decision: LocalRouteContextDecision) -> &'static str {
    match decision {
        LocalRouteContextDecision::Admit => "admit",
        LocalRouteContextDecision::Exclude => "exclude",
    }
}

fn worker_control_kind_label(kind: WorkerControlKind) -> &'static str {
    match kind {
        WorkerControlKind::StartOrResume => "START/RESUME",
        WorkerControlKind::Continue { .. } => "CONTINUE",
        WorkerControlKind::Stop => "STOP",
        WorkerControlKind::StatusRequest => "STATUS",
    }
}

fn route_user_decision_label(decision: RouteUserDecision) -> &'static str {
    match decision {
        RouteUserDecision::Allow => "allow",
        RouteUserDecision::Deny => "deny",
    }
}

fn route_gate_state_label(state: RouteGateState) -> &'static str {
    match state {
        RouteGateState::PendingApproval => "PENDING APPROVAL",
        RouteGateState::Allowed {
            by: DecisionAuthority::Policy,
        } => "ALLOWED · POLICY",
        RouteGateState::Allowed {
            by: DecisionAuthority::User,
        } => "ALLOWED · USER",
        RouteGateState::Denied {
            by: DecisionAuthority::Policy,
        } => "DENIED · POLICY",
        RouteGateState::Denied {
            by: DecisionAuthority::User,
        } => "DENIED · USER",
        RouteGateState::Dispatched {
            authorized_by: DecisionAuthority::Policy,
        } => "DISPATCHED · POLICY",
        RouteGateState::Dispatched {
            authorized_by: DecisionAuthority::User,
        } => "DISPATCHED · USER",
    }
}

fn session_lifecycle_phase_label(phase: SessionLifecyclePhase) -> &'static str {
    match phase {
        SessionLifecyclePhase::Healthy => "HEALTHY",
        SessionLifecyclePhase::Aging => "AGING",
        SessionLifecyclePhase::Saturated => "SATURATED",
        SessionLifecyclePhase::Retired => "RETIRED",
    }
}

fn local_worker_binding(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Option<LocalConversationWorkerBindingRecord>, String> {
    Ok(replay_local_conversation_worker_bindings(events)?
        .into_iter()
        .find(|binding| binding.conversation_id == conversation_id))
}

fn local_worker_session_alignment(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<(SessionId, Option<SessionId>), String> {
    let topology = local_conversation_topology(events, conversation_id)?.ok_or_else(|| {
        format!("local conversation {conversation_id} has no orchestration topology")
    })?;
    let sessions = replay_session_audit(events)?;
    let active = active_worker_session(&sessions, worker_id).map(|record| record.session_id);
    Ok((topology.current_session_id, active))
}

fn worker_record(
    events: &[EventEnvelope],
    worker_id: WorkerId,
) -> Result<Option<WorkerAuditRecord>, String> {
    Ok(replay_worker_audit(events)?
        .into_iter()
        .find(|record| record.worker_id == worker_id))
}

fn next_available_worker_id(events: &[EventEnvelope]) -> Result<WorkerId, String> {
    let mut highest = 0_u64;

    for binding in replay_local_conversation_worker_bindings(events)? {
        highest = highest.max(binding.worker_id.get());
    }
    for record in replay_worker_audit(events)? {
        highest = highest.max(record.worker_id.get());
    }
    for record in replay_session_audit(events)? {
        if let Some(binding) = record.worker_binding {
            highest = highest.max(binding.worker_id().get());
        }
    }

    let next = highest
        .checked_add(1)
        .ok_or_else(|| "worker identity space exhausted".to_owned())?;
    Ok(WorkerId::new(next))
}

fn worker_phase_label(phase: WorkerPhase) -> &'static str {
    match phase {
        WorkerPhase::Unassigned => "UNASSIGNED",
        WorkerPhase::Ready => "READY",
        WorkerPhase::Working => "WORKING",
        WorkerPhase::NeedsInput => "NEEDS INPUT",
        WorkerPhase::Blocked => "BLOCKED",
        WorkerPhase::Completed => "COMPLETED",
        WorkerPhase::Failed => "FAILED",
        WorkerPhase::Stopped => "STOPPED",
    }
}

fn context_transcript(messages: &[DisplayMessage]) -> Vec<context_composer::TranscriptMessage> {
    messages
        .iter()
        .filter_map(|message| {
            // MCP records are reader/audit evidence, not a synthetic message
            // from the user. The separately verified admission pipeline alone
            // supplies tool results to the model.
            let role = match message.role {
                DisplayRole::User => context_composer::TranscriptRole::User,
                DisplayRole::Assistant => context_composer::TranscriptRole::Assistant,
                DisplayRole::Mcp | DisplayRole::Activity => return None,
            };
            Some(context_composer::TranscriptMessage::durable(
                role,
                message.text.clone(),
                message.sequence,
            ))
        })
        .collect()
}

fn admitted_tool_result_context_messages(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    Ok(replay_admitted_tool_results(events, conversation_id)?
        .into_iter()
        .map(|outcome| {
            let record = outcome.record;
            context_composer::TranscriptMessage::tool_result(
                outcome.text.as_str(),
                record.call_id.get(),
                record.route_id.get(),
                record.provider_id.get(),
                record.source_session_id.get(),
                record.outcome_sequence,
                record.last_decision_sequence,
                record.outcome_kind.stable_name(),
            )
        })
        .collect())
}

fn admitted_routed_context_messages(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let inbox = replay_local_routed_inbox_for_conversation(events, conversation_id)?;
    let admitted = replay_admitted_local_route_context(events, conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let item = inbox
            .iter()
            .find(|item| item.route_id == record.route_id)
            .ok_or_else(|| {
                format!(
                    "admitted routed context for route {} has no delivered inbox item",
                    record.route_id.get()
                )
            })?;
        if item.payload_id != record.payload_id
            || item.source_conversation_id != record.source_conversation_id
            || item.destination_conversation_id != record.destination_conversation_id
            || item.source_session_id != record.source_session_id
            || item.destination_session_id != record.destination_session_id
            || item.delivered_sequence != record.delivered_sequence
        {
            return Err(format!(
                "admitted routed context for route {} disagrees with delivered inbox provenance",
                record.route_id.get()
            ));
        }

        messages.push(context_composer::TranscriptMessage::routed(
            item.text.as_str(),
            record.route_id.get(),
            record.payload_id.get(),
            record.source_conversation_id.to_string(),
            record.delivered_sequence,
            record.last_decision_sequence,
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

fn snapshot_one_shot_local_memory(
    events: &[EventEnvelope],
    destination_conversation_id: LocalConversationId,
    selected_memory_ids: &BTreeSet<LocalMemoryId>,
) -> Result<(Vec<PendingOneShotMemory>, Option<u64>), String> {
    if selected_memory_ids.is_empty() {
        return Ok((Vec::new(), None));
    }

    let snapshot_after_sequence = events.last().map(|event| event.sequence).unwrap_or(0);
    let memory_ids = selected_memory_ids.iter().copied().collect::<Vec<_>>();
    let artifacts = validate_local_memory_one_shot_snapshot(
        events,
        destination_conversation_id,
        &memory_ids,
        snapshot_after_sequence,
    )?;

    let pending = artifacts
        .into_iter()
        .map(|artifact| PendingOneShotMemory {
            memory_id: artifact.memory_id,
            source_conversation_id: artifact.source_conversation_id,
            text: artifact.text,
            artifact_sequence: artifact.recorded_sequence,
        })
        .collect::<Vec<_>>();

    Ok((pending, Some(snapshot_after_sequence)))
}

fn one_shot_local_memory_messages_for_turn(
    events: &[EventEnvelope],
    turn_id: LocalTurnId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let Some(selection) = replay_local_memory_turn_selection_audit(events)?
        .into_iter()
        .find(|record| record.turn_id == turn_id)
    else {
        return Ok(Vec::new());
    };

    let artifacts = replay_local_memory_audit(events)?
        .into_iter()
        .map(|artifact| (artifact.memory_id, artifact))
        .collect::<BTreeMap<_, _>>();
    let mut messages = Vec::with_capacity(selection.memory_ids.len());

    for memory_id in &selection.memory_ids {
        let artifact = artifacts.get(memory_id).ok_or_else(|| {
            format!(
                "one-shot local memory turn {} references missing immutable memory {}",
                turn_id,
                memory_id.get()
            )
        })?;
        messages.push(context_composer::TranscriptMessage::local_memory_one_shot(
            artifact.text.as_str(),
            artifact.memory_id.get(),
            artifact.source_conversation_id.to_string(),
            artifact.recorded_sequence,
            selection.selection_snapshot_after_sequence,
            Some(selection.recorded_sequence),
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

fn admitted_local_memory_messages(
    events: &[EventEnvelope],
    destination_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let artifacts = replay_local_memory_audit(events)?;
    let admitted = replay_admitted_local_memory_context(events, destination_conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.memory_id == record.memory_id)
            .ok_or_else(|| {
                format!(
                    "admitted local memory {} has no immutable artifact",
                    record.memory_id.get()
                )
            })?;
        if artifact.source_conversation_id != record.source_conversation_id
            || artifact.recorded_sequence != record.artifact_sequence
        {
            return Err(format!(
                "admitted local memory {} disagrees with artifact provenance",
                record.memory_id.get()
            ));
        }

        messages.push(context_composer::TranscriptMessage::local_memory(
            artifact.text.as_str(),
            record.memory_id.get(),
            record.source_conversation_id.to_string(),
            record.artifact_sequence,
            record.last_decision_sequence,
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

fn admitted_controller_worker_result_messages(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let results =
        replay_controller_worker_results_for_conversation(events, controller_conversation_id)?;
    let admitted =
        replay_admitted_controller_worker_result_context(events, controller_conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let result = results
            .iter()
            .find(|item| item.route_id == record.route_id)
            .ok_or_else(|| {
                format!(
                    "admitted controller result route {} has no terminal result",
                    record.route_id.get()
                )
            })?;
        if result.control_id != record.control_id
            || result.controller_conversation_id != Some(controller_conversation_id)
            || result.result_sequence != record.result_sequence
        {
            return Err(format!(
                "admitted controller result route {} disagrees with terminal result provenance",
                record.route_id.get()
            ));
        }

        let (result_kind, result_text) = match &result.detail {
            ControllerWorkerResultDetail::Status {
                phase,
                acknowledged_sequence,
            } => (
                "status",
                format!(
                    "phase: {}\nacknowledged_event: #{}",
                    worker_phase_label(*phase),
                    acknowledged_sequence,
                ),
            ),
            ControllerWorkerResultDetail::Action {
                kind,
                from_phase,
                resulting_phase,
                started_sequence,
                lifecycle_sequence,
            } => (
                "action",
                format!(
                    "control_kind: {}\nfrom_phase: {}\nresulting_phase: {}\naction_started_event: #{}\nlifecycle_event: #{}",
                    worker_control_kind_label(*kind),
                    worker_phase_label(*from_phase),
                    worker_phase_label(*resulting_phase),
                    started_sequence,
                    lifecycle_sequence,
                ),
            ),
            ControllerWorkerResultDetail::Continuation {
                outcome,
                execution_turn_id,
                lease_id,
                permit_ordinal,
                started_sequence,
                terminal_sequence,
                output_text,
            } => {
                let output = output_text
                    .as_deref()
                    .unwrap_or("(no durable assistant output)");
                (
                    "continuation",
                    format!(
                        "outcome: {}\ncontinuation_lease: {}\npermit_ordinal: {}\nexecution_turn_id: {}\nexecution_started_event: #{}\nterminal_transport_event: #{}\nworker_output:\n{}",
                        outcome.stable_name(),
                        lease_id.get(),
                        permit_ordinal,
                        execution_turn_id,
                        started_sequence,
                        terminal_sequence,
                        output,
                    ),
                )
            }
        };

        messages.push(
            context_composer::TranscriptMessage::controller_worker_result(
                result.control_id.get(),
                result.route_id.get(),
                result.worker_id.get(),
                result.goal_id.get(),
                result.result_sequence,
                record.last_decision_sequence,
                result_kind,
                result_text.as_str(),
            ),
        );
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

fn admitted_controller_coordination_result_messages(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let admitted =
        replay_admitted_controller_coordination_result_context(events, controller_conversation_id)?;
    let coordinations = replay_controller_coordination_audit(events)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let coordination = coordinations
            .iter()
            .find(|item| item.coordination_turn_id == record.coordination_turn_id)
            .ok_or_else(|| {
                format!(
                    "admitted controller coordination turn {} has no durable execution",
                    record.coordination_turn_id
                )
            })?;
        if coordination.controller_conversation_id != controller_conversation_id
            || coordination.result_sequence != Some(record.result_sequence)
        {
            return Err(format!(
                "admitted controller coordination turn {} disagrees with terminal result provenance",
                record.coordination_turn_id
            ));
        }
        let outcome = coordination.outcome.ok_or_else(|| {
            format!(
                "admitted controller coordination turn {} has no terminal outcome",
                record.coordination_turn_id
            )
        })?;
        let output = controller_coordination_output_text(events, record.coordination_turn_id)?
            .unwrap_or_else(|| "(no durable coordination output)".to_owned());

        messages.push(
            context_composer::TranscriptMessage::controller_coordination_result(
                record.coordination_turn_id.to_string(),
                record.result_sequence,
                record.last_decision_sequence,
                outcome.stable_name(),
                output.as_str(),
            ),
        );
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

fn projected_working_draft(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> String {
    let scope = local_conversation_scope(conversation_id);
    let scoped_draft = events.iter().rev().find(|event| {
        event.kind == EventKind::DraftChanged && event.scope.as_deref() == Some(scope.as_str())
    });
    let legacy_owner = projected_local_conversation_id(events).ok().flatten();
    let latest_draft = scoped_draft.or_else(|| {
        (legacy_owner == Some(conversation_id))
            .then(|| {
                events
                    .iter()
                    .rev()
                    .find(|event| event.kind == EventKind::DraftChanged && event.scope.is_none())
            })
            .flatten()
    });
    let latest_commit_sequence = events
        .iter()
        .filter_map(|event| {
            let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                decode_user_message_commit(event)
            else {
                return None;
            };
            (message.conversation_id == conversation_id).then_some(event.sequence)
        })
        .next_back()
        .unwrap_or_default();

    match latest_draft {
        Some(event) if event.sequence > latest_commit_sequence => event_text(&event.payload),
        _ => String::new(),
    }
}

fn recover_interrupted_remote_turns(store: &mut impl EventStore) -> Result<usize, String> {
    let mut authored_turns = HashSet::new();
    for event in store.events() {
        let Some(decoded) = decode_user_message_commit(event)? else {
            continue;
        };
        if let DecodedUserMessageCommit::Typed(message) = decoded {
            authored_turns.insert(message.turn_id);
        }
    }

    let mut interrupted = Vec::new();
    for turn_id in authored_turns {
        let scope = local_turn_scope(turn_id);
        let kinds = store
            .events()
            .iter()
            .filter(|event| event.scope.as_deref() == Some(scope.as_str()))
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        // Only the most recent dispatch step determines recovery. An earlier
        // failed/completed model attempt cannot mask an interrupted MCP
        // continuation under the same user-authored turn.
        let latest_dispatch = kinds
            .iter()
            .rposition(|kind| *kind == EventKind::DispatchAttempted)
            .unwrap_or(kinds.len());
        let already_interrupted =
            kinds[latest_dispatch..].contains(&EventKind::TransportInterrupted);
        let evidence = TurnEvidence::replay_event_kinds(kinds)
            .map_err(|error| format!("turn {turn_id} replay failed: {error}"))?;

        let remote_still_live = !already_interrupted
            && matches!(
                evidence.remote,
                RemoteEvidence::Dispatching | RemoteEvidence::AcceptedObserved
            )
            && evidence.assistant != AssistantEvidence::CompletedObserved;

        if remote_still_live {
            interrupted.push(turn_id);
        }
    }

    let mut continuation_interrupted = Vec::new();
    for execution in replay_worker_continuation_execution_audit(store.events())? {
        let transport =
            continuation_execution_transport_state(store.events(), execution.execution_turn_id)?;
        if transport.was_dispatched() && !transport.is_terminal() {
            continuation_interrupted.push(execution.execution_turn_id);
        }
    }

    let mut coordination_interrupted = Vec::new();
    for coordination in replay_controller_coordination_audit(store.events())? {
        let transport = controller_coordination_transport_state(
            store.events(),
            coordination.coordination_turn_id,
        )?;
        if transport.was_dispatched() && !transport.is_terminal() {
            coordination_interrupted.push(coordination.coordination_turn_id);
        }
    }

    for turn_id in &interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this remote turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!("failed to persist restart interruption for turn {turn_id}: {error}")
            })?;
    }

    for turn_id in &continuation_interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this bounded continuation turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!(
                    "failed to persist restart interruption for continuation turn {turn_id}: {error}"
                )
            })?;
    }

    for turn_id in &coordination_interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this non-authored controller coordination turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!(
                    "failed to persist restart interruption for controller coordination turn {turn_id}: {error}"
                )
            })?;
    }

    let continuation_turn_ids = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .map(|record| record.execution_turn_id)
        .collect::<Vec<_>>();
    for turn_id in continuation_turn_ids {
        append_worker_continuation_result_if_terminal(store, turn_id)?;
    }

    let coordination_turn_ids = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .map(|record| record.coordination_turn_id)
        .collect::<Vec<_>>();
    for turn_id in coordination_turn_ids {
        append_controller_coordination_result_if_terminal(store, turn_id)?;
    }

    Ok(interrupted.len() + continuation_interrupted.len() + coordination_interrupted.len())
}

fn projected_local_conversation_id(
    events: &[EventEnvelope],
) -> Result<Option<LocalConversationId>, String> {
    for event in events.iter().rev() {
        match decode_user_message_commit(event)? {
            Some(DecodedUserMessageCommit::Typed(message)) => {
                return Ok(Some(message.conversation_id));
            }
            Some(DecodedUserMessageCommit::LegacyText(_)) | None => {}
        }
    }
    Ok(None)
}

const REMOTE_HISTORY_CACHE_FILE: &str = "remote-history-cache.json";

fn remote_history_cache_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(REMOTE_HISTORY_CACHE_FILE)
}

fn local_reader_state_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-reader-state.json")
}

fn local_inference_settings_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-inference-settings.json")
}

fn local_behavior_profile_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("behavior-profiles.json")
}

fn local_conversation_catalog_path(journal_path: &Path) -> PathBuf {
    journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("local-conversations.json")
}

fn local_conversation_scope(conversation_id: LocalConversationId) -> String {
    format!("local-conversation:{conversation_id}")
}

fn discovered_local_conversation_ids(
    events: &[EventEnvelope],
) -> Result<Vec<LocalConversationId>, String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for event in events {
        let Some(decoded) = decode_user_message_commit(event)? else {
            continue;
        };
        if let DecodedUserMessageCommit::Typed(message) = decoded {
            if seen.insert(message.conversation_id) {
                ids.push(message.conversation_id);
            }
        }
    }
    Ok(ids)
}

fn local_conversation_display_title(
    catalog: &local_conversations::LocalConversationCatalog,
    conversation_id: LocalConversationId,
    events: &[EventEnvelope],
) -> String {
    catalog
        .entry(conversation_id)
        .and_then(|entry| entry.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            derived_conversation_title(&projected_local_display_messages(events, conversation_id))
        })
}

fn load_local_conversation_workspace(
    path: &Path,
    events: &[EventEnvelope],
) -> Result<
    (
        local_conversations::LocalConversationCatalog,
        LocalConversationId,
    ),
    String,
> {
    let mut catalog = local_conversations::LocalConversationCatalog::load(path)?;
    let now_ms = unix_now_ms();
    let mut changed = false;
    for conversation_id in discovered_local_conversation_ids(events)? {
        let title =
            derived_conversation_title(&projected_local_display_messages(events, conversation_id));
        changed |= catalog.ensure(conversation_id, Some(title), now_ms);
    }

    let mut active = catalog
        .active()
        .filter(|id| catalog.entry(*id).is_some_and(|entry| !entry.archived));
    if active.is_none() {
        active = catalog.first_unarchived();
    }
    let active = match active {
        Some(id) => id,
        None => {
            let id = LocalConversationId::new();
            catalog.create(id, now_ms);
            changed = true;
            id
        }
    };

    if catalog.active() != Some(active) {
        catalog.set_active(active, now_ms)?;
        changed = true;
    }
    if changed || !path.exists() {
        catalog.save_atomic(path)?;
    }
    Ok((catalog, active))
}

fn merge_history_discovery_catalog(
    existing: Vec<ConversationListItem>,
    candidates: &[account_bridge::HistorySurfaceCandidate],
) -> (Vec<ConversationListItem>, usize) {
    let mut catalog = existing
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut current_pass_ids = HashSet::new();

    for candidate in candidates {
        for item in &candidate.items {
            current_pass_ids.insert(item.id.clone());
            catalog
                .entry(item.id.clone())
                .or_insert_with(|| item.clone());
        }
    }

    (catalog.into_values().collect(), current_pass_ids.len())
}

fn load_remote_history_cache(path: &Path) -> Result<Vec<ConversationListItem>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-remote-history-cache")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(format!(
            "unsupported remote history cache schema in {}",
            path.display()
        ));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("remote history cache {} is missing items", path.display()))?;

    let mut catalog = BTreeMap::new();
    for (index, item) in items.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            format!(
                "remote history cache {} item {index} is not an object",
                path.display()
            )
        })?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 512)
            .ok_or_else(|| {
                format!(
                    "remote history cache {} item {index} has invalid id",
                    path.display()
                )
            })?
            .to_owned();
        let title = match object.get("title") {
            Some(Value::String(title)) => Some(title.clone()),
            Some(Value::Null) | None => None,
            Some(_) => {
                return Err(format!(
                    "remote history cache {} item {index} has invalid title",
                    path.display()
                ));
            }
        };
        let optional_value = |field: &str| match object.get(field) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.clone()),
        };
        catalog.entry(id.clone()).or_insert(ConversationListItem {
            id,
            title,
            create_time: optional_value("create_time"),
            update_time: optional_value("update_time"),
        });
    }

    Ok(catalog.into_values().collect())
}

fn persist_remote_history_cache(path: &Path, items: &[ConversationListItem]) -> Result<(), String> {
    let body = serde_json::json!({
        "schema": "chatarium-remote-history-cache",
        "version": 1,
        "items": items
            .iter()
            .map(|item| serde_json::json!({
                "id": item.id,
                "title": item.title,
                "create_time": item.create_time,
                "update_time": item.update_time,
            }))
            .collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|error| format!("failed to encode remote history cache: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create remote history cache directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "failed to write temporary remote history cache {}: {error}",
            temporary.display()
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        format!(
            "failed to replace remote history cache {}: {error}",
            path.display()
        )
    })
}

fn default_journal_path() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("CHATARIUM_DATA_DIR") {
        return PathBuf::from(override_dir).join("journal.jsonl");
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Chatarium")
            .join("journal.jsonl");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".chatarium")
        .join("journal.jsonl")
}

fn projected_local_display_messages(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Vec<DisplayMessage> {
    // Every assistant scope must have exactly one native conversation owner.
    // A duplicate turn ID claimed across conversations is ambiguous and must
    // never make assistant text visible in either transcript or model context.
    // User-authored messages remain visible to their original conversation.
    let mut owners = BTreeMap::<String, Option<LocalConversationId>>::new();
    for event in events {
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        match owners.entry(local_turn_scope(message.turn_id)) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(message.conversation_id));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry
                    .get()
                    .is_some_and(|previous| previous != message.conversation_id)
                {
                    entry.insert(None);
                }
            }
        }
    }
    let local_turn_scopes = owners
        .into_iter()
        .filter_map(|(scope, owner)| (owner == Some(local_conversation_id)).then_some(scope))
        .collect::<HashSet<_>>();

    // Project borrowed journal events directly; cloning large raw payloads
    // into a temporary vector is unnecessary during transcript rendering
    // and before request-context composition.
    projected_display_messages_iter(events.iter().filter(|event| {
        match event.kind {
            EventKind::UserMessageCommitted => matches!(
                decode_user_message_commit(event),
                Ok(Some(DecodedUserMessageCommit::Typed(message)))
                    if message.conversation_id == local_conversation_id
            ),
            EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => event
                .scope
                .as_ref()
                .is_some_and(|scope| local_turn_scopes.contains(scope)),
            _ => false,
        }
    }))
}

fn projected_display_messages(events: &[EventEnvelope]) -> Vec<DisplayMessage> {
    projected_display_messages_iter(events.iter())
}

/// Metadata-only native reader provenance for a durably committed file.
/// The complete base64 payload stays in the journal, never in the transcript.
fn authored_attachment_provenance(event: &EventEnvelope) -> Option<String> {
    if event.kind != EventKind::UserMessageCommitted {
        return None;
    }
    let attachment = decode_user_message_attachment(event).ok()??;
    let filename = attachment.get("filename")?.as_str()?;
    let bytes = attachment.get("byte_len")?.as_u64()?;
    let read_ms = attachment.get("read_elapsed_ms")?.as_f64()?;
    let encode_ms = attachment.get("encoding_elapsed_ms")?.as_f64()?;
    Some(format!(
        "local journal · authored · attached {filename} ({bytes} bytes; read {read_ms:.1} ms, encode {encode_ms:.1} ms)"
    ))
}

fn projected_display_messages_iter<'a>(
    events: impl IntoIterator<Item = &'a EventEnvelope>,
) -> Vec<DisplayMessage> {
    let mut messages = Vec::<DisplayMessage>::new();
    let mut keyed = BTreeMap::<String, usize>::new();

    for event in events {
        let role = match event.kind {
            EventKind::UserMessageCommitted | EventKind::TranscriptUserMessageObserved => {
                DisplayRole::User
            }
            EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => {
                DisplayRole::Assistant
            }
            _ => continue,
        };

        let text = event_text(&event.payload);
        if text.trim().is_empty() {
            continue;
        }

        let key = payload_message_identity(&event.payload, role)
            .map(|identity| format!("{role:?}:{identity}"))
            .unwrap_or_else(|| format!("event:{}", event.sequence));

        if let Some(index) = keyed.get(&key).copied() {
            messages[index].text = text;
            messages[index].sequence = event.sequence;
            continue;
        }

        keyed.insert(key, messages.len());
        messages.push(DisplayMessage {
            role,
            text,
            sequence: event.sequence,
            timestamp: Some(event.at_unix_ms as f64 / 1000.0),
            provenance_label: Some(match role {
                DisplayRole::User => authored_attachment_provenance(event)
                    .unwrap_or_else(|| "local journal · authored".to_owned()),
                DisplayRole::Assistant => "ChatGPT Responses · remote assistant".to_owned(),
                DisplayRole::Mcp => "verified local MCP audit".to_owned(),
                DisplayRole::Activity => "verified SIWC lifecycle observation".to_owned(),
            }),
        });
    }

    messages
}

fn historical_display_messages(
    messages: Vec<HistoricalTranscriptMessage>,
    imported_sequence: u64,
) -> Vec<DisplayMessage> {
    messages
        .into_iter()
        .map(|message| DisplayMessage {
            role: match message.role {
                HistoricalTranscriptRole::User => DisplayRole::User,
                HistoricalTranscriptRole::Assistant => DisplayRole::Assistant,
            },
            text: message.text,
            sequence: imported_sequence,
            timestamp: message.create_time,
            provenance_label: Some(format!(
                "historical snapshot · import event #{imported_sequence}"
            )),
        })
        .collect()
}

fn remote_display_messages(
    messages: Vec<RemoteTranscriptMessage>,
    snapshot_sequence: u64,
) -> Vec<DisplayMessage> {
    messages
        .into_iter()
        .map(|message| DisplayMessage {
            role: match message.role {
                RemoteTranscriptRole::User => DisplayRole::User,
                RemoteTranscriptRole::Assistant => DisplayRole::Assistant,
            },
            text: message.text,
            sequence: snapshot_sequence,
            timestamp: Some(message.create_time),
            provenance_label: Some(format!(
                "live mirror · remote snapshot event #{snapshot_sequence}"
            )),
        })
        .collect()
}

fn payload_message_identity(payload: &str, role: DisplayRole) -> Option<String> {
    let value = serde_json::from_str::<Value>(payload).ok()?;
    let paths: &[&str] = match role {
        DisplayRole::User => &["/details/observed_message_id", "/details/observed_id"],
        DisplayRole::Assistant => &["/details/local_turn_id", "/details/observed_id"],
        DisplayRole::Mcp | DisplayRole::Activity => return None,
    };

    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_str))
        .filter(|identity| !identity.is_empty())
        .map(ToOwned::to_owned)
}

fn derived_conversation_title(messages: &[DisplayMessage]) -> String {
    let Some(first) = messages
        .iter()
        .find(|message| message.role == DisplayRole::User)
    else {
        return "New local conversation".to_owned();
    };

    // Build at most the first 42 Unicode scalar values of the normalized
    // title. A large pasted first message must not allocate or scan the
    // entire body just to derive the sidebar label on each redraw.
    const LIMIT: usize = 42;
    let mut title = String::new();
    let mut count = 0;
    let mut has_word = false;
    for word in first.text.split_whitespace() {
        if has_word {
            if count == LIMIT {
                title.push('…');
                return title;
            }
            title.push(' ');
            count += 1;
        }
        for character in word.chars() {
            if count == LIMIT {
                title.push('…');
                return title;
            }
            title.push(character);
            count += 1;
        }
        has_word = true;
    }
    if title.is_empty() {
        "Local conversation".to_owned()
    } else {
        title
    }
}

/// Show an expanded first-line title only when the persisted title is the
/// default 42-scalar generated preview. Explicit renames remain untouched.
/// The egui label alone handles truncation to the currently available width.
fn expanded_native_conversation_header_title(
    stored_title: &str,
    messages: &[DisplayMessage],
) -> String {
    if !stored_title.ends_with('…') || stored_title != derived_conversation_title(messages) {
        return stored_title.to_owned();
    }

    let Some(first) = messages
        .iter()
        .find(|message| message.role == DisplayRole::User)
    else {
        return stored_title.to_owned();
    };
    let Some(first_line) = first.text.lines().find(|line| !line.trim().is_empty()) else {
        return stored_title.to_owned();
    };

    // Avoid putting a full pasted prompt into the header. This upper bound
    // only protects pathological single-line messages, not ordinary titles.
    const MAX_HEADER_SCALARS: usize = 180;
    let mut expanded = String::new();
    let mut count = 0_usize;
    for word in first_line.split_whitespace() {
        if !expanded.is_empty() {
            if count >= MAX_HEADER_SCALARS {
                expanded.push('…');
                return expanded;
            }
            expanded.push(' ');
            count += 1;
        }
        for ch in word.chars() {
            if count >= MAX_HEADER_SCALARS {
                expanded.push('…');
                return expanded;
            }
            expanded.push(ch);
            count += 1;
        }
    }

    if expanded.chars().count() > stored_title.chars().count() {
        expanded
    } else {
        stored_title.to_owned()
    }
}

fn event_text(payload: &str) -> String {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| payload.to_owned())
}

fn payload_preview(payload: &str) -> String {
    const LIMIT: usize = 100;
    let text = event_text(payload);
    let mut preview = text.chars().take(LIMIT).collect::<String>();
    if text.chars().count() > LIMIT {
        preview.push('…');
    }
    preview.replace('\n', " ↵ ")
}

fn configure_ui(ctx: &egui::Context) {
    system_fonts::install(ctx);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(23, 24, 29);
    visuals.window_fill = egui::Color32::from_rgb(23, 24, 29);
    visuals.extreme_bg_color = egui::Color32::from_rgb(16, 17, 20);
    visuals.faint_bg_color = egui::Color32::from_rgb(30, 32, 37);
    visuals.selection.bg_fill = egui::Color32::from_rgb(66, 87, 145);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 45, 53);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(51, 55, 64);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(61, 66, 77);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
    });
}

fn run_production_mirror_acceptance() {
    const MAX_ITEMS: usize = 3;
    let started = Instant::now();
    let journal_path = default_journal_path();
    let cache_path = remote_history_cache_path(&journal_path);
    let catalog = match load_remote_history_cache(&cache_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({
                    "terminal_state": "catalog_read_failed",
                    "error": error,
                    "remote_http_used": false,
                })
            );
            return;
        }
    };
    let store = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({
                    "terminal_state": "journal_open_failed",
                    "error": error.to_string(),
                    "remote_http_used": false,
                })
            );
            return;
        }
    };
    let initial_events = store.events().to_vec();
    let before = match derive_remote_mirror_queue(
        &catalog
            .iter()
            .enumerate()
            .map(|(index, item)| (index, item.id.clone()))
            .collect::<Vec<_>>(),
        &initial_events,
    ) {
        Ok(summary) => summary,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({
                    "terminal_state": "queue_replay_failed",
                    "error": error,
                    "remote_http_used": false,
                })
            );
            return;
        }
    };
    let runtime = match account_bridge::AccountBridgeRuntime::start() {
        Ok(runtime) => runtime,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({
                    "terminal_state": "bridge_start_failed",
                    "error": error.to_string(),
                    "remote_http_used": false,
                })
            );
            return;
        }
    };
    let provider = runtime.provider();
    let (persist_tx, persist_rx) = mpsc::channel();
    let (persist_notice_tx, persist_notice_rx) = mpsc::channel();
    let data_dir = journal_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let completion_tx = persist_tx.clone();
    let persist_worker = thread::Builder::new()
        .name("chatarium-acceptance-persistence".to_owned())
        .spawn(move || {
            persistence_worker(
                store,
                data_dir,
                persist_rx,
                completion_tx,
                persist_notice_tx,
            )
        });
    let Ok(persist_worker) = persist_worker else {
        println!(
            "{}",
            serde_json::json!({
                "terminal_state": "persistence_start_failed",
                "remote_http_used": false,
            })
        );
        return;
    };
    let (controller_tx, controller_rx) = mpsc::channel();
    let (controller_notice_tx, controller_notice_rx) = mpsc::channel();
    let controller_worker = thread::Builder::new()
        .name("chatarium-acceptance-controller".to_owned())
        .spawn(move || {
            mirror_controller_worker(
                provider,
                catalog,
                initial_events,
                Some(MAX_ITEMS),
                controller_rx,
                controller_notice_tx,
            )
        });
    let Ok(controller_worker) = controller_worker else {
        let _ = persist_tx.send(PersistCommand::Shutdown);
        let _ = persist_worker.join();
        println!(
            "{}",
            serde_json::json!({
                "terminal_state": "controller_start_failed",
                "remote_http_used": false,
            })
        );
        return;
    };
    let _ = controller_tx.send(MirrorControllerCommand::Start);
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut attempted = BTreeMap::<usize, ()>::new();
    let mut terminal_state = MirrorControllerState::Running;
    while Instant::now() < deadline {
        let mut progressed = false;
        for notice in controller_notice_rx.try_iter() {
            progressed = true;
            match notice {
                MirrorControllerNotice::State {
                    state,
                    current_catalog_index,
                    ..
                } => {
                    if let Some(index) = current_catalog_index {
                        attempted.insert(index, ());
                    }
                    terminal_state = state;
                }
                MirrorControllerNotice::Prepare {
                    remote_conversation_id,
                    catalog_index,
                    reply,
                } => {
                    let command = PersistCommand::MirrorQueuePrepare {
                        remote_conversation_id,
                        catalog_index,
                        reply,
                    };
                    if let Err(error) = persist_tx.send(command) {
                        if let PersistCommand::MirrorQueuePrepare { reply, .. } = error.0 {
                            let _ = reply.send(Err("persistence worker stopped".to_owned()));
                        }
                    }
                }
                MirrorControllerNotice::Capture {
                    remote_conversation_id,
                    body,
                    reply,
                } => {
                    let command = PersistCommand::MirrorQueueCapture {
                        remote_conversation_id,
                        body,
                        reply,
                    };
                    if let Err(error) = persist_tx.send(command) {
                        if let PersistCommand::MirrorQueueCapture { reply, .. } = error.0 {
                            let _ = reply.send(Err("persistence worker stopped".to_owned()));
                        }
                    }
                }
                MirrorControllerNotice::Failure {
                    remote_conversation_id,
                    failure_class,
                    reply,
                } => {
                    let command = PersistCommand::MirrorQueueFailure {
                        remote_conversation_id,
                        failure_class,
                        reply,
                    };
                    if let Err(error) = persist_tx.send(command) {
                        if let PersistCommand::MirrorQueueFailure { reply, .. } = error.0 {
                            let _ = reply.send(Err("persistence worker stopped".to_owned()));
                        }
                    }
                }
                MirrorControllerNotice::HealthSignal {
                    signal,
                    now_ms,
                    reply,
                } => {
                    if let Err(error) = persist_tx.send(PersistCommand::RecordRemoteHealthSignal {
                        signal,
                        now_ms,
                        reply,
                    }) {
                        if let PersistCommand::RecordRemoteHealthSignal { reply, .. } = error.0 {
                            let _ = reply.send(Err("persistence worker stopped".to_owned()));
                        }
                    }
                }
                MirrorControllerNotice::HealthIntent {
                    intent,
                    now_ms,
                    reply,
                } => {
                    if let Err(error) = persist_tx.send(PersistCommand::RecordRemoteHealthIntent {
                        intent,
                        now_ms,
                        reply,
                    }) {
                        if let PersistCommand::RecordRemoteHealthIntent { reply, .. } = error.0 {
                            let _ = reply.send(Err("persistence worker stopped".to_owned()));
                        }
                    }
                }
            }
        }
        let _ = persist_notice_rx.try_iter().count();
        if matches!(
            terminal_state,
            MirrorControllerState::Completed
                | MirrorControllerState::RateLimited
                | MirrorControllerState::AuthenticationRequired
                | MirrorControllerState::Failed
        ) {
            break;
        }
        if !progressed {
            thread::sleep(Duration::from_millis(20));
        }
    }
    if Instant::now() >= deadline {
        terminal_state = MirrorControllerState::Failed;
    }
    let _ = controller_tx.send(MirrorControllerCommand::Shutdown);
    let _ = controller_worker.join();
    let _ = persist_tx.send(PersistCommand::Shutdown);
    let _ = persist_worker.join();

    let after = JsonlEventStore::open(&journal_path).ok().and_then(|store| {
        derive_remote_mirror_queue(
            &before
                .items
                .iter()
                .map(|item| (item.catalog_index, item.remote_conversation_id.clone()))
                .collect::<Vec<_>>(),
            store.events(),
        )
        .ok()
    });
    let after = after.unwrap_or_else(|| before.clone());
    println!(
        "{}",
        serde_json::json!({
            "terminal_state": mirror_controller_state_label(terminal_state),
            "controller": "production-desktop",
            "max_items": MAX_ITEMS,
            "attempted": attempted.keys().copied().collect::<Vec<_>>(),
            "before": {
                "catalog_count": before.items.len(),
                "full_count": before.full_count(),
                "partial_count": before.partial_count(),
                "pending_count": before.pending_count(),
            },
            "after": {
                "full_count": after.full_count(),
                "partial_count": after.partial_count(),
                "pending_count": after.pending_count(),
                "transient_failure_count": after.count(RemoteMirrorQueueStatus::TransientFailure),
                "rate_limited_count": after.count(RemoteMirrorQueueStatus::RateLimited),
                "structural_failure_count": after.count(RemoteMirrorQueueStatus::StructuralFailure),
            },
            "serial_concurrency": 1,
            "remote_http_used": !attempted.is_empty(),
            "browser_started": !attempted.is_empty(),
            "auth_probe_used": !attempted.is_empty(),
            "temporary_tab_cleanup": "production_finally_cleanup",
            "debugger_cleanup": "production_finally_cleanup",
            "elapsed_ms": started.elapsed().as_millis(),
        })
    );
}

fn main() -> eframe::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--qa-reader") {
        return reader_visual_qa::run();
    }
    diagnostics::init();
    if std::env::args().nth(1).as_deref() == Some("--production-mirror-acceptance") {
        run_production_mirror_acceptance();
        return Ok(());
    }
    diagnostics::info("app", "starting Chatarium desktop");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0])
            .with_transparent(false),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Chatarium",
        options,
        Box::new(|creation_context| {
            configure_ui(&creation_context.egui_ctx);
            Ok(Box::new(ChatariumApp::new(&creation_context.egui_ctx)))
        }),
    );
    match &result {
        Ok(()) => diagnostics::info("app", "desktop event loop exited cleanly"),
        Err(error) => diagnostics::error("app", format!("desktop event loop failed: {error}")),
    }
    result
}
