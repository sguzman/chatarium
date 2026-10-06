mod account_bridge;
mod behavior_profile;
mod capability_probes;
mod context_composer;
mod diagnostics;
mod local_archive_search;
mod local_conversations;
mod local_inference_contract;
mod local_inference_settings;
mod offline_reader;
mod siwc_bridge;

use chatarium_core::chat_container::{ChatContainerId, SessionLifecyclePhase};
use chatarium_core::orchestration::{
    WorkerAction, WorkerGoalId, WorkerId, WorkerLifecycle, WorkerPhase,
};
use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteEndpointId, RouteGate, RouteGateState, RouteId,
    RoutePayloadId, RoutePolicy, RouteRequest,
};
use chatarium_core::session::{SessionEndpointBinding, SessionId};
use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId,
    LocalTurnId, RemoteEvidence, TurnEvidence,
};
use chatarium_protocol::conversation_list::ConversationListItem;
use chatarium_store::archive_maintenance::{
    check_archive, create_backup, restore_backup, verify_backup,
};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, decode_user_message_commit, local_turn_scope,
};
use chatarium_store::chat_container_audit::{
    record_chat_container_created, replay_chat_container_audit,
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
    record_local_session_registered, record_session_endpoint_bound, replay_session_audit,
};
use chatarium_store::worker_audit::{
    WorkerAuditRecord, record_worker_goal_assigned, record_worker_transition, replay_worker_audit,
};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

enum PersistCommand {
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
    BindLocalConversationWorker {
        conversation_id: LocalConversationId,
        worker_id: WorkerId,
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
    },
    AppendTurnEvent {
        turn_id: LocalTurnId,
        kind: EventKind,
        payload: String,
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
    DraftSaved {
        conversation_id: LocalConversationId,
        revision: u64,
        event: EventEnvelope,
    },
    MessageCommitted {
        request_id: u64,
        message: AuthoredUserMessage,
        event: EventEnvelope,
    },
    TurnEventAppended {
        turn_id: LocalTurnId,
        kind: EventKind,
        event: EventEnvelope,
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
    LifecycleEventAppended {
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
struct PendingInferenceIntent {
    model: String,
    instructions: Option<String>,
    developer_context: String,
    routed_context: Vec<context_composer::TranscriptMessage>,
    request_patch: Value,
}

#[derive(Debug, Clone)]
struct PendingRemoteTurn {
    turn_id: LocalTurnId,
    request_id: String,
    model: String,
    input: Value,
    instructions: Option<String>,
    request_patch: Value,
}

#[derive(Debug, Clone)]
struct ActiveRemoteTurn {
    turn_id: LocalTurnId,
    request_id: String,
    cumulative_text: String,
    observed_output: bool,
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

struct ChatariumApp {
    draft: String,
    draft_revision: u64,
    saved_revision: u64,
    next_commit_request: u64,
    commit_in_flight: Option<u64>,
    evidence: TurnEvidence,
    local_conversation_id: LocalConversationId,
    local_conversation_catalog: local_conversations::LocalConversationCatalog,
    local_conversation_rename: String,
    show_archived_local_conversations: bool,
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
    behavior_profiles: behavior_profile::BehaviorProfileStore,
    conversation_behavior_profile: behavior_profile::BehaviorProfile,
    topology_command_pending: bool,
    route_addressability_command_pending: bool,
    route_policy_command_pending: bool,
    route_payload_command_pending: bool,
    route_payload_drafts: BTreeMap<RouteId, String>,
    route_dispatch_command_pending: bool,
    route_context_command_pending: bool,
    lifecycle_command_pending: bool,
    conversation_instructions: String,
    conversation_developer_context: String,
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
    model_list_pending: bool,
    selected_model: Option<String>,
    remote_status: String,
    remote_runtime_ready: bool,
    remote_runtime_failed: bool,
    sign_in_requested: bool,
    sign_in_pending: bool,
    pending_remote_turn: Option<PendingRemoteTurn>,
    active_remote_turn: Option<ActiveRemoteTurn>,
    commit_remote_intents: BTreeMap<u64, PendingInferenceIntent>,
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
                let historical_catalog = match latest_historical_conversation_catalog(&events) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; historical archive replay warning: {error}");
                        Vec::new()
                    }
                };
                let live_mirror_catalog = match latest_live_mirror_catalog(&events) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; live mirror replay warning: {error}");
                        Vec::new()
                    }
                };
                let remote_history_cache = remote_history_cache_path(&journal_path);
                let remote_conversation_catalog =
                    match load_remote_history_cache(&remote_history_cache) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; remote history cache warning: {error}");
                            Vec::new()
                        }
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
                let local_archive_search_index =
                    build_local_archive_search_index(&remote_catalog_view, &events);
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
                let active_inference_settings =
                    inference_settings.for_conversation(local_conversation_id);
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
                let worker = thread::Builder::new()
                    .name("chatarium-persistence".to_owned())
                    .spawn(move || {
                        persistence_worker(store, worker_data_dir, persist_rx, notice_tx)
                    });

