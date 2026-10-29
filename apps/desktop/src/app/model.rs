//! Typed desktop state, notices, commands and projections.
use super::super::*;

pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerControlAction {
    StartOrResume,
    Continue,
    Stop,
    StatusRequest,
}

impl ControllerControlAction {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::StartOrResume => "start/resume",
            Self::Continue => "continue",
            Self::Stop => "stop",
            Self::StatusRequest => "status request",
        }
    }
}

pub(crate) enum PersistCommand {
    ProjectNativeSearchIndex {
        prefix_len: usize,
        last_sequence: Option<u64>,
        reply:
            Sender<Result<Arc<native_conversation_search::NativeConversationSearchIndex>, String>>,
    },
    ProjectInlineMcpStatus {
        conversation_id: LocalConversationId,
        reply: Sender<(Option<u64>, Option<InlineMcpStatus>)>,
    },
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

pub(crate) enum PersistNotice {
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
pub(crate) enum MirrorFailureClass {
    Transient,
    RateLimited,
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MirrorPersisted {
    pub(crate) snapshot_sequence: u64,
    pub(crate) mirror_state: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MirrorControllerState {
    Stopped,
    Running,
    Paused,
    RateLimited,
    AuthenticationRequired,
    Completed,
    Failed,
}

pub(crate) enum MirrorControllerCommand {
    Start,
    Pause,
    Resume,
    RecheckHealth,
    Shutdown,
}

pub(crate) enum MirrorControllerNotice {
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

pub(crate) enum LiveMirrorFetchNotice {
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
pub(crate) enum DisplayRole {
    User,
    Assistant,
    /// Audited local tool activity; never user-authored or assistant-authored.
    Mcp,
    /// Source-observed inference lifecycle, not assistant-authored content.
    Activity,
}

#[derive(Debug, Clone)]
pub(crate) struct DisplayMessage {
    pub(crate) role: DisplayRole,
    pub(crate) text: String,
    pub(crate) sequence: u64,
    pub(crate) timestamp: Option<f64>,
    pub(crate) provenance_label: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingOneShotMemory {
    pub(crate) memory_id: LocalMemoryId,
    pub(crate) source_conversation_id: LocalConversationId,
    pub(crate) text: String,
    pub(crate) artifact_sequence: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingInferenceIntent {
    pub(crate) model: String,
    pub(crate) instructions: Option<String>,
    pub(crate) developer_context: String,
    pub(crate) routed_context: Vec<context_composer::TranscriptMessage>,
    pub(crate) tool_result_context: Vec<context_composer::TranscriptMessage>,
    pub(crate) local_memory_context: Vec<context_composer::TranscriptMessage>,
    pub(crate) controller_result_context: Vec<context_composer::TranscriptMessage>,
    pub(crate) controller_coordination_result_context: Vec<context_composer::TranscriptMessage>,
    pub(crate) request_patch: Value,
    pub(crate) attachment: Option<attachment_intake::PreparedAttachment>,
    pub(crate) coordination_ids: Vec<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingRemoteTurn {
    pub(crate) conversation_id: LocalConversationId,
    pub(crate) turn_id: LocalTurnId,
    pub(crate) request_id: String,
    pub(crate) model: String,
    pub(crate) input: Value,
    pub(crate) instructions: Option<String>,
    pub(crate) request_patch: Value,
}

/// One user-authored turn may contain several remote response steps and MCP
/// calls. All remote steps remain correlated to the ORIGINAL local turn ID.
/// Tool dispatches are independently journaled before side effects.
#[derive(Debug, Clone)]
pub(crate) struct AgentMcpFlow {
    pub(crate) turn_id: LocalTurnId,
    pub(crate) conversation_id: LocalConversationId,
    pub(crate) command: siwc_bridge::BridgeCommand,
    pub(crate) proposal_text: String,
    pub(crate) call_count: u32,
    pub(crate) pending_call: Option<ToolCallId>,
    pub(crate) pending_route: Option<RouteId>,
    pub(crate) policy: mcp_agent_defaults::ToolPolicy,
}

/// Durable, conversation-scoped status for the latest MCP request.
/// Presentation only: buttons still use the original audited route checks.
#[derive(Clone)]
pub(crate) struct InlineMcpStatus {
    pub(crate) call_id: ToolCallId,
    pub(crate) route_id: RouteId,
    pub(crate) provider: String,
    pub(crate) operation: String,
    pub(crate) arguments: String,
    pub(crate) stage: mcp_call_review::CallReviewStage,
    pub(crate) automatically_authorized: bool,
    pub(crate) successful: Option<bool>,
}

pub(crate) fn project_inline_mcp_status(
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
pub(crate) struct ActiveRemoteTurn {
    pub(crate) conversation_id: LocalConversationId,
    pub(crate) turn_id: LocalTurnId,
    pub(crate) request_id: String,
    pub(crate) cumulative_text: String,
    pub(crate) observed_output: bool,
    pub(crate) source_suspect_logged: bool,
    pub(crate) command: siwc_bridge::BridgeCommand,
    pub(crate) retries: u32,
    pub(crate) retry_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub(crate) struct LiveMirrorCatalogEntry {
    pub(crate) local_conversation_id: LocalConversationId,
    pub(crate) remote_conversation_id: String,
    pub(crate) title: String,
    pub(crate) snapshot_sequence: u64,
    pub(crate) truncated_before: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteCatalogViewEntry {
    pub(crate) catalog_index: usize,
    pub(crate) item: ConversationListItem,
    pub(crate) status: RemoteMirrorQueueStatus,
    pub(crate) local_conversation_id: Option<LocalConversationId>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingHistoryFetchProof {
    pub(crate) local_conversation_id: Option<LocalConversationId>,
    pub(crate) remote_conversation_id: String,
    pub(crate) proof: account_bridge::BrowserProof,
    pub(crate) http_status: u16,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenVikingSummaryAttempt {
    pub(crate) request_id: String,
    pub(crate) conversation_id: LocalConversationId,
    pub(crate) turn_id: LocalTurnId,
    pub(crate) completed_sequence: u64,
    pub(crate) had_confirmed_prior: bool,
}

pub(crate) struct ChatariumApp {
    pub(crate) draft: String,
    pub(crate) attachment_path_draft: String,
    pub(crate) staged_attachment: Option<attachment_intake::PreparedAttachment>,
    pub(crate) draft_revision: u64,
    pub(crate) saved_revision: u64,
    pub(crate) next_commit_request: u64,
    pub(crate) commit_in_flight: Option<u64>,
    pub(crate) evidence: TurnEvidence,
    pub(crate) local_conversation_id: LocalConversationId,
    pub(crate) local_conversation_catalog: local_conversations::LocalConversationCatalog,
    pub(crate) project_memory_grants: openviking_memory_delivery::ProjectMemoryGrants,
    pub(crate) coordination_workspace: coordination_workspace::Workspace,
    pub(crate) coordination_pending_admissions: BTreeMap<String, Vec<u64>>,
    pub(crate) coordination_ui: coordination_workspace::UiDrafts,
    pub(crate) coordination_window_open: bool,
    pub(crate) coordination_tool_budget: coordination_tool_budget::Budget,
    pub(crate) coordination_last_queue_check: Instant,
    pub(crate) coordination_auto_wake_targets: HashSet<LocalConversationId>,
    pub(crate) coordination_auto_wake_count: u32,
    pub(crate) coordination_pending_wake: Option<(LocalTurnId, LocalConversationId, Vec<u64>)>,
    pub(crate) local_conversation_rename: String,
    pub(crate) local_conversation_project_directory: String,
    pub(crate) show_archived_local_conversations: bool,
    pub(crate) local_conversation_search_query: String,
    pub(crate) local_conversation_search_selection: Option<LocalConversationId>,
    pub(crate) events: Vec<EventEnvelope>,
    pub(crate) historical_catalog: Vec<HistoricalConversationCatalogEntry>,
    pub(crate) selected_historical_conversation: Option<LocalConversationId>,
    pub(crate) loaded_historical_conversation: Option<LocalConversationId>,
    pub(crate) historical_messages: Vec<DisplayMessage>,
    pub(crate) historical_load_pending: Option<LocalConversationId>,
    pub(crate) live_mirrored_conversations: HashSet<LocalConversationId>,
    pub(crate) live_mirror_catalog: Vec<LiveMirrorCatalogEntry>,
    pub(crate) live_mirror_pending: Option<LocalConversationId>,
    pub(crate) remote_discovery_pending: Option<String>,
    pub(crate) remote_mirror_failures: BTreeMap<String, String>,
    pub(crate) remote_mirror_retry_after: BTreeMap<String, Instant>,
    pub(crate) mirror_status: String,
    pub(crate) live_mirror_truncated_before: bool,
    pub(crate) remote_conversation_catalog: Vec<ConversationListItem>,
    pub(crate) remote_catalog_view: Vec<RemoteCatalogViewEntry>,
    pub(crate) local_archive_search_index: local_archive_search::LocalArchiveSearchIndex,
    pub(crate) archive_search_query: String,
    pub(crate) archive_search_mode: local_archive_search::ArchiveSearchMode,
    pub(crate) archive_state_filter: local_archive_search::ArchiveStateFilter,
    pub(crate) archive_search_selection: Option<usize>,
    pub(crate) reader_state_path: PathBuf,
    pub(crate) reader_positions: offline_reader::ReaderPositionStore,
    pub(crate) reader_restore_pending: bool,
    pub(crate) reader_last_saved_offset: f32,
    pub(crate) reader_last_position_write: Instant,
    pub(crate) reader_search_query: String,
    pub(crate) reader_search_hit: Option<usize>,
    pub(crate) inference_settings: local_inference_settings::InferenceSettingsStore,
    pub(crate) mcp_agent_defaults: mcp_agent_defaults::AgentDefaults,
    pub(crate) host_shell_settings: host_shell_mcp::Settings,
    pub(crate) behavior_profiles: behavior_profile::BehaviorProfileStore,
    pub(crate) conversation_behavior_profile: behavior_profile::BehaviorProfile,
    pub(crate) topology_command_pending: bool,
    pub(crate) route_addressability_command_pending: bool,
    pub(crate) route_policy_command_pending: bool,
    pub(crate) route_payload_command_pending: bool,
    pub(crate) route_payload_drafts: BTreeMap<RouteId, String>,
    pub(crate) route_dispatch_command_pending: bool,
    pub(crate) route_context_command_pending: bool,
    pub(crate) tool_command_pending: bool,
    // Read-only, point-in-time host observation; never an execution permit.
    pub(crate) last_sandbox_host_check: Option<Result<(), String>>,
    pub(crate) tool_provider_name_draft: String,
    pub(crate) tool_selected_provider: Option<ToolProviderId>,
    pub(crate) tool_stdio_executable_draft: String,
    pub(crate) tool_stdio_argv_draft: String,
    pub(crate) tool_stdio_operations_draft: String,
    pub(crate) tool_operation_draft: String,
    pub(crate) tool_arguments_draft: String,
    pub(crate) local_memory_command_pending: bool,
    pub(crate) local_memory_draft: String,
    // Transient source of an unedited message staged from native search.
    // Editing the draft clears this origin; recording remains an explicit act.
    pub(crate) staged_local_memory_origin: Option<(LocalConversationId, u64)>,
    pub(crate) local_memory_search_query: String,
    pub(crate) local_memory_label_filter: Option<LocalMemoryLabel>,
    pub(crate) local_memory_source_filter: Option<LocalConversationId>,
    pub(crate) show_superseded_local_memory: bool,
    pub(crate) local_memory_label_drafts: BTreeMap<LocalMemoryId, String>,
    pub(crate) local_memory_one_shot_selections:
        BTreeMap<LocalConversationId, BTreeSet<LocalMemoryId>>,
    pub(crate) lifecycle_command_pending: bool,
    pub(crate) supervision_command_pending: bool,
    pub(crate) controller_control_command_pending: bool,
    pub(crate) controller_result_context_command_pending: bool,
    pub(crate) controller_coordination_command_pending: bool,
    pub(crate) controller_coordination_result_context_command_pending: bool,
    pub(crate) controller_coordination_suggestion_command_pending: bool,
    pub(crate) continuation_allowance_drafts: BTreeMap<LocalConversationId, u32>,
    pub(crate) worker_control_command_pending: bool,
    pub(crate) conversation_instructions: String,
    pub(crate) conversation_developer_context: String,
    pub(crate) conversation_mcp_enabled: bool,
    pub(crate) archive_backup_path: String,
    pub(crate) archive_maintenance_status: String,
    pub(crate) archive_maintenance_result: Option<Receiver<String>>,
    pub(crate) archive_restore_confirmation_pending: bool,
    pub(crate) selected_remote_catalog_id: Option<String>,
    pub(crate) remote_conversation_total: Option<u64>,
    pub(crate) history_discovery_started: bool,
    pub(crate) history_list_pending: bool,
    pub(crate) history_bridge_proven: bool,
    pub(crate) pending_history_fetch_proof: Option<PendingHistoryFetchProof>,
    pub(crate) journal_path: PathBuf,
    pub(crate) persist_tx: Option<Sender<PersistCommand>>,
    pub(crate) notice_rx: Option<Receiver<PersistNotice>>,
    pub(crate) worker: Option<JoinHandle<()>>,
    pub(crate) status: String,
    pub(crate) account_bridge: Option<account_bridge::AccountBridgeRuntime>,
    pub(crate) account_bridge_provider: Option<account_bridge::BrowserBridgeProvider>,
    pub(crate) account_bridge_status: String,
    pub(crate) live_mirror_fetch_tx: Sender<LiveMirrorFetchNotice>,
    pub(crate) live_mirror_fetch_rx: Receiver<LiveMirrorFetchNotice>,
    pub(crate) mirror_controller_tx: Option<Sender<MirrorControllerCommand>>,
    pub(crate) mirror_controller_notice_rx: Option<Receiver<MirrorControllerNotice>>,
    pub(crate) mirror_controller_worker: Option<JoinHandle<()>>,
    pub(crate) mirror_controller_state: MirrorControllerState,
    pub(crate) remote_health: RemoteHealthController,
    pub(crate) mirror_controller_current_item: Option<usize>,
    pub(crate) mirror_controller_detail: Option<String>,
    pub(crate) remote: siwc_bridge::BridgeRuntime,
    pub(crate) remote_session: siwc_bridge::SessionState,
    pub(crate) remote_models: Vec<siwc_bridge::Model>,
    pub(crate) capability_probe: capability_probes::ProbeRun,
    pub(crate) local_inference_contract: Option<local_inference_contract::LoadedContract>,
    // One-click verification intent; never enable a different chat/model/profile.
    pub(crate) pending_web_search_enable: Option<(LocalConversationId, String, String)>,
    pub(crate) model_list_pending: bool,
    pub(crate) selected_model: Option<String>,
    pub(crate) slow_warning_seconds: u32,
    pub(crate) remote_status: String,
    pub(crate) network_health: network_health::NetworkHealth,
    pub(crate) remote_runtime_ready: bool,
    pub(crate) remote_runtime_failed: bool,
    pub(crate) sign_in_requested: bool,
    pub(crate) sign_in_pending: bool,
    pub(crate) pending_remote_turn: Option<PendingRemoteTurn>,
    pub(crate) active_remote_turn: Option<ActiveRemoteTurn>,
    pub(crate) agent_mcp_flow: Option<AgentMcpFlow>,
    pub(crate) openviking_checkpoint_status: String,
    pub(crate) openviking_summary_in_flight: Option<OpenVikingSummaryAttempt>,
    pub(crate) openviking_summary_last_attempt: Option<Instant>,
    pub(crate) openviking_write_last_attempt: Option<Instant>,
    pub(crate) openviking_summary_blocked: HashSet<String>,
    pub(crate) openviking_retrieval_cache:
        BTreeMap<LocalConversationId, Vec<openviking_memory_delivery::RetrievedCheckpoint>>,
    pub(crate) openviking_retrieval_in_flight: HashSet<LocalConversationId>,
    pub(crate) openviking_retrieval_retry_after: BTreeMap<LocalConversationId, Instant>,
    pub(crate) openviking_retrieval_stale: HashSet<LocalConversationId>,
    pub(crate) openviking_query_tracker: openviking_project_cache::QueryRefreshTracker,
    pub(crate) openviking_project_cache: openviking_project_cache::RetrievalCache,
    pub(crate) commit_remote_intents: BTreeMap<u64, PendingInferenceIntent>,
    pub(crate) delivery: delivery_telemetry::Timeline,
}