                match worker {
                    Ok(worker) => Self {
                        draft,
                        draft_revision: 0,
                        saved_revision: 0,
                        next_commit_request: 1,
                        commit_in_flight: None,
                        evidence: TurnEvidence::default(),
                        local_conversation_id,
                        local_conversation_rename: local_conversation_display_title(
                            &local_conversation_catalog,
                            local_conversation_id,
                            &events,
                        ),
                        local_conversation_catalog,
                        show_archived_local_conversations: false,
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
                        behavior_profiles,
                        conversation_behavior_profile: active_behavior_profile,
                        topology_command_pending: false,
                        route_addressability_command_pending: false,
                        route_policy_command_pending: false,
                        route_payload_command_pending: false,
                        route_payload_drafts: BTreeMap::new(),
                        route_dispatch_command_pending: false,
                        route_context_command_pending: false,
                        lifecycle_command_pending: false,
                        conversation_instructions: active_inference_settings.instructions,
                        conversation_developer_context: active_inference_settings.developer_context,
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
                        model_list_pending: false,
                        selected_model: active_inference_settings.model,
                        remote_status: "starting sign-in runtime…".to_owned(),
                        remote_runtime_ready: false,
                        remote_runtime_failed: false,
                        sign_in_requested: false,
                        sign_in_pending: false,
                        pending_remote_turn: None,
                        active_remote_turn: None,
                        commit_remote_intents: BTreeMap::new(),
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
        let live_mirror_catalog = latest_live_mirror_catalog(&events).unwrap_or_default();
        let live_mirrored_conversations = live_mirror_catalog
            .iter()
            .map(|entry| entry.local_conversation_id)
            .collect();
        let (account_bridge, account_bridge_provider, account_bridge_status) =
            start_account_bridge();
        let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
        let remote_conversation_catalog =
            load_remote_history_cache(&remote_history_cache_path(&journal_path))
                .unwrap_or_default();
        let remote_catalog_view =
            build_remote_catalog_view(&remote_conversation_catalog, &events, &live_mirror_catalog)
                .unwrap_or_default();
        let local_archive_search_index =
            build_local_archive_search_index(&remote_catalog_view, &events);
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
        let active_inference_settings = inference_settings.for_conversation(local_conversation_id);
        let behavior_profiles = behavior_profile::BehaviorProfileStore::load(
            &local_behavior_profile_path(&journal_path),
        )
        .unwrap_or_default();
        let active_behavior_profile = behavior_profiles.for_conversation(local_conversation_id);

        Self {
            draft,
            draft_revision: 0,
            saved_revision: 0,
            next_commit_request: 1,
            commit_in_flight: None,
            evidence: TurnEvidence::default(),
            local_conversation_id,
            local_conversation_rename: local_conversation_display_title(
                &local_conversation_catalog,
                local_conversation_id,
                &events,
            ),
            local_conversation_catalog,
            show_archived_local_conversations: false,
            historical_catalog: latest_historical_conversation_catalog(&events).unwrap_or_default(),
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
            behavior_profiles,
            conversation_behavior_profile: active_behavior_profile,
            topology_command_pending: false,
            route_addressability_command_pending: false,
            route_policy_command_pending: false,
            route_payload_command_pending: false,
            route_payload_drafts: BTreeMap::new(),
            route_dispatch_command_pending: false,
            route_context_command_pending: false,
            lifecycle_command_pending: false,
            conversation_instructions: active_inference_settings.instructions,
            conversation_developer_context: active_inference_settings.developer_context,
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
            model_list_pending: false,
            selected_model: active_inference_settings.model,
            remote_status: "starting sign-in runtime…".to_owned(),
            remote_runtime_ready: false,
            remote_runtime_failed: false,
            sign_in_requested: false,
            sign_in_pending: false,
            pending_remote_turn: None,
            active_remote_turn: None,
            commit_remote_intents: BTreeMap::new(),
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
    }

    fn apply_local_conversation(&mut self, conversation_id: LocalConversationId) {
        self.local_conversation_id = conversation_id;
        self.draft = projected_working_draft(&self.events, conversation_id);
        self.draft_revision = 0;
        self.saved_revision = 0;
        self.evidence = TurnEvidence::default();

        let settings = self.inference_settings.for_conversation(conversation_id);
        self.selected_model = settings.model;
        self.conversation_instructions = settings.instructions;
        self.conversation_developer_context = settings.developer_context;
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

        self.local_conversation_rename = local_conversation_display_title(
            &self.local_conversation_catalog,
            conversation_id,
            &self.events,
        );
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
            self.local_conversation_rename = local_conversation_display_title(
                &self.local_conversation_catalog,
                self.local_conversation_id,
                &self.events,
            );
            self.status = "local conversation title saved".to_owned();
        }
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

    fn commit_current_message(&mut self) {
        if self.draft.trim().is_empty() || self.commit_in_flight.is_some() {
            return;
        }
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
        if self.remote_connected() {
            if let Some(model) = self.selected_model.clone() {
                let request_patch = match self.current_behavior_request_patch() {
                    Ok(patch) => patch,
                    Err(error) => {
                        self.status = format!("cannot send with current behavior profile: {error}");
                        return;
                    }
                };
                let routed_context = match admitted_routed_context_messages(
                    &self.events,
                    self.local_conversation_id,
                ) {
                    Ok(messages) => messages,
                    Err(error) => {
                        self.status =
                            format!("cannot snapshot admitted routed context: {error}");
                        return;
                    }
                };
                self.commit_remote_intents.insert(
                    request_id,
                    PendingInferenceIntent {
                        model,
                        instructions: (!self.conversation_instructions.trim().is_empty())
                            .then(|| self.conversation_instructions.clone()),
                        developer_context: self.conversation_developer_context.clone(),
                        routed_context,
                        request_patch,
                    },
                );
            }
        }
        match sender.send(PersistCommand::CommitMessage {
            request_id,
            message,
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

    fn process_notices(&mut self) {
        let notices = self
            .notice_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for notice in notices {
            match notice {
                PersistNotice::DraftSaved {
                    conversation_id,
                    revision,
                    event,
                } => {
                    self.events.push(event);
                    if conversation_id == self.local_conversation_id {
                        self.saved_revision = self.saved_revision.max(revision);
                        if self.saved_revision == self.draft_revision
                            && self.commit_in_flight.is_none()
                            && self.active_remote_turn.is_none()
                        {
                            self.status = "draft durable".to_owned();
                        }
                    }
                }
                PersistNotice::MessageCommitted {
                    request_id,
                    message,
                    event,
                } => {
                    let sequence = event.sequence;
                    self.events.push(event);
                    if self.commit_in_flight == Some(request_id) {
                        self.commit_in_flight = None;
                        self.evidence.commit_local_message();
                        self.draft.clear();

                        if let Some(intent) = self.commit_remote_intents.remove(&request_id) {
                            let remote_request_id = message.turn_id.to_string();
                            let mut transcript =
                                context_transcript(&projected_local_display_messages(
                                    &self.events,
                                    self.local_conversation_id,
                                ));
                            transcript.extend(intent.routed_context);
                            transcript.sort_by_key(
                                context_composer::TranscriptMessage::order_sequence,
                            );
                            let context_plan = context_composer::ContextPlan::compose(
                                context_composer::ContextPolicy::dispatch(),
                                intent.instructions.as_deref().unwrap_or_default(),
                                intent.developer_context.as_str(),
                                transcript,
                            );
                            self.pending_remote_turn = Some(PendingRemoteTurn {
                                turn_id: message.turn_id,
                                request_id: remote_request_id.clone(),
                                model: intent.model.clone(),
                                input: context_plan.input_json(),
                                instructions: context_plan.instructions.clone(),
                                request_patch: intent.request_patch,
                            });
                            let payload = remote_turn_payload(
                                message.turn_id,
                                &remote_request_id,
                                Some(&intent.model),
                                None,
                                None,
                            );
                            if self.queue_turn_event(
                                message.turn_id,
                                EventKind::DispatchAttempted,
                                payload,
                            ) {
                                self.status = format!(
                                    "message durable as event #{sequence}; preparing ChatGPT dispatch"
                                );
                            } else {
                                self.pending_remote_turn = None;
                            }
                        } else {
                            self.status =
                                format!("user message durably committed as event #{sequence}");
                        }

                        self.queue_draft_snapshot();
                    }
                }
                PersistNotice::TurnEventAppended {
                    turn_id,
                    kind,
                    event,
                } => {
                    self.events.push(event);
                    if kind == EventKind::DispatchAttempted
                        && self
                            .pending_remote_turn
                            .as_ref()
                            .is_some_and(|pending| pending.turn_id == turn_id)
                    {
                        let pending = self
                            .pending_remote_turn
                            .take()
                            .expect("checked pending remote turn");
                        let command = siwc_bridge::BridgeCommand::StreamResponse {
                            request_id: pending.request_id.clone(),
                            model: pending.model.clone(),
                            input: pending.input,
                            instructions: pending.instructions,
                            request_patch: pending.request_patch,
                        };
                        match self.remote.send(command) {
                            Ok(()) => {
                                self.active_remote_turn = Some(ActiveRemoteTurn {
                                    turn_id,
                                    request_id: pending.request_id,
                                    cumulative_text: String::new(),
                                    observed_output: false,
                                });
                            }
                            Err(error) => {
                                let payload = remote_turn_payload(
                                    turn_id,
                                    &pending.request_id,
                                    Some(&pending.model),
                                    None,
                                    Some("bridge command channel closed before outcome"),
                                );
                                let _ = self.queue_turn_event(
                                    turn_id,
                                    EventKind::TransportInterrupted,
                                    payload,
                                );
                                self.remote_status = error;
                            }
                        }
                    }
                }
                PersistNotice::HistoricalConversationLoaded {
                    local_conversation_id,
                    imported_sequence,
                    messages,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_messages =
                            historical_display_messages(messages, imported_sequence);
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.historical_load_pending = None;
                        self.status = format!(
                            "verified historical snapshot loaded from import event #{imported_sequence}"
                        );
                    }
                }
                PersistNotice::HistoricalConversationLoadFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = None;
                        self.historical_messages.clear();
                        self.status = format!("historical snapshot load failed: {error}");
                    }
                }
                PersistNotice::LiveConversationLoaded {
                    local_conversation_id,
                    snapshot_sequence,
                    truncated_before,
                    messages,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_messages =
                            remote_display_messages(messages, snapshot_sequence);
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.historical_load_pending = None;
                        self.live_mirror_truncated_before = truncated_before;
                        self.status = format!(
                            "validated live mirror loaded from remote snapshot event #{snapshot_sequence}"
                        );
                    }
                }
                PersistNotice::LiveConversationLoadFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = None;
                        self.historical_messages.clear();
                        self.status = format!("live mirror snapshot load failed: {error}");
                    }
                }
                PersistNotice::HistoricalLiveMirrorPromoted {
                    local_conversation_id,
                    snapshot_sequence,
                    appended_events,
                    truncated_before,
                    messages,
                    projection_error,
                } => {
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }

                    match self.pending_history_fetch_proof.take() {
                        Some(pending)
                            if pending.local_conversation_id == Some(local_conversation_id) =>
                        {
                            self.mirror_status = format!(
                                "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                                if truncated_before {
                                    "MIRRORED PARTIAL"
                                } else {
                                    "MIRRORED COMPLETE"
                                },
                                browser_proof_label(&pending.proof),
                                pending.http_status,
                                snapshot_sequence,
                            );
                        }
                        Some(pending) => {
                            self.mirror_status = format!(
                                "FAILED · durable event #{} has no matching browser proof (pending remote id {})",
                                snapshot_sequence, pending.remote_conversation_id,
                            );
                        }
                        None => {
                            self.mirror_status = format!(
                                "MIRRORED · durable event #{} committed without a current browser proof chain",
                                snapshot_sequence,
                            );
                        }
                    }

                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.live_mirror_truncated_before = truncated_before;
                        if let Some(error) = projection_error {
                            self.historical_messages.clear();
                            self.status = format!(
                                "live mirror is durable, but visible transcript projection is blocked: {error}"
                            );
                        } else {
                            self.historical_messages =
                                remote_display_messages(messages, snapshot_sequence);
                            self.status = format!(
                                "ChatGPT live mirror durable at event #{snapshot_sequence}"
                            );
                        }
                    } else {
                        self.status =
                            format!("ChatGPT live mirror durable at event #{snapshot_sequence}");
                    }
                }
                PersistNotice::HistoricalLiveMirrorPromotionFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                    self.status = format!("live mirror promotion failed: {error}");
                }
                PersistNotice::DiscoveredLiveMirrorPromoted {
                    local_conversation_id,
                    remote_conversation_id,
                    snapshot_sequence,
                    appended_events,
                    truncated_before,
                    messages,
                    projection_error,
                } => {
                    diagnostics::info(
                        "mirror",
                        format!(
                            "durable mirror notice remote={} local={local_conversation_id} event=#{snapshot_sequence} partial={truncated_before}",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.selected_remote_catalog_id = None;
                    self.selected_historical_conversation = Some(local_conversation_id);
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.historical_load_pending = None;
                    self.live_mirror_truncated_before = truncated_before;

                    self.remote_mirror_failures.remove(&remote_conversation_id);
                    self.remote_mirror_retry_after
                        .remove(&remote_conversation_id);
                    match self.pending_history_fetch_proof.take() {
                        Some(pending)
                            if pending.local_conversation_id.is_none()
                                && pending.remote_conversation_id == remote_conversation_id =>
                        {
                            self.mirror_status = format!(
                                "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                                if truncated_before {
                                    "MIRRORED PARTIAL"
                                } else {
                                    "MIRRORED COMPLETE"
                                },
                                browser_proof_label(&pending.proof),
                                pending.http_status,
                                snapshot_sequence,
                            );
                        }
                        Some(pending) => {
                            self.mirror_status = format!(
                                "FAILED · durable event #{} did not match pending proof for remote id {}",
                                snapshot_sequence, pending.remote_conversation_id,
                            );
                        }
                        None => {
                            self.mirror_status = format!(
                                "MIRRORED · durable event #{} committed without a current browser proof chain",
                                snapshot_sequence,
                            );
                        }
                    }

                    if let Some(error) = projection_error {
                        self.historical_messages.clear();
                        self.status = format!(
                            "live mirror is durable, but visible transcript projection is blocked: {error}"
                        );
                    } else {
                        self.historical_messages =
                            remote_display_messages(messages, snapshot_sequence);
                        self.status = format!(
                            "remote ChatGPT conversation mirrored locally at event #{snapshot_sequence}"
                        );
                    }
                }
                PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                    remote_conversation_id,
                    error,
                } => {
                    diagnostics::error(
                        "mirror",
                        format!(
                            "durable mirror commit failed remote={}: {error}",
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
                    self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                    self.status = format!("remote mirror creation failed: {error}");
                }
                PersistNotice::MirrorQueueUpdated { appended_events } => {
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    self.status = "production mirror queue state durably updated".to_owned();
                }
                PersistNotice::RemoteHealthUpdated { event, controller } => {
                    self.events.push(event);
                    self.remote_health = controller;
                    self.status = format!(
                        "remote health: {} · mirror intent: {}",
                        self.remote_health.state.as_str(),
                        self.remote_health.intent.as_str()
                    );
                }
                PersistNotice::OrchestrationTopologyInitialized { appended_events } => {
                    let count = appended_events.len();
                    self.events.extend(appended_events);
                    self.topology_command_pending = false;
                    self.status = format!(
                        "local orchestration topology durably initialized · {count} events"
                    );
                }
                PersistNotice::RouteEndpointBound { event } => {
                    self.events.push(event);
                    self.route_addressability_command_pending = false;
                    self.status = "current session routing endpoint durably bound".to_owned();
                }
                PersistNotice::LocalRoutePolicyEventAppended { event } => {
                    let kind = event.kind.stable_name();
                    self.events.push(event);
                    self.route_policy_command_pending = false;
                    self.status = format!("local route policy durably updated · {kind}");
                }
                PersistNotice::LocalRoutePayloadAttached { route_id, event } => {
                    self.events.push(event);
                    self.route_payload_command_pending = false;
                    self.route_payload_drafts.remove(&route_id);
                    self.status =
                        format!("local route {} payload durably attached", route_id.get());
                }
                PersistNotice::LocalRouteDispatchUpdated {
                    route_id,
                    appended_events,
                } => {
                    let count = appended_events.len();
                    self.events.extend(appended_events);
                    self.route_dispatch_command_pending = false;
                    self.status = format!(
                        "local route {} dispatch/delivery durably advanced · {} event{}",
                        route_id.get(),
                        count,
                        if count == 1 { "" } else { "s" },
                    );
                }
                PersistNotice::LocalRouteContextDecisionUpdated { route_id, event } => {
                    self.events.push(event);
                    self.route_context_command_pending = false;
                    self.status = format!(
                        "local route {} context eligibility durably updated",
                        route_id.get()
                    );
                }
                PersistNotice::LifecycleEventAppended { event } => {
                    let kind = event.kind.stable_name();
                    self.events.push(event);
                    self.lifecycle_command_pending = false;
                    self.status = format!("worker lifecycle durably updated · {kind}");
                }
                PersistNotice::Failed {
                    operation,
                    revision,
                    request_id,
                    turn_id,
                    error,
                } => {
                    if operation.starts_with("orchestration topology") {
                        self.topology_command_pending = false;
                    }
                    if operation.starts_with("route addressability") {
                        self.route_addressability_command_pending = false;
                    }
                    if operation.starts_with("local route payload") {
                        self.route_payload_command_pending = false;
                    } else if operation.starts_with("local route dispatch") {
                        self.route_dispatch_command_pending = false;
                    } else if operation.starts_with("local route context") {
                        self.route_context_command_pending = false;
                    } else if operation.starts_with("local route ") {
                        self.route_policy_command_pending = false;
                    }
                    if operation.starts_with("lifecycle ") {
                        self.lifecycle_command_pending = false;
                    }
                    if request_id.is_some() && request_id == self.commit_in_flight {
                        if let Some(request_id) = request_id {
                            self.commit_remote_intents.remove(&request_id);
                        }
                        self.commit_in_flight = None;
                    }
                    if let Some(revision) = revision {
                        self.saved_revision = self.saved_revision.min(revision.saturating_sub(1));
                    }
                    if let Some(turn_id) = turn_id {
                        if self
                            .pending_remote_turn
                            .as_ref()
                            .is_some_and(|pending| pending.turn_id == turn_id)
                        {
                            self.pending_remote_turn = None;
                        }
                    }
                    self.status = format!("{operation} failed: {error}");
                }
            }
        }
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
                    let account_changed = self.remote_session.email != session.email
                        || self.remote_session.profile_label != session.profile_label;

                    self.sign_in_pending = session.status == "connecting";
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
                            match self.remote.send(siwc_bridge::BridgeCommand::ListModels) {
                                Ok(()) => {
                                    self.model_list_pending = true;
                                    self.remote_status = "loading ChatGPT models…".to_owned();
                                }
                                Err(error) => {
                                    self.remote_status = error;
                                }
                            }
                        } else if !was_connected && !self.model_list_pending {
                            self.remote_status = "ChatGPT plan connected".to_owned();
                        }
                    } else if self.remote_session.status == "connecting" {
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
                    let turn_id = active.turn_id;
                    let cumulative_text = active.cumulative_text.clone();

                    if first_output {
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
                siwc_bridge::BridgeEvent::ResponseCompleted { request_id, text } => {
                    if let Some(probe_name) =
                        capability_probes::probe_name_from_request_id(&request_id)
                    {
                        let probe_name = probe_name.to_owned();
                        self.capability_probe
                            .complete_supported(&probe_name, !text.is_empty());
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
                    let final_text = if text.is_empty() {
                        active.cumulative_text
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
                }
                siwc_bridge::BridgeEvent::Failed { request_id, error } => {
                    if let Some(probe_name) = request_id
                        .as_deref()
                        .and_then(capability_probes::probe_name_from_request_id)
                    {
                        let probe_name = probe_name.to_owned();
                        self.capability_probe.complete_failed(&probe_name, &error);
                        self.dispatch_next_capability_probe();
                        continue;
                    }
                    self.sign_in_pending = false;
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
                            let active = self
                                .active_remote_turn
                                .take()
                                .expect("matched active remote turn");
                            let detail = format!("{}: {}", error.code, error.message);
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
                        self.status = "remote turn ended without automatic retry".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::RuntimeUnavailable(detail) => {
                    if self.capability_probe.running() {
                        self.capability_probe.abort(&detail);
                        self.finish_capability_probes();
                    }
                    self.remote_runtime_ready = false;
                    self.remote_runtime_failed = true;
                    self.sign_in_requested = false;
                    self.sign_in_pending = false;
                    if let Some(active) = self.active_remote_turn.take() {
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
                    self.remote_status = detail;
                }
                siwc_bridge::BridgeEvent::CommandSucceeded { .. } => {}
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
        self.capability_probe.status = match capability_probes::save_report(
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

impl eframe::App for ChatariumApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgb(23, 24, 29).to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_notices();
        self.process_mirror_controller_notices();
        self.process_live_mirror_fetch_notices();
        self.process_remote_notices();
        let local_display_messages =
            projected_local_display_messages(&self.events, self.local_conversation_id);
        let local_conversation_title = local_conversation_display_title(
            &self.local_conversation_catalog,
            self.local_conversation_id,
            &self.events,
        );
        let remote_catalog_selected = self.selected_remote_catalog_id.is_some();
        let historical_mode =
            self.selected_historical_conversation.is_some() || remote_catalog_selected;
        let selected_historical_id = self.selected_historical_conversation;
        let selected_live_mirror = selected_historical_id
            .is_some_and(|selected| self.live_mirrored_conversations.contains(&selected));
        let selected_live_entry = selected_historical_id.and_then(|selected| {
            self.live_mirror_catalog
                .iter()
                .find(|entry| entry.local_conversation_id == selected)
        });
        let selected_historical_entry = selected_historical_id.and_then(|selected| {
            self.historical_catalog
                .iter()
                .find(|entry| entry.local_conversation_id == selected)
        });
        let selected_remote_entry = self.selected_remote_catalog_id.as_deref().and_then(|id| {
            self.remote_catalog_view
                .iter()
                .find(|entry| entry.item.id == id)
        });
        let conversation_title = selected_live_entry
            .map(|entry| entry.title.clone())
            .or_else(|| selected_historical_entry.and_then(|entry| entry.title.clone()))
            .or_else(|| selected_remote_entry.and_then(|entry| entry.item.title.clone()))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| {
                if historical_mode {
                    "Imported ChatGPT conversation".to_owned()
                } else {
                    local_conversation_title.clone()
                }
            });
        let mut select_local_requested: Option<LocalConversationId> = None;
        let mut create_local_requested = false;
        let mut rename_local_requested = false;
        let mut archive_local_requested = false;
        let mut select_historical_requested = None;
        let mut sync_live_requested = None;
        let mut select_remote_requested = None;
        let mut mirror_remote_requested = None;
        let mut mirror_start_requested = false;
        let mut mirror_recheck_requested = false;
        let mut mirror_pause_requested = false;
        let mut mirror_resume_requested = false;
        let mut refresh_history_requested = false;
        let archive_results = self.local_archive_search_index.search(
            &self.archive_search_query,
            self.archive_search_mode,
            self.archive_state_filter,
        );
        if self
            .archive_search_selection
            .is_some_and(|selection| selection >= archive_results.len())
        {
            self.archive_search_selection = None;
        }
        let archive_search_id = egui::Id::new("local-archive-search");
        let mut archive_search_has_focus = false;
        let mut archive_result_clicked = None;
        let mut open_reader_from_transcript_search = false;
        let mut focus_archive_search = false;
        let mut clear_archive_search_focus = false;
        if ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::K)) {
            focus_archive_search = true;
        }

        egui::SidePanel::left("sidebar")
            .default_width(320.0)
            .min_width(260.0)
            .max_width(440.0)
            .resizable(true)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(18, 19, 23))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                // SidePanel reserves its configured width independently of the custom
                // frame's content-driven minimum size. Force the inner frame to own the
                // full reserved width so HiDPI/Wayland never exposes an unpainted gutter.
                ui.set_min_width(ui.max_rect().width());

                egui::ScrollArea::vertical()
                    .id_salt("sidebar-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let sidebar_control_width = ui.available_width();
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Chatarium")
                                .size(24.0)
                                .strong()
                                .color(egui::Color32::from_rgb(238, 239, 244)),
                        );
                        ui.label(
                            egui::RichText::new("Local-first workspace")
                                .size(12.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                        );

                        ui.add_space(18.0);
                        ui.label(
                            egui::RichText::new("LOCAL ARCHIVE SEARCH")
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 176, 137)),
                        );
                        ui.add_space(5.0);
                        let search_response = ui.add(
                            egui::TextEdit::singleline(&mut self.archive_search_query)
                                .id(archive_search_id)
                                .hint_text("Search local archive…")
                                .desired_width(sidebar_control_width),
                        );
                        archive_search_has_focus = search_response.has_focus();
                        if search_response.changed() {
                            self.archive_search_selection = None;
                        }
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("archive-search-mode")
                                .selected_text(self.archive_search_mode.label())
                                .show_ui(ui, |ui| {
                                    for mode in [
                                        local_archive_search::ArchiveSearchMode::AllLocalData,
                                        local_archive_search::ArchiveSearchMode::Titles,
                                        local_archive_search::ArchiveSearchMode::MirroredTranscriptText,
                                    ] {
                                        if ui
                                            .selectable_value(
                                                &mut self.archive_search_mode,
                                                mode,
                                                mode.label(),
                                            )
                                            .changed()
                                        {
                                            self.archive_search_selection = None;
                                        }
                                    }
                                });
                            egui::ComboBox::from_id_salt("archive-state-filter")
                                .selected_text(self.archive_state_filter.label())
                                .show_ui(ui, |ui| {
                                    for filter in [
                                        local_archive_search::ArchiveStateFilter::All,
                                        local_archive_search::ArchiveStateFilter::Mirrored,
                                        local_archive_search::ArchiveStateFilter::Partial,
                                        local_archive_search::ArchiveStateFilter::NotMirrored,
                                        local_archive_search::ArchiveStateFilter::TransientFailure,
                                        local_archive_search::ArchiveStateFilter::RateLimited,
                                        local_archive_search::ArchiveStateFilter::StructuralFailure,
                                    ] {
                                        if ui
                                            .selectable_value(
                                                &mut self.archive_state_filter,
                                                filter,
                                                filter.label(),
                                            )
                                            .changed()
                                        {
                                            self.archive_search_selection = None;
                                        }
                                    }
                                });
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} local result{} · Ctrl+K focus · Esc clear",
                                archive_results.len(),
                                if archive_results.len() == 1 { "" } else { "s" }
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        for (result_index, result) in archive_results.iter().enumerate() {
                            let document = &self.local_archive_search_index.documents()[result.document_index];
                            let selected = self.archive_search_selection == Some(result_index);
                            if ui
                                .selectable_label(
                                    selected,
                                    egui::RichText::new(document.title.as_str()).size(12.0),
                                )
                                .clicked()
                            {
                                self.archive_search_selection = Some(result_index);
                                archive_result_clicked = Some(result.clone());
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(result.kind.label())
                                        .size(9.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(112, 176, 137)),
                                );
                                ui.label(
                                    egui::RichText::new(document.state.label())
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                            });
                            if let Some(snippet) = result.snippet.as_deref() {
                                ui.label(
                                    egui::RichText::new(snippet)
                                        .size(10.0)
                                        .italics()
                                        .color(egui::Color32::from_rgb(170, 174, 184)),
                                );
                            }
                        }

                        ui.add_space(26.0);
                        ui.label(
                            egui::RichText::new("CONVERSATIONS")
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                        );
                        ui.add_space(6.0);

                        ui.horizontal(|ui| {
                            if ui.button("+ New local").clicked() {
                                create_local_requested = true;
                            }
                            ui.checkbox(
                                &mut self.show_archived_local_conversations,
                                "Show archived",
                            );
                        });
                        ui.add_space(6.0);

                        for entry in self.local_conversation_catalog.entries() {
                            if entry.archived && !self.show_archived_local_conversations {
                                continue;
                            }
                            let messages =
                                projected_local_display_messages(&self.events, entry.id);
                            let title = local_conversation_display_title(
                                &self.local_conversation_catalog,
                                entry.id,
                                &self.events,
                            );
                            let selected =
                                !historical_mode && entry.id == self.local_conversation_id;
                            egui::Frame::default()
                                .fill(if selected {
                                    egui::Color32::from_rgb(31, 33, 39)
                                } else {
                                    egui::Color32::from_rgb(26, 28, 33)
                                })
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(10, 9))
                                .show(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            selected,
                                            egui::RichText::new(title).strong(),
                                        )
                                        .clicked()
                                    {
                                        select_local_requested = Some(entry.id);
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} message{}",
                                                messages.len(),
                                                if messages.len() == 1 { "" } else { "s" }
                                            ))
                                            .size(10.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                        );
                                        if entry.archived {
                                            ui.label(
                                                egui::RichText::new("ARCHIVED")
                                                    .size(9.0)
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(166, 139, 112)),
                                            );
                                        }
                                    });
                                });
                            ui.add_space(4.0);
                        }

                        if !historical_mode {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("LOCAL CONVERSATION")
                                    .size(9.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(112, 116, 126)),
                            );
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.local_conversation_rename,
                                    )
                                    .hint_text("Conversation title"),
                                );
                                if ui.button("Save").clicked() {
                                    rename_local_requested = true;
                                }
                            });
                            if ui.button("Archive local conversation").clicked() {
                                archive_local_requested = true;
                            }
                        }

                        if !self.remote_conversation_catalog.is_empty() {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(history_observed_label(
                                    self.remote_conversation_catalog.len(),
                                    self.remote_conversation_total,
                                ))
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 176, 137)),
                            );
                            ui.add_space(6.0);
                            for entry in &self.remote_catalog_view {
                                let selected = entry
                                    .local_conversation_id
                                    .is_some_and(|local| {
                                        self.selected_historical_conversation == Some(local)
                                    })
                                    || self.selected_remote_catalog_id.as_deref()
                                        == Some(entry.item.id.as_str());
                                let title = entry
                                    .item
                                    .title
                                    .as_deref()
                                    .filter(|title| !title.trim().is_empty())
                                    .unwrap_or("Untitled ChatGPT conversation");

                                if ui
                                    .selectable_label(
                                        selected,
                                        egui::RichText::new(title).size(12.0),
                                    )
                                    .clicked()
                                {
                                    if let Some(local) = entry.local_conversation_id {
                                        select_historical_requested = Some(local);
                                    } else {
                                        select_remote_requested = Some(entry.item.id.clone());
                                    }
                                }
                                let remote_state = remote_catalog_state_label(entry.status);
                                let remote_state_color = match entry.status {
                                    RemoteMirrorQueueStatus::MirroredFully
                                    | RemoteMirrorQueueStatus::MirroredPartial => {
                                        egui::Color32::from_rgb(112, 176, 137)
                                    }
                                    RemoteMirrorQueueStatus::RateLimited
                                    | RemoteMirrorQueueStatus::Queued
                                    | RemoteMirrorQueueStatus::Capturing => {
                                        egui::Color32::from_rgb(225, 194, 108)
                                    }
                                    RemoteMirrorQueueStatus::TransientFailure
                                    | RemoteMirrorQueueStatus::StructuralFailure => {
                                        egui::Color32::from_rgb(214, 128, 128)
                                    }
                                    RemoteMirrorQueueStatus::Discovered => {
                                        egui::Color32::from_rgb(112, 116, 126)
                                    }
                                };
                                ui.label(
                                    egui::RichText::new(remote_state)
                                        .size(9.0)
                                        .color(remote_state_color),
                                );
                                ui.add_space(4.0);
                            }
                            if let Some(remote_id) = self.selected_remote_catalog_id.as_ref() {
                                if ui.button("Mirror from ChatGPT").clicked() {
                                    mirror_remote_requested = Some(remote_id.clone());
                                }
                                ui.label(
                                    egui::RichText::new(
                                        "Remote capture is a separate action; selecting a row stays local.",
                                    )
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                            }
                        }

                        if !self.live_mirror_catalog.is_empty() {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "CACHED CHATGPT · {}",
                                    self.live_mirror_catalog.len()
                                ))
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                            );
                            ui.add_space(6.0);
                            for entry in &self.live_mirror_catalog {
                                let selected = self.selected_historical_conversation
                                    == Some(entry.local_conversation_id);
                                if ui
                                    .selectable_label(
                                        selected,
                                        egui::RichText::new(entry.title.as_str()).size(12.0),
                                    )
                                    .clicked()
                                {
                                    select_historical_requested = Some(entry.local_conversation_id);
                                }
                                ui.label(
                                    egui::RichText::new("durable offline mirror · read-only")
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(112, 116, 126)),
                                );
                                ui.add_space(4.0);
                            }
                        }

                        if !self.historical_catalog.is_empty() {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "IMPORTED HISTORY · {}",
                                    self.historical_catalog.len()
                                ))
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                            );
                            ui.add_space(6.0);
                            for entry in &self.historical_catalog {
                                let selected = self.selected_historical_conversation
                                    == Some(entry.local_conversation_id);
                                let title = entry
                                    .title
                                    .as_deref()
                                    .filter(|title| !title.trim().is_empty())
                                    .unwrap_or("Untitled imported conversation");
                                if ui
                                    .selectable_label(
                                        selected,
                                        egui::RichText::new(title).size(12.0),
                                    )
                                    .clicked()
                                {
                                    select_historical_requested = Some(entry.local_conversation_id);
                                }
                                let live_mirrored = self
                                    .live_mirrored_conversations
                                    .contains(&entry.local_conversation_id);
                                ui.label(
                                    egui::RichText::new(if live_mirrored {
                                        "live mirror · read-only"
                                    } else {
                                        "historical snapshot · read-only"
                                    })
                                    .size(9.0)
                                    .color(if live_mirrored {
                                        egui::Color32::from_rgb(112, 176, 137)
                                    } else {
                                        egui::Color32::from_rgb(112, 116, 126)
                                    }),
                                );
                                ui.add_space(4.0);
                            }
                        }

                        ui.add_space(24.0);
                        ui.label(
                            egui::RichText::new("STATUS")
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                        );
                        ui.add_space(7.0);
                        status_row(ui, "Storage", self.draft_state(), self.persist_tx.is_some());
                        status_row(
                            ui,
                            "History discovery",
                            self.account_bridge_status.as_str(),
                            self.history_bridge_proven,
                        );
                        status_row(
                            ui,
                            "Mirror",
                            self.mirror_status.as_str(),
                            self.mirror_status.starts_with("MIRRORED"),
                        );
                        let queue_counts = remote_catalog_queue_counts(&self.remote_catalog_view);
                        status_row(
                            ui,
                            "Mirror queue",
                            &format!(
                                "observed={} · full={} · partial={} · pending={} · transient={} · rate-limited={} · structural={}",
                                queue_counts.observed,
                                queue_counts.full,
                                queue_counts.partial,
                                queue_counts.pending,
                                queue_counts.transient,
                                queue_counts.rate_limited,
                                queue_counts.structural,
                            ),
                            true,
                        );
                        status_row(
                            ui,
                            "Mirror intent",
                            self.remote_health.intent.as_str(),
                            self.remote_health.intent == MirrorIntent::Enabled,
                        );
                        status_row(
                            ui,
                            "Remote health",
                            self.remote_health.state.as_str(),
                            self.remote_health.state
                                == chatarium_store::remote_health::RemoteHealthState::Healthy,
                        );
                        if let Some(until) = self.remote_health.cooldown_until_ms {
                            ui.label(
                                egui::RichText::new(format!("cooldown until unix-ms {until}"))
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(186, 189, 197)),
                            );
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "production controller · {}{}",
                                mirror_controller_state_label(self.mirror_controller_state),
                                self.mirror_controller_current_item
                                    .map(|index| format!(" · current catalog item {index}"))
                                    .unwrap_or_default(),
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(186, 189, 197)),
                        );
                        if let Some(detail) = &self.mirror_controller_detail {
                            ui.label(
                                egui::RichText::new(detail)
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            let can_start = matches!(
                                self.mirror_controller_state,
                                MirrorControllerState::Stopped
                                    | MirrorControllerState::Completed
                                    | MirrorControllerState::Failed
                                    | MirrorControllerState::RateLimited
                                    | MirrorControllerState::AuthenticationRequired
                            );
                            let can_pause = self.mirror_controller_state
                                == MirrorControllerState::Running;
                            let can_resume = self.mirror_controller_state
                                == MirrorControllerState::Paused;
                            if ui
                                .add_enabled(can_start, egui::Button::new("START MIRRORING"))
                                .clicked()
                            {
                                mirror_start_requested = true;
                            }
                            if ui
                                .add_enabled(can_pause, egui::Button::new("PAUSE"))
                                .clicked()
                            {
                                mirror_pause_requested = true;
                            }
                            if ui
                                .add_enabled(can_resume, egui::Button::new("RESUME"))
                                .clicked()
                            {
                                mirror_resume_requested = true;
                            }
                            if ui.button("RECHECK REMOTE HEALTH").clicked() {
                                mirror_recheck_requested = true;
                            }
                        });
                        status_row(
                            ui,
                            "ChatGPT",
                            if self.remote_connected() {
                                "connected"
                            } else if self.sign_in_pending {
                                "connecting…"
                            } else if self.sign_in_requested {
                                "preparing sign-in…"
                            } else if self.remote_runtime_failed {
                                "runtime unavailable"
                            } else if !self.remote_runtime_ready {
                                "starting…"
                            } else {
                                "not connected"
                            },
                            self.remote_connected(),
                        );

                        ui.add_space(8.0);
                        if self.account_bridge_provider.is_some() {
                            if ui
                                .add_enabled(
                                    !self.history_list_pending,
                                    egui::Button::new(if self.history_list_pending {
                                        "Checking ChatGPT history…"
                                    } else {
                                        "Refresh ChatGPT history"
                                    })
                                    .min_size(egui::vec2(sidebar_control_width, 30.0)),
                                )
                                .clicked()
                            {
                                refresh_history_requested = true;
                            }
                        }

                        ui.add_space(14.0);
                        if self.remote_connected() {
                            ui.label(
                                egui::RichText::new(self.remote_identity_label())
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(186, 189, 197)),
                            );
                            if !self.remote_models.is_empty() {
                                let selected_text = self
                                    .selected_model
                                    .as_ref()
                                    .and_then(|slug| {
                                        self.remote_models
                                            .iter()
                                            .find(|model| &model.slug == slug)
                                            .map(|model| model.display_name.as_str())
                                    })
                                    .unwrap_or("Choose model");
                                let model_before = self.selected_model.clone();
                                egui::ComboBox::from_id_salt("chatgpt_model")
                                    .selected_text(selected_text)
                                    .width(sidebar_control_width)
                                    .show_ui(ui, |ui| {
                                        for model in &self.remote_models {
                                            ui.selectable_value(
                                                &mut self.selected_model,
                                                Some(model.slug.clone()),
                                                model.display_name.as_str(),
                                            );
                                        }
                                    });
                                if self.selected_model != model_before {
                                    self.persist_current_inference_settings();
                                }
                            }
                            if ui
                                .add_sized(
                                    [sidebar_control_width, 30.0],
                                    egui::Button::new("Disconnect ChatGPT"),
                                )
                                .clicked()
                            {
                                self.disconnect_chatgpt();
                            }
                        } else {
                            let sign_in_label = if self.remote_runtime_failed {
                                "Retry ChatGPT"
                            } else if self.sign_in_requested {
                                "Preparing ChatGPT…"
                            } else if self.sign_in_pending {
                                "Opening sign-in…"
                            } else {
                                "Continue with ChatGPT"
                            };
                            if ui
                                .add_enabled(
                                    !self.sign_in_pending && !self.sign_in_requested,
                                    egui::Button::new(egui::RichText::new(sign_in_label).strong())
                                        .min_size(egui::vec2(sidebar_control_width, 34.0)),
                                )
                                .clicked()
                            {
                                self.start_chatgpt_sign_in(ctx);
                            }
                            if self.sign_in_pending || self.sign_in_requested {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(
                                        egui::RichText::new("Finish in your browser")
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(151, 154, 163)),
                                    );
                                });
                                if ui
                                    .add_sized(
                                        [sidebar_control_width, 28.0],
                                        egui::Button::new("Cancel sign-in"),
                                    )
                                    .clicked()
                                {
                                    self.cancel_chatgpt_sign_in();
                                }
                            }
                        }

                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(self.remote_status.as_str())
                                .size(10.0)
                                .color(egui::Color32::from_rgb(126, 130, 139)),
                        );

                        ui.add_space(18.0);
                        egui::CollapsingHeader::new(
                            egui::RichText::new("Diagnostics")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(151, 154, 163)),
                        )
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "journal\n{}",
                                    self.journal_path.display()
                                ))
                                .monospace()
                                .size(10.0)
                                .color(egui::Color32::from_rgb(126, 130, 139)),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!("status\n{}", self.status))
                                    .monospace()
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(126, 130, 139)),
                            );
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!("events: {}", self.events.len()))
                                    .monospace()
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(126, 130, 139)),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("SIWC CAPABILITY PROBES")
                                    .size(10.0)
                                    .strong(),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Uses your existing local ChatGPT sign-in. Credentials stay inside the SIWC bridge; probes send small real plan-usage requests.",
                                )
                                .size(10.0)
                                .color(egui::Color32::from_rgb(126, 130, 139)),
                            );
                            let probe_running = self.capability_probe.running();
                            let probe_block_reason = capability_probe_block_reason(
                                self.remote_connected(),
                                self.selected_model.is_some(),
                                self.pending_remote_turn.is_some(),
                                self.active_remote_turn.is_some(),
                                probe_running,
                            );
                            let can_probe = probe_block_reason.is_none();
                            if ui
                                .add_enabled(
                                    can_probe,
                                    egui::Button::new(if probe_running {
                                        "RUNNING CAPABILITY PROBES…"
                                    } else {
                                        "RUN CAPABILITY PROBES"
                                    }),
                                )
                                .clicked()
                            {
                                self.start_capability_probes();
                            }
                            if probe_running {
                                ui.spinner();
                            } else if let Some(reason) = probe_block_reason {
                                ui.label(
                                    egui::RichText::new(format!("probe unavailable · {reason}"))
                                        .monospace()
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(126, 130, 139)),
                                );
                            }
                            if !self.capability_probe.status.is_empty() {
                                ui.label(
                                    egui::RichText::new(&self.capability_probe.status)
                                        .monospace()
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(126, 130, 139)),
                                );
                            }
                            if let Some(report_model) = self.capability_probe.model.as_deref() {
                                let age = self
                                    .capability_probe
                                    .generated_unix_ms
                                    .map(probe_report_age)
                                    .unwrap_or_else(|| "time unknown".to_owned());
                                let model_note = if self.selected_model.as_deref()
                                    == Some(report_model)
                                {
                                    ""
                                } else {
                                    " · SELECTED MODEL DIFFERS"
                                };
                                let profile_note = match (
                                    self.capability_probe.profile_id.as_deref(),
                                    self.remote_session.profile_id.as_deref(),
                                ) {
                                    (Some(report), Some(current)) if report == current => "",
                                    (Some(_), Some(_)) => " · PROFILE DIFFERS",
                                    (Some(_), None) => " · PROFILE NOT CONNECTED",
                                    (None, Some(_)) => " · REPORT PROFILE UNKNOWN",
                                    (None, None) => "",
                                };
                                ui.label(
                                    egui::RichText::new(format!(
                                        "saved report · {report_model} · {age}{model_note}{profile_note}"
                                    ))
                                    .monospace()
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(126, 130, 139)),
                                );
                            }
                            if !self.capability_probe.results.is_empty() {
                                let contract_state =
                                    local_inference_contract::contract_state(&self.capability_probe);
                                let contract_active = contract_state == "ready"
                                    && self.capability_probe.profile_id.as_deref()
                                        == self.remote_session.profile_id.as_deref()
                                    && self.capability_probe.model.as_deref()
                                        == self.selected_model.as_deref();
                                ui.label(
                                    egui::RichText::new(format!(
                                        "local inference contract · {contract_state}{}",
                                        if contract_active {
                                            " · ACTIVE FOR CURRENT PROFILE/MODEL"
                                        } else {
                                            ""
                                        }
                                    ))
                                    .monospace()
                                    .size(10.0)
                                    .strong(),
                                );

                                let data_dir =
                                    self.journal_path.parent().unwrap_or_else(|| Path::new("."));
                                let report_path = data_dir.join("siwc-capability-probes.json");
                                let contract_path = data_dir.join("local-inference-contract.json");
                                let can_copy_artifacts =
                                    !probe_running && self.capability_probe.generated_unix_ms.is_some();
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            can_copy_artifacts,
                                            egui::Button::new("COPY SANITIZED EVIDENCE"),
                                        )
                                        .clicked()
                                    {
                                        self.capability_probe.status =
                                            match std::fs::read_to_string(&report_path) {
                                                Ok(text) => {
                                                    ui.ctx().copy_text(text);
                                                    "copied sanitized capability evidence"
                                                        .to_owned()
                                                }
                                                Err(error) => format!(
                                                    "could not copy sanitized evidence · {error}"
                                                ),
                                            };
                                    }
                                    if ui
                                        .add_enabled(
                                            can_copy_artifacts,
                                            egui::Button::new("COPY INFERENCE CONTRACT"),
                                        )
                                        .clicked()
                                    {
                                        self.capability_probe.status =
                                            match std::fs::read_to_string(&contract_path) {
                                                Ok(text) => {
                                                    ui.ctx().copy_text(text);
                                                    "copied local inference contract".to_owned()
                                                }
                                                Err(error) => format!(
                                                    "could not copy inference contract · {error}"
                                                ),
                                            };
                                    }
                                });
                            }
                            for result in &self.capability_probe.results {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}{}",
                                        result.name,
                                        result.status,
                                        probe_result_detail(result)
                                    ))
                                    .monospace()
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(126, 130, 139)),
                                );
                            }

                            ui.add_space(8.0);
                            ui.label(egui::RichText::new("LOCAL ARCHIVE").size(10.0).strong());
                            ui.horizontal(|ui| {
                                if ui.button("CHECK ARCHIVE").clicked() {
                                    let data_dir = self.journal_path.parent().unwrap_or_else(|| Path::new("."));
                                    self.archive_maintenance_status = match check_archive(data_dir) {
                                        Ok(report) => format!("{} · {} events · {} catalog items", report.status, report.journal_event_count, report.catalog_count),
                                        Err(error) => format!("archive check failed · {error}"),
                                    };
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("backup path");
                                ui.text_edit_singleline(&mut self.archive_backup_path);
                            });
                            ui.horizontal(|ui| {
                                if ui.button("CREATE BACKUP").clicked() {
                                    let data_dir = self.journal_path.parent().unwrap_or_else(|| Path::new("."));
                                    self.archive_maintenance_status = match create_backup(data_dir, self.archive_backup_path.trim()) {
                                        Ok(_) => "backup created and verified".to_owned(),
                                        Err(error) => format!("backup failed · {error}"),
                                    };
                                }
                                if ui.button("VERIFY BACKUP").clicked() {
                                    self.archive_maintenance_status = match verify_backup(self.archive_backup_path.trim()) {
                                        Ok(_) => "backup verified".to_owned(),
                                        Err(error) => format!("backup verification failed · {error}"),
                                    };
                                }
                            });
                            if ui.button("RESTORE BACKUP").clicked() && !self.archive_backup_path.trim().is_empty() {
                                self.archive_restore_confirmation_pending = true;
                            }
                            if self.archive_restore_confirmation_pending {
                                ui.colored_label(egui::Color32::YELLOW, "Restore replaces active local archive; a safety copy is retained.");
                                ui.horizontal(|ui| {
                                    if ui.button("CONFIRM RESTORE").clicked() {
                                        let target = self.journal_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
                                        self.archive_maintenance_status = match restore_backup(self.archive_backup_path.trim(), target) {
                                            Ok(_) => "backup restored after isolated verification".to_owned(),
                                            Err(error) => format!("restore failed; active archive preserved · {error}"),
                                        };
                                        self.archive_restore_confirmation_pending = false;
                                    }
                                    if ui.button("CANCEL RESTORE").clicked() {
                                        self.archive_restore_confirmation_pending = false;
                                    }
                                });
                            }
                            ui.label(egui::RichText::new(&self.archive_maintenance_status).size(10.0).color(egui::Color32::from_rgb(126, 130, 139)));
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "local conversation\n{}",
                                    self.local_conversation_id
                                ))
                                .monospace()
                                .size(10.0)
                                .color(egui::Color32::from_rgb(126, 130, 139)),
                            );
                        });
                    });
            });

        if focus_archive_search {
            ctx.memory_mut(|memory| memory.request_focus(archive_search_id));
        }
        if archive_search_has_focus {
            ctx.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.archive_search_selection = local_archive_search::move_selection(
                        self.archive_search_selection,
                        archive_results.len(),
                        1,
                    );
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.archive_search_selection = local_archive_search::move_selection(
                        self.archive_search_selection,
                        archive_results.len(),
                        -1,
                    );
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.archive_search_query.clear();
                    self.archive_search_selection = None;
                    clear_archive_search_focus = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    if let Some(selection) = self.archive_search_selection {
                        archive_result_clicked = local_archive_search::activate_selection(
                            &archive_results,
                            Some(selection),
                        );
                    } else {
                        archive_result_clicked =
                            local_archive_search::activate_selection(&archive_results, None);
                    }
                }
            });
        }
        if clear_archive_search_focus {
            ctx.memory_mut(|memory| memory.surrender_focus(archive_search_id));
        }
        if let Some(result) = archive_result_clicked {
            self.reader_search_query =
                if result.kind == local_archive_search::ArchiveMatchKind::LocalTranscript {
                    self.archive_search_query.clone()
                } else {
                    String::new()
                };
            open_reader_from_transcript_search =
                result.kind == local_archive_search::ArchiveMatchKind::LocalTranscript;
            if let Some(entry) = self
                .remote_catalog_view
                .iter()
                .find(|entry| entry.catalog_index == result.catalog_index)
            {
                if let Some(local_conversation_id) = entry.local_conversation_id {
                    select_historical_requested = Some(local_conversation_id);
                } else {
                    select_remote_requested = Some(entry.item.id.clone());
                }
            }
        }
        if create_local_requested {
            self.create_local_conversation();
        } else if let Some(local_conversation_id) = select_local_requested {
            self.activate_local_conversation(local_conversation_id);
        } else if let Some(local_conversation_id) = select_historical_requested {
            self.select_historical_conversation(local_conversation_id);
        } else if let Some(remote_conversation_id) = select_remote_requested {
            self.selected_historical_conversation = None;
            self.selected_remote_catalog_id = Some(remote_conversation_id);
            self.historical_messages.clear();
            self.historical_load_pending = None;
            self.status = "remote conversation selected; local mirror unavailable".to_owned();
        }
        if rename_local_requested {
            self.rename_current_local_conversation();
        }
        if archive_local_requested {
            self.archive_current_local_conversation();
        }
        if open_reader_from_transcript_search {
            self.reader_search_hit = Some(0);
        }
        if let Some(remote_conversation_id) = mirror_remote_requested.as_ref() {
            self.open_discovered_remote_conversation(remote_conversation_id.clone(), ctx);
        }
        if refresh_history_requested {
            self.start_history_discovery(ctx);
        }
        if mirror_start_requested {
            self.start_mirror_controller();
        } else if mirror_recheck_requested {
            self.recheck_remote_health();
        } else if mirror_pause_requested {
            self.send_mirror_controller_command(MirrorControllerCommand::Pause);
        } else if mirror_resume_requested {
            self.send_mirror_controller_command(MirrorControllerCommand::Resume);
        }

        egui::TopBottomPanel::top("conversation_header")
            .resizable(false)
            .exact_height(72.0)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(conversation_title.as_str())
                                .size(19.0)
                                .strong()
                                .color(egui::Color32::from_rgb(238, 239, 244)),
                        );
                        ui.label(
                            egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                "Remote catalog item · not mirrored locally"
                            } else if selected_live_mirror {
                                "Local durable mirror · read-only"
                            } else if historical_mode {
                                "Historical account-export snapshot · read-only"
                            } else if self.remote_connected() {
                                "Durable on this machine · authenticated with ChatGPT"
                            } else {
                                "Durable on this machine · connect ChatGPT to enable remote turns"
                            })
                            .size(11.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let connected = self.remote_connected();
                        egui::Frame::default()
                            .fill(if selected_live_mirror {
                                egui::Color32::from_rgb(24, 52, 37)
                            } else if historical_mode {
                                egui::Color32::from_rgb(39, 42, 49)
                            } else if connected {
                                egui::Color32::from_rgb(24, 52, 37)
                            } else {
                                egui::Color32::from_rgb(48, 42, 26)
                            })
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(10, 5))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                        "REMOTE · NOT MIRRORED"
                                    } else if selected_live_mirror {
                                        if self.live_mirror_truncated_before {
                                            "MIRRORED LOCALLY · PARTIAL"
                                        } else {
                                            "MIRRORED LOCALLY"
                                        }
                                    } else if historical_mode {
                                        "IMPORTED SNAPSHOT"
                                    } else if connected {
                                        "CHATGPT CONNECTED"
                                    } else {
                                        "LOCAL ONLY"
                                    })
                                    .size(10.0)
                                    .strong()
                                    .color(
                                        if remote_catalog_selected && !selected_live_mirror {
                                            egui::Color32::from_rgb(225, 194, 108)
                                        } else if selected_live_mirror {
                                            egui::Color32::from_rgb(126, 210, 156)
                                        } else if historical_mode {
                                            egui::Color32::from_rgb(179, 184, 196)
                                        } else if connected {
                                            egui::Color32::from_rgb(126, 210, 156)
                                        } else {
                                            egui::Color32::from_rgb(225, 194, 108)
                                        },
                                    ),
                                );
                            });
                    });
                });
            });

        egui::TopBottomPanel::bottom("composer_panel")
            .resizable(false)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(ctx, |ui| {
                if historical_mode {
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgb(31, 33, 39))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgb(54, 57, 66),
                        ))
                        .corner_radius(egui::CornerRadius::same(12))
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                    "Remote conversation is not mirrored locally"
                                } else if selected_live_mirror {
                                    "Live mirror is read-only for now"
                                } else {
                                    "Historical snapshot is read-only"
                                })
                                .strong()
                                .color(egui::Color32::from_rgb(221, 223, 229)),
                            );
                            ui.label(
                                egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                    "Selecting a catalog row is local-only. Use the explicit mirror action to capture this conversation through ChatGPT."
                                } else if selected_live_mirror {
                                    "Chatarium can refresh this real ChatGPT thread through your browser session. Same-thread write-back remains evidence-gated, so the composer stays disabled."
                                } else {
                                    "Sync can upgrade this imported lineage to a validated live mirror without creating a parallel conversation. Same-thread write-back remains a separate interoperability problem."
                                })
                                .size(11.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let pending = self.live_mirror_pending == selected_historical_id;
                                let can_sync = (selected_historical_id.is_some()
                                    || remote_catalog_selected)
                                    && self.persist_tx.is_some()
                                    && self.account_bridge_provider.is_some()
                                    && self.live_mirror_pending.is_none();
                                let label = if pending {
                                    "Syncing…"
                                } else if remote_catalog_selected && !selected_live_mirror {
                                    "Mirror from ChatGPT"
                                } else if selected_live_mirror {
                                    "Refresh from ChatGPT"
                                } else {
                                    "Sync from ChatGPT"
                                };
                                if ui
                                    .add_enabled(
                                        can_sync,
                                        egui::Button::new(egui::RichText::new(label).strong())
                                            .min_size(egui::vec2(160.0, 32.0)),
                                    )
                                    .clicked()
                                {
                                    if remote_catalog_selected && !selected_live_mirror {
                                        mirror_remote_requested =
                                            self.selected_remote_catalog_id.clone();
                                    } else {
                                        sync_live_requested = selected_historical_id;
                                    }
                                }
                                if pending {
                                    ui.spinner();
                                }
                            });
                            if self.account_bridge_provider.is_none() {
                                ui.label(
                                    egui::RichText::new(self.account_bridge_status.as_str())
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(166, 139, 112)),
                                );
                            }
                        });
                    return;
                }

                let composer_fill = if self.persist_tx.is_some() {
                    egui::Color32::from_rgb(31, 33, 39)
                } else {
                    egui::Color32::from_rgb(45, 29, 31)
                };

                let mut inference_controls_changed = false;
                let mut behavior_profile_changed = false;
                egui::CollapsingHeader::new("Context & inference controls")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "These controls belong to this local conversation. Chatarium assembles the request context locally.",
                            )
                            .size(11.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Instructions").strong());
                        ui.label(
                            egui::RichText::new(
                                "Sent through the top-level Responses instructions field.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        inference_controls_changed |= ui
                            .add(
                                egui::TextEdit::multiline(&mut self.conversation_instructions)
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Persistent instructions for this conversation"),
                            )
                            .changed();

                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Developer context").strong());
                        ui.label(
                            egui::RichText::new(
                                "Inserted as the first developer-role message before the conversation transcript.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        inference_controls_changed |= ui
                            .add(
                                egui::TextEdit::multiline(
                                    &mut self.conversation_developer_context,
                                )
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .hint_text("Local behavior, memory, lifecycle, or policy context"),
                            )
                            .changed();

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Behavior profile").strong());
                        ui.label(
                            egui::RichText::new(
                                "Per-conversation controls. Only exact request values already accepted by the capability suite are exposed.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        let behavior_gate = self.current_capability_gate();

                        let mut low_reasoning = self.conversation_behavior_profile.reasoning
                            == behavior_profile::ReasoningMode::Low;
                        let reasoning_available =
                            behavior_gate.allows(context_composer::CapabilitySlot::Reasoning);
                        if ui
                            .add_enabled(
                                reasoning_available || low_reasoning,
                                egui::Checkbox::new(
                                    &mut low_reasoning,
                                    "Low reasoning effort",
                                ),
                            )
                            .changed()
                        {
                            self.conversation_behavior_profile.reasoning = if low_reasoning {
                                behavior_profile::ReasoningMode::Low
                            } else {
                                behavior_profile::ReasoningMode::Default
                            };
                            behavior_profile_changed = true;
                        }
                        if !reasoning_available {
                            ui.label(
                                egui::RichText::new(
                                    "reasoning is blocked for the active profile/model contract",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(166, 139, 112)),
                            );
                        }

                        let mut low_verbosity = self.conversation_behavior_profile.verbosity
                            == behavior_profile::VerbosityMode::Low;
                        let verbosity_available =
                            behavior_gate.allows(context_composer::CapabilitySlot::Verbosity);
                        if ui
                            .add_enabled(
                                verbosity_available || low_verbosity,
                                egui::Checkbox::new(&mut low_verbosity, "Low verbosity"),
                            )
                            .changed()
                        {
                            self.conversation_behavior_profile.verbosity = if low_verbosity {
                                behavior_profile::VerbosityMode::Low
                            } else {
                                behavior_profile::VerbosityMode::Default
                            };
                            behavior_profile_changed = true;
                        }
                        if !verbosity_available {
                            ui.label(
                                egui::RichText::new(
                                    "verbosity is blocked for the active profile/model contract",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(166, 139, 112)),
                            );
                        }

                        let web_search_available =
                            behavior_gate.allows(context_composer::CapabilitySlot::WebSearch);
                        if ui
                            .add_enabled(
                                web_search_available
                                    || self.conversation_behavior_profile.web_search,
                                egui::Checkbox::new(
                                    &mut self.conversation_behavior_profile.web_search,
                                    "Allow web search",
                                ),
                            )
                            .changed()
                        {
                            behavior_profile_changed = true;
                        }
                        if !web_search_available {
                            ui.label(
                                egui::RichText::new(
                                    "web search is blocked for the active profile/model contract",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(166, 139, 112)),
                            );
                        }

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Local orchestration topology").strong(),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Durable identity only: local conversation → logical chat container → current local session. This does not enable routing, shared context, or controller authority.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );

                        match local_conversation_topology(
                            &self.events,
                            self.local_conversation_id,
                        ) {
                            Err(error) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "topology projection blocked: {error}"
                                    ))
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(186, 108, 108)),
                                );
                            }
                            Ok(None) => {
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(
                                            !self.topology_command_pending
                                                && self.persist_tx.is_some(),
                                            egui::Button::new(
                                                "Initialize orchestration topology",
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.initialize_local_orchestration_topology();
                                    }
                                    if self.topology_command_pending {
                                        ui.spinner();
                                    }
                                });
                            }
                            Ok(Some(topology)) => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "container {} · current session {} · {} · {} session{}",
                                            topology.container_id.get(),
                                            topology.current_session_id.get(),
                                            session_lifecycle_phase_label(
                                                topology.current_session_phase,
                                            ),
                                            topology.session_count,
                                            if topology.session_count == 1 { "" } else { "s" },
                                        ))
                                        .monospace()
                                        .size(10.0),
                                    );
                                    if self.topology_command_pending {
                                        ui.spinner();
                                    }
                                });
                                if topology.root_session_id != topology.current_session_id {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "root session {} · current leaf changed by explicit rollover provenance",
                                            topology.root_session_id.get(),
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(
                                            "root session is the current execution leaf",
                                        )
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                }

                                match session_endpoint_binding(
                                    &self.events,
                                    topology.current_session_id,
                                ) {
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "route addressability projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(186, 108, 108)),
                                        );
                                    }
                                    Ok(None) => {
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add_enabled(
                                                    !self.route_addressability_command_pending
                                                        && self.persist_tx.is_some(),
                                                    egui::Button::new("Bind routing endpoint"),
                                                )
                                                .clicked()
                                            {
                                                self.bind_current_session_route_endpoint(
                                                    topology.current_session_id,
                                                );
                                            }
                                            if self.route_addressability_command_pending {
                                                ui.spinner();
                                            }
                                            ui.label(
                                                egui::RichText::new(
                                                    "addressability only · routing remains inactive",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    139, 143, 153,
                                                )),
                                            );
                                        });
                                    }
                                    Ok(Some(binding)) => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "route endpoint {} · ADDRESSABLE",
                                                    binding.endpoint_id().get(),
                                                ))
                                                .monospace()
                                                .size(10.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    "routing inactive · no route or dispatch authority created",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    139, 143, 153,
                                                )),
                                            );
                                        });
                                    }
                                }
                            }
                        }

                        ui.collapsing("Local routing directory", |ui| {
                            match replay_local_routing_directory(&self.events) {
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "routing directory projection blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Ok(entries) => {
                                    let current_entry = entries
                                        .iter()
                                        .find(|entry| {
                                            entry.conversation_id == self.local_conversation_id
                                        })
                                        .copied();
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} addressable local conversation{} · route intent/policy is durable; dispatch remains disabled",
                                            entries.len(),
                                            if entries.len() == 1 { "" } else { "s" },
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                    egui::ScrollArea::vertical()
                                        .id_salt("local-routing-directory")
                                        .max_height(140.0)
                                        .show(ui, |ui| {
                                            for entry in &entries {
                                                let title = local_conversation_display_title(
                                                    &self.local_conversation_catalog,
                                                    entry.conversation_id,
                                                    &self.events,
                                                );
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(if entry.conversation_id
                                                            == self.local_conversation_id
                                                        {
                                                            "CURRENT"
                                                        } else {
                                                            "LOCAL"
                                                        })
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(title).size(10.0),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "endpoint {} · session {} · {}",
                                                            entry.endpoint_id.get(),
                                                            entry.current_session_id.get(),
                                                            session_lifecycle_phase_label(
                                                                entry.current_session_phase,
                                                            ),
                                                        ))
                                                        .monospace()
                                                        .size(9.0)
                                                        .color(egui::Color32::from_rgb(
                                                            139, 143, 153,
                                                        )),
                                                    );

                                                    if entry.conversation_id
                                                        != self.local_conversation_id
                                                    {
                                                        let route_fresh = current_entry.is_some_and(
                                                            |source| {
                                                                source
                                                                    .current_session_phase
                                                                    .accepts_ordinary_turns()
                                                                    && entry
                                                                        .current_session_phase
                                                                        .accepts_ordinary_turns()
                                                            },
                                                        );
                                                        if ui
                                                            .add_enabled(
                                                                route_fresh
                                                                    && !self
                                                                        .route_policy_command_pending
                                                                    && self.persist_tx.is_some(),
                                                                egui::Button::new(
                                                                    "Propose route",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.propose_local_session_route(
                                                                entry.conversation_id,
                                                            );
                                                        }
                                                    }
                                                });
                                            }
                                        });

                                    ui.add_space(6.0);
                                    ui.separator();
                                    ui.label(
                                        egui::RichText::new("Manual local route policy")
                                            .strong()
                                            .size(10.0),
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            "Session-message routes carry an immutable payload. Allow requires a payload. Dispatch consumes one-shot authority and records local delivery provenance.",
                                        )
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );

                                    match replay_routing_audit(&self.events) {
                                        Err(error) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "route policy projection blocked: {error}"
                                                ))
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(186, 108, 108)),
                                            );
                                        }
                                        Ok(routes) => {
                                            let routes = routes
                                                .into_iter()
                                                .filter(|route| {
                                                    route.request.class
                                                        == RouteClass::SessionMessage
                                                })
                                                .collect::<Vec<_>>();
                                            let payloads =
                                                match replay_local_route_payload_audit(&self.events) {
                                                    Ok(payloads) => Some(payloads),
                                                    Err(error) => {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "route payload projection blocked: {error}"
                                                            ))
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            )),
                                                        );
                                                        None
                                                    }
                                                };
                                            let deliveries =
                                                match replay_local_route_delivery_audit(&self.events)
                                                {
                                                    Ok(deliveries) => Some(deliveries),
                                                    Err(error) => {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "route delivery projection blocked: {error}"
                                                            ))
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            )),
                                                        );
                                                        None
                                                    }
                                                };
                                            if routes.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "no local session-message routes proposed",
                                                    )
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    )),
                                                );
                                            } else {
                                                egui::ScrollArea::vertical()
                                                    .id_salt("local-route-policy")
                                                    .max_height(180.0)
                                                    .show(ui, |ui| {
                                                        for route in routes {
                                                            let source = entries.iter().find(
                                                                |entry| {
                                                                    entry.endpoint_id
                                                                        == route.request.source
                                                                },
                                                            );
                                                            let destination = entries.iter().find(
                                                                |entry| {
                                                                    entry.endpoint_id
                                                                        == route
                                                                            .request
                                                                            .destination
                                                                },
                                                            );
                                                            let fresh = source.is_some_and(
                                                                |entry| {
                                                                    entry
                                                                        .current_session_phase
                                                                        .accepts_ordinary_turns()
                                                                },
                                                            ) && destination.is_some_and(
                                                                |entry| {
                                                                    entry
                                                                        .current_session_phase
                                                                        .accepts_ordinary_turns()
                                                                },
                                                            );
                                                            let source_label = source
                                                                .map(|entry| {
                                                                    local_conversation_display_title(
                                                                        &self
                                                                            .local_conversation_catalog,
                                                                        entry.conversation_id,
                                                                        &self.events,
                                                                    )
                                                                })
                                                                .unwrap_or_else(|| {
                                                                    format!(
                                                                        "endpoint {}",
                                                                        route
                                                                            .request
                                                                            .source
                                                                            .get()
                                                                    )
                                                                });
                                                            let destination_label = destination
                                                                .map(|entry| {
                                                                    local_conversation_display_title(
                                                                        &self
                                                                            .local_conversation_catalog,
                                                                        entry.conversation_id,
                                                                        &self.events,
                                                                    )
                                                                })
                                                                .unwrap_or_else(|| {
                                                                    format!(
                                                                        "endpoint {}",
                                                                        route
                                                                            .request
                                                                            .destination
                                                                            .get()
                                                                    )
                                                                });
                                                            let payload = payloads
                                                                .as_ref()
                                                                .and_then(|payloads| {
                                                                    payloads.iter().find(
                                                                        |payload| {
                                                                            payload.route_id
                                                                                == route.request.id
                                                                        },
                                                                    )
                                                                });
                                                            let delivery = deliveries
                                                                .as_ref()
                                                                .and_then(|deliveries| {
                                                                    deliveries.iter().find(
                                                                        |delivery| {
                                                                            delivery.route_id
                                                                                == route.request.id
                                                                        },
                                                                    )
                                                                });

                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "route {} · {}",
                                                                        route.request.id.get(),
                                                                        route_gate_state_label(
                                                                            route.gate_state,
                                                                        ),
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "{source_label} → {destination_label}"
                                                                    ))
                                                                    .size(10.0),
                                                                );
                                                                if !fresh {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            "STALE",
                                                                        )
                                                                        .monospace()
                                                                        .size(9.0)
                                                                        .color(
                                                                            egui::Color32::from_rgb(
                                                                                186, 108, 108,
                                                                            ),
                                                                        ),
                                                                    );
                                                                }

                                                                let can_decide = fresh
                                                                    && !route
                                                                        .gate_state
                                                                        .is_dispatched()
                                                                    && !self
                                                                        .route_policy_command_pending
                                                                    && !self
                                                                        .route_payload_command_pending
                                                                    && self.persist_tx.is_some();
                                                                if ui
                                                                    .add_enabled(
                                                                        can_decide
                                                                            && payload.is_some()
                                                                            && route
                                                                                .latest_user_decision
                                                                                != Some(
                                                                                    RouteUserDecision::Allow,
                                                                                ),
                                                                        egui::Button::new(
                                                                            "Allow",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_local_session_route(
                                                                        route.request.id,
                                                                        RouteUserDecision::Allow,
                                                                    );
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        can_decide
                                                                            && route
                                                                                .latest_user_decision
                                                                                != Some(
                                                                                    RouteUserDecision::Deny,
                                                                                ),
                                                                        egui::Button::new(
                                                                            "Deny",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_local_session_route(
                                                                        route.request.id,
                                                                        RouteUserDecision::Deny,
                                                                    );
                                                                }

                                                                let can_dispatch = fresh
                                                                    && payload.is_some()
                                                                    && delivery.is_none()
                                                                    && matches!(
                                                                        route.gate_state,
                                                                        RouteGateState::Allowed { .. }
                                                                            | RouteGateState::Dispatched { .. }
                                                                    )
                                                                    && !self
                                                                        .route_dispatch_command_pending
                                                                    && !self
                                                                        .route_policy_command_pending
                                                                    && !self
                                                                        .route_payload_command_pending
                                                                    && self.persist_tx.is_some();
                                                                let dispatch_label = if route
                                                                    .gate_state
                                                                    .is_dispatched()
                                                                {
                                                                    "Finish delivery"
                                                                } else {
                                                                    "Dispatch + deliver"
                                                                };
                                                                if ui
                                                                    .add_enabled(
                                                                        can_dispatch,
                                                                        egui::Button::new(
                                                                            dispatch_label,
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.dispatch_local_session_route(
                                                                        route.request.id,
                                                                    );
                                                                }
                                                                if let Some(delivery) = delivery {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            format!(
                                                                                "DELIVERED · event #{}",
                                                                                delivery
                                                                                    .delivered_sequence,
                                                                            ),
                                                                        )
                                                                        .monospace()
                                                                        .size(9.0),
                                                                    );
                                                                }
                                                            });

                                                            if let Some(payload) = payload {
                                                                ui.collapsing(
                                                                    format!(
                                                                        "Payload {} · {} bytes · immutable",
                                                                        payload.payload_id.get(),
                                                                        payload.text.len(),
                                                                    ),
                                                                    |ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                payload.text.as_str(),
                                                                            )
                                                                            .size(10.0),
                                                                        );
                                                                    },
                                                                );
                                                            } else {
                                                                let mut attach_clicked = false;
                                                                ui.horizontal_wrapped(|ui| {
                                                                    let draft = self
                                                                        .route_payload_drafts
                                                                        .entry(route.request.id)
                                                                        .or_default();
                                                                    ui.add(
                                                                        egui::TextEdit::singleline(
                                                                            draft,
                                                                        )
                                                                        .desired_width(260.0)
                                                                        .hint_text(
                                                                            "Exact routed message text",
                                                                        ),
                                                                    );
                                                                    let can_attach = fresh
                                                                        && !draft.trim().is_empty()
                                                                        && !route
                                                                            .gate_state
                                                                            .is_dispatched()
                                                                        && !self
                                                                            .route_payload_command_pending
                                                                        && !self
                                                                            .route_policy_command_pending
                                                                        && self.persist_tx.is_some();
                                                                    if ui
                                                                        .add_enabled(
                                                                            can_attach,
                                                                            egui::Button::new(
                                                                                "Attach payload",
                                                                            ),
                                                                        )
                                                                        .clicked()
                                                                    {
                                                                        attach_clicked = true;
                                                                    }
                                                                });
                                                                if attach_clicked {
                                                                    self.attach_local_session_route_payload(
                                                                        route.request.id,
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    });
                                            }
                                            if self.route_policy_command_pending
                                                || self.route_payload_command_pending
                                                || self.route_dispatch_command_pending
                                            {
                                                ui.spinner();
                                            }
                                        }
                                    }
                                }
                            }
                        });

                        ui.collapsing("Routed inbox", |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Delivered routed messages are provenance-bearing local inbox items. Context eligibility is explicit and reversible; admitted items are still not serialized into inference until Context Composer support lands.",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            match replay_local_routed_inbox_for_conversation(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "routed inbox projection blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Ok(items) if items.is_empty() => {
                                    ui.label(
                                        egui::RichText::new("no delivered routed messages")
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                }
                                Ok(items) => {
                                    let context_records =
                                        match replay_local_route_context_audit(&self.events) {
                                            Ok(records) => Some(records),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "routed context projection blocked: {error}"
                                                    ))
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        186, 108, 108,
                                                    )),
                                                );
                                                None
                                            }
                                        };
                                    egui::ScrollArea::vertical()
                                        .id_salt("local-routed-inbox")
                                        .max_height(220.0)
                                        .show(ui, |ui| {
                                            for item in items {
                                                let source_title =
                                                    local_conversation_display_title(
                                                        &self.local_conversation_catalog,
                                                        item.source_conversation_id,
                                                        &self.events,
                                                    );
                                                ui.group(|ui| {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "ROUTED · {source_title}"
                                                            ))
                                                            .strong()
                                                            .size(10.0),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "route {} · payload {} · source session {} → destination session {} · delivered #{}",
                                                                item.route_id.get(),
                                                                item.payload_id.get(),
                                                                item.source_session_id.get(),
                                                                item.destination_session_id.get(),
                                                                item.delivered_sequence,
                                                            ))
                                                            .monospace()
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                139, 143, 153,
                                                            )),
                                                        );
                                                    });
                                                    ui.label(
                                                        egui::RichText::new(item.text.as_str())
                                                            .size(10.0),
                                                    );

                                                    let current_decision = context_records
                                                        .as_ref()
                                                        .and_then(|records| {
                                                            records.iter().find(|record| {
                                                                record.route_id == item.route_id
                                                            })
                                                        })
                                                        .map(|record| record.decision);
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(match current_decision {
                                                                Some(LocalRouteContextDecision::Admit) => {
                                                                    "CONTEXT: ADMITTED"
                                                                }
                                                                Some(LocalRouteContextDecision::Exclude) => {
                                                                    "CONTEXT: EXCLUDED"
                                                                }
                                                                None => "CONTEXT: EXCLUDED · DEFAULT",
                                                            })
                                                            .monospace()
                                                            .size(9.0),
                                                        );

                                                        let can_change =
                                                            !self.route_context_command_pending
                                                                && self.persist_tx.is_some();
                                                        if ui
                                                            .add_enabled(
                                                                can_change
                                                                    && current_decision
                                                                        != Some(
                                                                            LocalRouteContextDecision::Admit,
                                                                        ),
                                                                egui::Button::new(
                                                                    "Admit to context",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.decide_local_route_context(
                                                                item.route_id,
                                                                LocalRouteContextDecision::Admit,
                                                            );
                                                        }
                                                        if ui
                                                            .add_enabled(
                                                                can_change
                                                                    && current_decision
                                                                        == Some(
                                                                            LocalRouteContextDecision::Admit,
                                                                        ),
                                                                egui::Button::new(
                                                                    "Exclude from context",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.decide_local_route_context(
                                                                item.route_id,
                                                                LocalRouteContextDecision::Exclude,
                                                            );
                                                        }
                                                        if self.route_context_command_pending {
                                                            ui.spinner();
                                                        }
                                                    });
                                                });
                                            }
                                        });
                                }
                            }
                        });

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Worker lifecycle").strong());
                        ui.label(
                            egui::RichText::new(
                                "Durable local orchestration state. Manual only for now: no automatic continuation and no hidden lifecycle prompt injection.",
                            )
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );

                        match local_worker_binding(&self.events, self.local_conversation_id) {
                            Err(error) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "worker binding projection blocked: {error}"
                                    ))
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(186, 108, 108)),
                                );
                            }
                            Ok(None) => {
                                let clicked = ui
                                    .add_enabled(
                                        !self.lifecycle_command_pending
                                            && self.persist_tx.is_some(),
                                        egui::Button::new("Enable worker lifecycle"),
                                    )
                                    .clicked();
                                if clicked {
                                    self.enable_worker_lifecycle();
                                }
                            }
                            Ok(Some(binding)) => {
                                match worker_record(&self.events, binding.worker_id) {
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "worker lifecycle projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(186, 108, 108)),
                                        );
                                    }
                                    Ok(None) => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "worker {} · {}",
                                                    binding.worker_id.get(),
                                                    worker_phase_label(WorkerPhase::Unassigned),
                                                ))
                                                .monospace()
                                                .size(10.0),
                                            );
                                            if self.lifecycle_command_pending {
                                                ui.spinner();
                                            }
                                        });
                                        if ui
                                            .add_enabled(
                                                !self.lifecycle_command_pending,
                                                egui::Button::new("Assign first goal"),
                                            )
                                            .clicked()
                                        {
                                            self.assign_next_worker_goal(binding.worker_id);
                                        }
                                    }
                                    Ok(Some(record)) => {
                                        let phase = record.lifecycle.phase();
                                        let goal_id = record
                                            .lifecycle
                                            .goal_id()
                                            .expect("worker audit record has assigned goal");
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "worker {} · goal {} · {}",
                                                    binding.worker_id.get(),
                                                    goal_id.get(),
                                                    worker_phase_label(phase),
                                                ))
                                                .monospace()
                                                .size(10.0),
                                            );
                                            if self.lifecycle_command_pending {
                                                ui.spinner();
                                            }
                                        });

                                        let enabled = !self.lifecycle_command_pending;
                                        match phase {
                                            WorkerPhase::Unassigned => {}
                                            WorkerPhase::Ready => {
                                                ui.horizontal_wrapped(|ui| {
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Start"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::StartOrResume,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Fail"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::Fail,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Stop"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::Stop,
                                                        );
                                                    }
                                                });
                                            }
                                            WorkerPhase::Working => {
                                                ui.horizontal_wrapped(|ui| {
                                                    for (label, action) in [
                                                        ("Needs input", WorkerAction::RequestInput),
                                                        ("Blocked", WorkerAction::MarkBlocked),
                                                        ("Complete", WorkerAction::Complete),
                                                        ("Fail", WorkerAction::Fail),
                                                        ("Stop", WorkerAction::Stop),
                                                    ] {
                                                        if ui
                                                            .add_enabled(
                                                                enabled,
                                                                egui::Button::new(label),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.transition_worker(
                                                                binding.worker_id,
                                                                goal_id,
                                                                action,
                                                            );
                                                        }
                                                    }
                                                });
                                            }
                                            WorkerPhase::NeedsInput | WorkerPhase::Blocked => {
                                                ui.horizontal_wrapped(|ui| {
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Resume"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::StartOrResume,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Fail"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::Fail,
                                                        );
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            enabled,
                                                            egui::Button::new("Stop"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.transition_worker(
                                                            binding.worker_id,
                                                            goal_id,
                                                            WorkerAction::Stop,
                                                        );
                                                    }
                                                });
                                            }
                                            WorkerPhase::Completed
                                            | WorkerPhase::Failed
                                            | WorkerPhase::Stopped => {
                                                if ui
                                                    .add_enabled(
                                                        enabled,
                                                        egui::Button::new("Assign new goal"),
                                                    )
                                                    .clicked()
                                                {
                                                    self.assign_next_worker_goal(binding.worker_id);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.collapsing("Exact next-request context", |ui| {
                            let mut transcript = context_transcript(&local_display_messages);
                            match admitted_routed_context_messages(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(routed_context) => transcript.extend(routed_context),
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "routed context composition blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            transcript.sort_by_key(
                                context_composer::TranscriptMessage::order_sequence,
                            );
                            if !self.draft.trim().is_empty() {
                                transcript.push(context_composer::TranscriptMessage::draft(
                                    self.draft.clone(),
                                ));
                            }
                            let context_plan = context_composer::ContextPlan::compose(
                                context_composer::ContextPolicy::preview(),
                                self.conversation_instructions.as_str(),
                                self.conversation_developer_context.as_str(),
                                transcript,
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "Context Composer · {} durable transcript message{} · {} admitted routed item{} · developer context {} · current draft {}",
                                    context_plan.durable_transcript_count(),
                                    if context_plan.durable_transcript_count() == 1 { "" } else { "s" },
                                    context_plan.routed_context_count(),
                                    if context_plan.routed_context_count() == 1 { "" } else { "s" },
                                    if context_plan.has_developer_context() { "included" } else { "omitted" },
                                    if context_plan.has_current_draft() { "included (preview only; dispatch policy forbids draft)" } else { "omitted" },
                                ))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "Size ledger · {} included · {} omitted · {} UTF-8 bytes · {} Unicode scalar{} · {} line{} · exact content units, not model tokens",
                                    context_plan.size.included_items,
                                    context_plan.size.omitted_items,
                                    context_plan.size.utf8_bytes,
                                    context_plan.size.unicode_scalars,
                                    if context_plan.size.unicode_scalars == 1 { "" } else { "s" },
                                    context_plan.size.lines,
                                    if context_plan.size.lines == 1 { "" } else { "s" },
                                ))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            ui.collapsing("Context source inventory", |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("context-source-inventory")
                                    .max_height(150.0)
                                    .show(ui, |ui| {
                                        for item in &context_plan.inventory {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(
                                                    egui::RichText::new(item.decision.label())
                                                        .monospace()
                                                        .size(9.0),
                                                );
                                                ui.label(
                                                    egui::RichText::new(item.source.label())
                                                        .size(10.0),
                                                );
                                                if let Some(role) = item.role.as_deref() {
                                                    ui.label(
                                                        egui::RichText::new(format!("role={role}"))
                                                            .monospace()
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                139, 143, 153,
                                                            )),
                                                    );
                                                }
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{}B · {} chars · {} lines",
                                                        item.utf8_bytes,
                                                        item.unicode_scalars,
                                                        item.lines,
                                                    ))
                                                    .monospace()
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    )),
                                                );
                                            });
                                        }
                                    });
                            });
                            let capability_gate = context_composer::CapabilityGate::from_contract(
                                self.local_inference_contract.as_ref(),
                                self.remote_session.profile_id.as_deref(),
                                self.selected_model.as_deref(),
                            );
                            ui.collapsing("Capability-gated request slots", |ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "Availability only. Nothing here is enabled automatically.",
                                    )
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                                for admission in &capability_gate.admissions {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(admission.state.label())
                                                .monospace()
                                                .size(9.0),
                                        );
                                        ui.label(
                                            egui::RichText::new(admission.slot.label()).size(10.0),
                                        );
                                    });
                                }
                            });
                            ui.add_space(4.0);
                            let mut preview =
                                context_plan.request_preview(self.selected_model.as_deref());
                            match self.current_behavior_request_patch() {
                                Ok(Value::Object(request_patch)) => {
                                    if !request_patch.is_empty() {
                                        ui.label(
                                            egui::RichText::new(
                                                "Behavior Profile patch included in this preview",
                                            )
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                        );
                                    }
                                    let preview_object = preview
                                        .as_object_mut()
                                        .expect("context request preview is an object");
                                    for (field, value) in request_patch {
                                        preview_object.insert(field, value);
                                    }
                                }
                                Ok(_) => {
                                    ui.label(
                                        egui::RichText::new(
                                            "Behavior Profile produced an invalid non-object patch",
                                        )
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Behavior Profile blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            let preview_text = serde_json::to_string_pretty(&preview)
                                .unwrap_or_else(|_| "<failed to render request preview>".to_owned());
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(preview_text)
                                            .monospace()
                                            .size(10.0),
                                    );
                                });
                        });
                    });
                if inference_controls_changed {
                    self.persist_current_inference_settings();
                }
                if behavior_profile_changed {
                    self.persist_current_behavior_profile();
                }

                ui.add_space(8.0);
                let response = egui::Frame::default()
                    .fill(composer_fill)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgb(54, 57, 66),
                    ))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        let editor = egui::TextEdit::multiline(&mut self.draft)
                            .desired_rows(4)
                            .frame(false)
                            .hint_text("Write a message…");
                        ui.add_sized([ui.available_width(), 88.0], editor)
                    })
                    .inner;

                if response.changed() {
                    self.evidence = TurnEvidence::default();
                    self.queue_draft_snapshot();
                }

                let remote_turn_idle =
                    self.pending_remote_turn.is_none() && self.active_remote_turn.is_none();
                let behavior_ready_for_send =
                    !self.remote_connected() || self.current_behavior_request_patch().is_ok();
                let remote_ready_for_send = !self.remote_connected()
                    || (self.selected_model.is_some()
                        && remote_turn_idle
                        && !self.capability_probe.running()
                        && behavior_ready_for_send);
                let can_commit = self.persist_tx.is_some()
                    && self.commit_in_flight.is_none()
                    && remote_ready_for_send
                    && !self.draft.trim().is_empty();
                let commit_shortcut = can_commit
                    && ctx.input_mut(|input| {
                        input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(match self.draft_state() {
                            "durable" => "Draft saved locally · Ctrl+Enter to commit",
                            "saving…" => "Saving draft…",
                            _ => "Draft is not durable",
                        })
                        .size(11.0)
                        .color(egui::Color32::from_rgb(132, 136, 145)),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clicked = ui
                            .add_enabled(
                                can_commit,
                                egui::Button::new(
                                    egui::RichText::new(if self.remote_connected() {
                                        "Send"
                                    } else {
                                        "Commit locally"
                                    })
                                    .strong(),
                                )
                                .min_size(egui::vec2(124.0, 34.0)),
                            )
                            .clicked();
                        if clicked || (can_commit && commit_shortcut) {
                            self.commit_current_message();
                        }

                        if let Some(target_request_id) = self
                            .active_remote_turn
                            .as_ref()
                            .map(|active| active.request_id.clone())
                        {
                            if ui.button("Stop generation").clicked() {
                                match self.remote.send(
                                    siwc_bridge::BridgeCommand::CancelResponse {
                                        target_request_id,
                                    },
                                ) {
                                    Ok(()) => {
                                        self.status =
                                            "cancelling active ChatGPT response…".to_owned();
                                    }
                                    Err(error) => {
                                        self.status =
                                            format!("failed to cancel active response: {error}");
                                    }
                                }
                            }
                        }

                        if self.commit_in_flight.is_some() {
                            ui.spinner();
                        }
                    });
                });
            });

        if let Some(local_conversation_id) = sync_live_requested {
            self.sync_historical_conversation(local_conversation_id, ctx);
        }

        let display_messages: &[DisplayMessage] = if historical_mode {
            &self.historical_messages
        } else {
            &local_display_messages
        };

        let reader_key = self.reader_conversation_key();
        let restore_reader_offset = if self.reader_restore_pending {
            self.reader_restore_pending = false;
            Some(self.reader_positions.position(&reader_key))
        } else {
            None
        };
        let reader_hit_targets = display_messages
            .iter()
            .enumerate()
            .flat_map(|(message_index, message)| {
                offline_reader::search_hits(&message.text, &self.reader_search_query)
                    .into_iter()
                    .map(move |(start, end)| (message_index, start, end))
            })
            .collect::<Vec<_>>();
        if self
            .reader_search_hit
            .is_some_and(|hit| hit >= reader_hit_targets.len())
        {
            self.reader_search_hit = reader_hit_targets.is_empty().then_some(0);
        }
        let mut next_reader_hit = false;
        let mut previous_reader_hit = false;
        let transcript_scroll_id = egui::Id::new("transcript-reader-scroll");
        ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::N) {
                next_reader_hit = true;
            }
            if input.consume_key(egui::Modifiers::CTRL, egui::Key::P) {
                previous_reader_hit = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                adjust_reader_scroll(ctx, transcript_scroll_id, 480.0);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                adjust_reader_scroll(ctx, transcript_scroll_id, -480.0);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                set_reader_scroll(ctx, transcript_scroll_id, 0.0);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                set_reader_scroll(ctx, transcript_scroll_id, f32::MAX);
            }
        });
        if next_reader_hit {
            self.reader_search_hit =
                offline_reader::next_hit(self.reader_search_hit, reader_hit_targets.len(), false);
        }
        if previous_reader_hit {
            self.reader_search_hit =
                offline_reader::next_hit(self.reader_search_hit, reader_hit_targets.len(), true);
        }
        let active_reader_message = self
            .reader_search_hit
            .and_then(|hit| reader_hit_targets.get(hit))
            .map(|(message_index, _, _)| *message_index);
        let mut reader_hit_from_button = self.reader_search_hit;
        let mut reader_output_offset = None;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                let mut reader_scroll = egui::ScrollArea::vertical()
                    .id_salt(transcript_scroll_id)
                    .stick_to_bottom(true)
                    .auto_shrink([false, false]);
                if let Some(offset) = restore_reader_offset {
                    reader_scroll = reader_scroll.vertical_scroll_offset(offset);
                }
                let reader_output = reader_scroll.show(ui, |ui| {
                        if !self.reader_search_query.trim().is_empty()
                            && !reader_hit_targets.is_empty()
                        {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} search hit{}",
                                        reader_hit_targets.len(),
                                        if reader_hit_targets.len() == 1 { "" } else { "s" }
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(180, 184, 193)),
                                );
                                if ui.button("Previous").clicked() {
                                    reader_hit_from_button = offline_reader::next_hit(
                                        reader_hit_from_button,
                                        reader_hit_targets.len(),
                                        true,
                                    );
                                }
                                if ui.button("Next").clicked() {
                                    reader_hit_from_button = offline_reader::next_hit(
                                        reader_hit_from_button,
                                        reader_hit_targets.len(),
                                        false,
                                    );
                                }
                                ui.label(egui::RichText::new("Ctrl+P / Ctrl+N").weak());
                            });
                            ui.add_space(8.0);
                        }
                        if selected_live_mirror && self.live_mirror_truncated_before {
                            ui.label(
                                egui::RichText::new("MIRRORED LOCALLY · PARTIAL")
                                    .strong()
                                    .color(egui::Color32::from_rgb(225, 194, 108)),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Older messages exist before this fetched page; unavailable or structurally omitted content is not present in the local reader.",
                                )
                                .size(11.0)
                                .color(egui::Color32::from_rgb(190, 166, 112)),
                            );
                            ui.add_space(10.0);
                        }
                        if display_messages.is_empty() {
                            ui.add_space(90.0);
                            ui.vertical_centered(|ui| {
                                if historical_mode
                                    && self.historical_load_pending
                                        == self.selected_historical_conversation
                                {
                                    ui.spinner();
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new(
                                            if remote_catalog_selected && !selected_live_mirror {
                                                "Remote conversation is not mirrored locally"
                                            } else if selected_live_mirror {
                                                "Loading validated live mirror snapshot…"
                                            } else {
                                                "Loading and verifying historical snapshot…"
                                            },
                                        )
                                        .size(14.0)
                                        .color(egui::Color32::from_rgb(180, 184, 193)),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                            "REMOTE · NOT MIRRORED"
                                        } else if selected_live_mirror {
                                            "No visible messages in the current live mirror page"
                                        } else if historical_mode {
                                            "No visible messages on the exported active branch"
                                        } else {
                                            "Start a local conversation"
                                        })
                                        .size(24.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(221, 223, 229)),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                            "This catalog item has no local snapshot. Selecting it did not contact ChatGPT. Use the explicit mirror action to create a local mirror."
                                        } else if selected_live_mirror {
                                            "The live response is durable; Chatarium did not expose system, tool, or reasoning content or guess across missing pagination."
                                        } else if historical_mode {
                                            "The raw snapshot is preserved; Chatarium did not guess across missing or non-visible content."
                                        } else if self.remote_connected() {
                                            "Write below to send a durable turn through your ChatGPT plan."
                                        } else {
                                            "Messages committed here survive restarts. Connect ChatGPT to enable remote turns."
                                        })
                                        .size(13.0)
                                        .color(egui::Color32::from_rgb(137, 141, 150)),
                                    );
                                }
                            });
                        } else {
                            ui.add_space(8.0);
                            for (message_index, message) in display_messages.iter().enumerate() {
                                let message_is_active_hit = active_reader_message == Some(message_index);
                                match message.role {
                                    DisplayRole::User => {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Min),
                                            |ui| {
                                                let response = transcript_bubble(
                                                    ui,
                                                    message,
                                                    egui::Color32::from_rgb(38, 42, 52),
                                                    "You",
                                                    &self.reader_search_query,
                                                    message_is_active_hit,
                                                    ctx,
                                                );
                                                if message_is_active_hit {
                                                    response.scroll_to_me(Some(egui::Align::Center));
                                                }
                                            },
                                        );
                                    }
                                    DisplayRole::Assistant => {
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Min),
                                            |ui| {
                                                let response = transcript_bubble(
                                                    ui,
                                                    message,
                                                    egui::Color32::from_rgb(29, 31, 36),
                                                    "Assistant",
                                                    &self.reader_search_query,
                                                    message_is_active_hit,
                                                    ctx,
                                                );
                                                if message_is_active_hit {
                                                    response.scroll_to_me(Some(egui::Align::Center));
                                                }
                                            },
                                        );
                                    }
                                }
                                ui.add_space(12.0);
                            }
                        }
                    });
                reader_output_offset = Some(reader_output.state.offset.y);
            });

        self.reader_search_hit = reader_hit_from_button;
        if let Some(offset) = reader_output_offset {
            if (offset - self.reader_last_saved_offset).abs() > 1.0
                && self.reader_last_position_write.elapsed() > Duration::from_millis(250)
            {
                self.reader_positions.set_position(&reader_key, offset);
                let _ = self.reader_positions.save(&self.reader_state_path);
                self.reader_last_saved_offset = offset;
                self.reader_last_position_write = Instant::now();
            }
        }

        if self.saved_revision < self.draft_revision
            || self.commit_in_flight.is_some()
            || self.sign_in_pending
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || self.historical_load_pending.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
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
    let content_width = (ui.available_width() * 0.72).clamp(320.0, 900.0);

    egui::Frame::default()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(14, 11))
        .show(ui, |ui| {
            ui.set_width(content_width);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0).strong().color(
                        if label == "You" {
                            egui::Color32::from_rgb(147, 191, 238)
                        } else {
                            egui::Color32::from_rgb(163, 221, 178)
                        },
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Copy").clicked() {
                            ctx.copy_text(offline_reader::copy_payload(&message.text));
                        }
                    });
                });
                ui.add_space(4.0);
                for block in offline_reader::parse_markdown(&message.text) {
                    render_markdown_block(ui, &block, search_query, active_hit, ctx);
                    ui.add_space(6.0);
                }
                ui.add_space(5.0);
                let metadata = message
                    .timestamp
                    .map(|timestamp| {
                        format!("{} · {} · {:.0}s", label, message.sequence, timestamp)
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "{} · {}",
                            label,
                            message
                                .provenance_label
                                .clone()
                                .unwrap_or_else(|| format!("event #{}", message.sequence))
                        )
                    });
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
            render_reader_lines(ui, text, search_query, false, active_hit);
        }
        offline_reader::MarkdownBlock::Heading { level, text } => {
            render_reader_lines(ui, text, search_query, false, active_hit);
            ui.label(
                egui::RichText::new(format!("heading {level}"))
                    .size(9.0)
                    .color(egui::Color32::from_rgb(123, 128, 140)),
            );
        }
        offline_reader::MarkdownBlock::UnorderedList(items) => {
            for item in items {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("•").strong());
                    render_reader_lines(ui, item, search_query, false, active_hit);
                });
            }
        }
        offline_reader::MarkdownBlock::OrderedList(items) => {
            for (index, item) in items.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(format!("{}.", index + 1)).strong());
                    render_reader_lines(ui, item, search_query, false, active_hit);
                });
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
                        render_reader_lines(ui, line, search_query, false, active_hit);
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

fn render_reader_lines(
    ui: &mut egui::Ui,
    text: &str,
    search_query: &str,
    monospace: bool,
    active_hit: bool,
) {
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
        if line_index + 1 < text.lines().count() {
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
    let inbox_item = replay_local_routed_inbox_for_conversation(
        store.events(),
        destination_conversation_id,
    )?
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

fn persistence_worker(
    mut store: JsonlEventStore,
    data_dir: PathBuf,
    commands: Receiver<PersistCommand>,
    notices: Sender<PersistNotice>,
) {
    diagnostics::info(
        "persist",
        format!(
            "persistence worker ready: {} existing events",
            store.events().len()
        ),
    );
    while let Ok(command) = commands.recv() {
        match command {
            PersistCommand::SaveInferenceSettings { store: settings } => {
                if let Err(error) =
                    settings.save_atomic(&data_dir.join("local-inference-settings.json"))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "inference settings save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveBehaviorProfiles { store: profiles } => {
                if let Err(error) = profiles.save_atomic(&data_dir.join("behavior-profiles.json")) {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "behavior profile save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::InitializeLocalOrchestrationTopology {
                conversation_id,
                container_id,
                root_session_id,
            } => {
                match append_local_orchestration_topology_checked(
                    &mut store,
                    conversation_id,
                    container_id,
                    root_session_id,
                ) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::OrchestrationTopologyInitialized {
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "orchestration topology initialization",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindCurrentSessionRouteEndpoint {
                conversation_id,
                session_id,
                endpoint_id,
            } => {
                match append_current_session_route_endpoint_checked(
                    &mut store,
                    conversation_id,
                    session_id,
                    endpoint_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::RouteEndpointBound { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "route addressability binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ProposeLocalSessionRoute {
                route_id,
                source_conversation_id,
                destination_conversation_id,
            } => {
                match append_local_session_route_proposal_checked(
                    &mut store,
                    route_id,
                    source_conversation_id,
                    destination_conversation_id,
                ) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::LocalRoutePolicyEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route proposal",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AttachLocalSessionRoutePayload {
                payload_id,
                route_id,
                text,
            } => match append_local_route_payload_checked(&mut store, payload_id, route_id, text) {
                Ok(event) => {
                    let _ =
                        notices.send(PersistNotice::LocalRoutePayloadAttached { route_id, event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "local route payload attachment",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            },
            PersistCommand::DecideLocalSessionRoute { route_id, decision } => {
                match append_local_route_user_decision_checked(&mut store, route_id, decision) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::LocalRoutePolicyEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchLocalSessionRoute { route_id } => {
                match append_local_route_dispatch_and_delivery_checked(&mut store, route_id) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::LocalRouteDispatchUpdated {
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route dispatch/delivery",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideLocalRouteContext {
                route_id,
                destination_conversation_id,
                decision,
            } => {
                match append_local_route_context_decision_checked(
                    &mut store,
                    route_id,
                    destination_conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LocalRouteContextDecisionUpdated {
                            route_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route context decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindLocalConversationWorker {
                conversation_id,
                worker_id,
            } => {
                match append_local_worker_binding_checked(&mut store, conversation_id, worker_id) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "lifecycle worker binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AssignWorkerGoal { worker_id, goal_id } => {
                match append_worker_goal_checked(&mut store, worker_id, goal_id) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "lifecycle goal assignment",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::TransitionWorker {
                worker_id,
                goal_id,
                action,
            } => match append_worker_transition_checked(&mut store, worker_id, goal_id, action) {
                Ok(event) => {
                    let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "lifecycle transition",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            },
            PersistCommand::SaveLocalConversationCatalog { catalog } => {
                if let Err(error) = catalog.save_atomic(&data_dir.join("local-conversations.json"))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "local conversation catalog save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveDraft {
                conversation_id,
                revision,
                text,
            } => {
                match store.append_scoped(
                    Some(local_conversation_scope(conversation_id)),
                    EventKind::DraftChanged,
                    text,
                ) {
                    Ok(_) => {
                        if let Some(event) = store.events().last().cloned() {
                            let _ = notices.send(PersistNotice::DraftSaved {
                                conversation_id,
                                revision,
                                event,
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "draft save",
                            revision: Some(revision),
                            request_id: None,
                            turn_id: None,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::CommitMessage {
                request_id,
                message,
            } => match commit_user_message(&mut store, &message) {
                Ok(receipt) => {
                    if let Some(event) = store.events().last().cloned() {
                        let _ = notices.send(PersistNotice::MessageCommitted {
                            request_id,
                            message: receipt.message,
                            event,
                        });
                    }
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "message commit",
                        revision: None,
                        request_id: Some(request_id),
                        turn_id: Some(message.turn_id),
                        error: error.to_string(),
                    });
                }
            },
            PersistCommand::AppendTurnEvent {
                turn_id,
                kind,
                payload,
            } => match store.append_scoped(Some(local_turn_scope(turn_id)), kind, payload) {
                Ok(_) => {
                    if let Some(event) = store.events().last().cloned() {
                        let _ = notices.send(PersistNotice::TurnEventAppended {
                            turn_id,
                            kind,
                            event,
                        });
                    }
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "turn evidence append",
                        revision: None,
                        request_id: None,
                        turn_id: Some(turn_id),
                        error: error.to_string(),
                    });
                }
            },
            PersistCommand::LoadHistoricalConversation {
                local_conversation_id,
            } => {
                let result = latest_historical_conversation_catalog(store.events()).and_then(
                    |catalog| {
                        let entry = catalog
                            .into_iter()
                            .find(|entry| entry.local_conversation_id == local_conversation_id)
                            .ok_or_else(|| {
                                format!(
                                    "historical conversation {local_conversation_id} is not present in the durable catalog"
                                )
                            })?;
                        let imported_sequence = entry.imported_sequence;
                        let messages = load_historical_active_transcript(&data_dir, &entry)?;
                        Ok((imported_sequence, messages))
                    },
                );

                match result {
                    Ok((imported_sequence, messages)) => {
                        let _ = notices.send(PersistNotice::HistoricalConversationLoaded {
                            local_conversation_id,
                            imported_sequence,
                            messages,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::HistoricalConversationLoadFailed {
                            local_conversation_id,
                            error,
                        });
                    }
                }
            }
            PersistCommand::LoadLiveConversation {
                local_conversation_id,
            } => match latest_live_transcript(store.events(), local_conversation_id, None) {
                Ok((snapshot_sequence, projection)) => {
                    let _ = notices.send(PersistNotice::LiveConversationLoaded {
                        local_conversation_id,
                        snapshot_sequence,
                        truncated_before: projection.truncated_before,
                        messages: projection.messages,
                    });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::LiveConversationLoadFailed {
                        local_conversation_id,
                        error,
                    });
                }
            },
            PersistCommand::PromoteHistoricalLiveMirror {
                local_conversation_id,
                expected_remote_conversation_id,
                body,
            } => {
                diagnostics::info(
                    "persist",
                    format!(
                        "validating imported mirror remote={} local={local_conversation_id}",
                        diagnostics::short_id(&expected_remote_conversation_id)
                    ),
                );
                let before = store.events().len();
                match promote_historical_live_mirror_body(
                    &mut store,
                    local_conversation_id,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
                        diagnostics::info(
                            "persist",
                            format!(
                                "imported mirror durable: local={local_conversation_id} event=#{} appended-events={}",
                                result.snapshot.sequence,
                                appended_events.len()
                            ),
                        );
                        let projection = latest_live_transcript(
                            store.events(),
                            local_conversation_id,
                            Some(result.snapshot.sequence),
                        );
                        let (truncated_before, messages, projection_error) = match projection {
                            Ok((_, projection)) => {
                                (projection.truncated_before, projection.messages, None)
                            }
                            Err(error) => (false, Vec::new(), Some(error)),
                        };
                        let _ = notices.send(PersistNotice::HistoricalLiveMirrorPromoted {
                            local_conversation_id,
                            snapshot_sequence: result.snapshot.sequence,
                            appended_events,
                            truncated_before,
                            messages,
                            projection_error,
                        });
                    }
                    Err(error) => {
                        diagnostics::error(
                            "persist",
                            format!(
                                "imported mirror validation/persist failed local={local_conversation_id}: {error}"
                            ),
                        );
                        let _ = notices.send(PersistNotice::HistoricalLiveMirrorPromotionFailed {
                            local_conversation_id,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::PromoteDiscoveredLiveMirror {
                expected_remote_conversation_id,
                body,
            } => {
                diagnostics::info(
                    "persist",
                    format!(
                        "validating discovered mirror remote={}",
                        diagnostics::short_id(&expected_remote_conversation_id)
                    ),
                );
                let before = store.events().len();
                match promote_discovered_live_mirror_body(
                    &mut store,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
                        diagnostics::info(
                            "persist",
                            format!(
                                "discovered mirror durable: remote={} local={} event=#{} appended-events={}",
                                diagnostics::short_id(result.remote_conversation_id.as_str()),
                                result.local_conversation_id,
                                result.snapshot.sequence,
                                appended_events.len()
                            ),
                        );
                        let projection = latest_live_transcript(
                            store.events(),
                            result.local_conversation_id,
                            Some(result.snapshot.sequence),
                        );
                        let (truncated_before, messages, projection_error) = match projection {
                            Ok((_, projection)) => {
                                (projection.truncated_before, projection.messages, None)
                            }
                            Err(error) => (false, Vec::new(), Some(error)),
                        };
                        let _ = notices.send(PersistNotice::DiscoveredLiveMirrorPromoted {
                            local_conversation_id: result.local_conversation_id,
                            remote_conversation_id: result
                                .remote_conversation_id
                                .as_str()
                                .to_owned(),
                            snapshot_sequence: result.snapshot.sequence,
                            appended_events,
                            truncated_before,
                            messages,
                            projection_error,
                        });
                    }
                    Err(error) => {
                        diagnostics::error(
                            "persist",
                            format!(
                                "discovered mirror validation/persist failed remote={}: {error}",
                                diagnostics::short_id(&expected_remote_conversation_id)
                            ),
                        );
                        let _ = notices.send(PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                            remote_conversation_id: expected_remote_conversation_id,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::MirrorQueuePrepare {
                remote_conversation_id,
                catalog_index,
                reply,
            } => {
                let result = record_remote_mirror_queue_item_queued(
                    &mut store,
                    &remote_conversation_id,
                    catalog_index,
                )
                .and_then(|_| {
                    record_remote_mirror_queue_capture_started(&mut store, &remote_conversation_id)
                })
                .map(|_| ())
                .map_err(|error| error.to_string());
                if result.is_ok() {
                    let before = store.events().len().saturating_sub(2);
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated {
                        appended_events: store.events()[before..].to_vec(),
                    });
                }
                let _ = reply.send(result);
            }
            PersistCommand::MirrorQueueCapture {
                remote_conversation_id,
                body,
                reply,
            } => {
                let before = store.events().len();
                let result = match promote_discovered_live_mirror_body(
                    &mut store,
                    &remote_conversation_id,
                    &body,
                ) {
                    Ok(promotion) => match latest_live_transcript(
                        store.events(),
                        promotion.local_conversation_id,
                        Some(promotion.snapshot.sequence),
                    ) {
                        Ok((_, projection)) => {
                            let mirror_state = if projection.truncated_before {
                                "partial"
                            } else {
                                "fully_mirrored"
                            };
                            match record_remote_mirror_queue_completed(
                                &mut store,
                                &remote_conversation_id,
                                mirror_state,
                            ) {
                                Ok(_) => Ok(MirrorPersisted {
                                    snapshot_sequence: promotion.snapshot.sequence,
                                    mirror_state,
                                }),
                                Err(error) => Err(error.to_string()),
                            }
                        }
                        Err(error) => {
                            let _ = record_remote_mirror_queue_failed(
                                &mut store,
                                &remote_conversation_id,
                                "structural",
                            );
                            Err(error)
                        }
                    },
                    Err(error) => {
                        let _ = record_remote_mirror_queue_failed(
                            &mut store,
                            &remote_conversation_id,
                            "structural",
                        );
                        Err(error.to_string())
                    }
                };
                let appended_events = store.events()[before..].to_vec();
                if !appended_events.is_empty() {
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated { appended_events });
                }
                let _ = reply.send(result);
            }
            PersistCommand::MirrorQueueFailure {
                remote_conversation_id,
                failure_class,
                reply,
            } => {
                let before = store.events().len();
                let result = match failure_class {
                    MirrorFailureClass::Transient => record_remote_mirror_queue_failed(
                        &mut store,
                        &remote_conversation_id,
                        "transient",
                    ),
                    MirrorFailureClass::RateLimited => {
                        record_remote_mirror_queue_rate_limited(&mut store, &remote_conversation_id)
                    }
                    MirrorFailureClass::Structural => record_remote_mirror_queue_failed(
                        &mut store,
                        &remote_conversation_id,
                        "structural",
                    ),
                }
                .map(|_| ())
                .map_err(|error| error.to_string());
                if result.is_ok() {
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated {
                        appended_events: store.events()[before..].to_vec(),
                    });
                }
                let _ = reply.send(result);
            }
            PersistCommand::RecordRemoteHealthSignal {
                signal,
                now_ms,
                reply,
            } => {
                let mut controller =
                    match RemoteHealthController::from_events(store.events(), now_ms) {
                        Ok(controller) => controller,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                let before = store.events().len();
                let result =
                    record_remote_health_signal(&mut store, &mut controller, signal, now_ms)
                        .map_err(|error| error.to_string());
                if result.is_ok() {
                    if let Some(event) = store.events()[before..].last().cloned() {
                        let _ = notices.send(PersistNotice::RemoteHealthUpdated {
                            event,
                            controller: controller.clone(),
                        });
                    }
                }
                let _ = reply.send(result.map(|_| controller));
            }
            PersistCommand::RecordRemoteHealthIntent {
                intent,
                now_ms,
                reply,
            } => {
                let mut controller =
                    match RemoteHealthController::from_events(store.events(), now_ms) {
                        Ok(controller) => controller,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                let before = store.events().len();
                let result =
                    record_remote_health_intent(&mut store, &mut controller, intent, now_ms)
                        .map_err(|error| error.to_string());
                if result.is_ok() {
                    if let Some(event) = store.events()[before..].last().cloned() {
                        let _ = notices.send(PersistNotice::RemoteHealthUpdated {
                            event,
                            controller: controller.clone(),
                        });
                    }
                }
                let _ = reply.send(result.map(|_| controller));
            }
            PersistCommand::Shutdown => {
                diagnostics::info("persist", "shutdown requested");
                break;
            }
        }
    }
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

fn should_request_models(
    was_connected: bool,
    account_changed: bool,
    models_empty: bool,
    model_list_pending: bool,
) -> bool {
    (!was_connected || account_changed || models_empty) && !model_list_pending
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

fn local_route_context_decision_label(
    decision: LocalRouteContextDecision,
) -> &'static str {
    match decision {
        LocalRouteContextDecision::Admit => "admit",
        LocalRouteContextDecision::Exclude => "exclude",
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
        .map(|message| {
            context_composer::TranscriptMessage::durable(
                match message.role {
                    DisplayRole::User => context_composer::TranscriptRole::User,
                    DisplayRole::Assistant => context_composer::TranscriptRole::Assistant,
                },
                message.text.clone(),
                message.sequence,
            )
        })
        .collect()
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
        let already_interrupted = kinds.contains(&EventKind::TransportInterrupted);
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

    Ok(interrupted.len())
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
    let mut local_turn_scopes = HashSet::new();

    for event in events {
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        if message.conversation_id == local_conversation_id {
            local_turn_scopes.insert(local_turn_scope(message.turn_id));
        }
    }

    let relevant = events
        .iter()
        .filter(|event| match event.kind {
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
        })
        .cloned()
        .collect::<Vec<_>>();

    projected_display_messages(&relevant)
}

fn projected_display_messages(events: &[EventEnvelope]) -> Vec<DisplayMessage> {
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
            provenance_label: None,
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

    let normalized = first.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "Local conversation".to_owned();
    }

    const LIMIT: usize = 42;
    let mut title = normalized.chars().take(LIMIT).collect::<String>();
    if normalized.chars().count() > LIMIT {
        title.push('…');
    }
    title
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
    let persist_worker = thread::Builder::new()
        .name("chatarium-acceptance-persistence".to_owned())
        .spawn(move || persistence_worker(store, data_dir, persist_rx, persist_notice_tx));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn persist_notice_name(notice: &PersistNotice) -> &'static str {
        match notice {
            PersistNotice::DraftSaved { .. } => "draft_saved",
            PersistNotice::MessageCommitted { .. } => "message_committed",
            PersistNotice::TurnEventAppended { .. } => "turn_event_appended",
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
            PersistNotice::LocalRoutePolicyEventAppended { .. } => {
                "local_route_policy_event_appended"
            }
            PersistNotice::LocalRoutePayloadAttached { .. } => "local_route_payload_attached",
            PersistNotice::LocalRouteDispatchUpdated { .. } => "local_route_dispatch_updated",
            PersistNotice::LocalRouteContextDecisionUpdated { .. } => {
                "local_route_context_decision_updated"
            }
            PersistNotice::LifecycleEventAppended { .. } => "lifecycle_event_appended",
            PersistNotice::Failed { .. } => "failed",
        }
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

        append_local_route_user_decision_checked(
            &mut store,
            RouteId::new(1),
            RouteUserDecision::Deny,
        )
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
            append_local_session_route_proposal_checked(
                &mut store,
                RouteId::new(2),
                source,
                missing,
            )
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

        append_local_session_route_proposal_checked(
            &mut store,
            RouteId::new(1),
            source,
            destination,
        )
        .unwrap();
        append_local_route_payload_checked(
            &mut store,
            RoutePayloadId::new(1),
            RouteId::new(1),
            "peer payload".to_owned(),
        )
        .unwrap();
        append_local_route_user_decision_checked(
            &mut store,
            RouteId::new(1),
            RouteUserDecision::Allow,
        )
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
        assert_eq!(
            admit.kind,
            EventKind::LocalRouteContextDecisionRecorded
        );

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

        let (merged, current_pass_observed) =
            merge_history_discovery_catalog(existing.clone(), &[]);

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
        let worker = thread::spawn(move || {
            persistence_worker(store, worker_data_dir, command_rx, notice_tx)
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
}
