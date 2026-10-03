mod account_bridge;
mod siwc_bridge;

use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId,
    LocalTurnId, RemoteEvidence, TurnEvidence,
};
use chatarium_protocol::conversation_list::{ConversationListItem, ConversationListPage};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, decode_user_message_commit, local_turn_scope,
};
use chatarium_store::historical_transcript::{
    HistoricalConversationCatalogEntry, HistoricalTranscriptMessage, HistoricalTranscriptRole,
    latest_historical_conversation_catalog, load_historical_active_transcript,
};
use chatarium_store::remote_mirror_bootstrap::{
    promote_discovered_live_mirror_body, promote_historical_live_mirror_body,
};
use chatarium_store::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use chatarium_store::remote_mirror_transcript::{
    RemoteTranscriptMessage, RemoteTranscriptProjection, RemoteTranscriptRole,
    project_remote_active_transcript,
};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

enum PersistCommand {
    SaveDraft {
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
    Shutdown,
}

enum PersistNotice {
    DraftSaved {
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
    Failed {
        operation: &'static str,
        revision: Option<u64>,
        request_id: Option<u64>,
        turn_id: Option<LocalTurnId>,
        error: String,
    },
}

enum LiveMirrorFetchNotice {
    HistoryAuthenticated,
    HistoryUnauthenticated,
    HistoryAuthenticationUnknown,
    HistoryProbeFailed {
        error: account_bridge::BrowserBridgeError,
    },
    HistoryListLoaded {
        page: ConversationListPage,
    },
    HistoryListFailed {
        error: String,
    },
    Fetched {
        local_conversation_id: LocalConversationId,
        remote_conversation_id: String,
        body: Value,
    },
    Failed {
        local_conversation_id: LocalConversationId,
        error: String,
    },
    DiscoveredFetched {
        remote_conversation_id: String,
        body: Value,
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
    provenance_label: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingRemoteTurn {
    turn_id: LocalTurnId,
    request_id: String,
    model: String,
    input: Value,
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
}

struct ChatariumApp {
    draft: String,
    draft_revision: u64,
    saved_revision: u64,
    next_commit_request: u64,
    commit_in_flight: Option<u64>,
    evidence: TurnEvidence,
    local_conversation_id: LocalConversationId,
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
    live_mirror_truncated_before: bool,
    remote_conversation_catalog: Vec<ConversationListItem>,
    remote_conversation_total: Option<u64>,
    history_discovery_started: bool,
    history_list_pending: bool,
    history_bridge_authenticated: bool,
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
    remote: siwc_bridge::BridgeRuntime,
    remote_session: siwc_bridge::SessionState,
    remote_models: Vec<siwc_bridge::Model>,
    model_list_pending: bool,
    selected_model: Option<String>,
    remote_status: String,
    remote_runtime_ready: bool,
    remote_runtime_failed: bool,
    sign_in_requested: bool,
    sign_in_pending: bool,
    pending_remote_turn: Option<PendingRemoteTurn>,
    active_remote_turn: Option<ActiveRemoteTurn>,
    commit_remote_intents: BTreeMap<u64, String>,
}

impl ChatariumApp {
    fn new(repaint: &egui::Context) -> Self {
        let journal_path = default_journal_path();
        let data_dir = journal_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        match JsonlEventStore::open(&journal_path) {
            Ok(mut store) => {
                let recovery = recover_interrupted_remote_turns(&mut store);
                let events = store.events().to_vec();
                let draft = projected_working_draft(&events);
                let (local_conversation_id, mut startup_status) =
                    match projected_local_conversation_id(&events) {
                        Ok(Some(id)) => (id, "journal ready".to_owned()),
                        Ok(None) => (LocalConversationId::new(), "journal ready".to_owned()),
                        Err(error) => (
                            LocalConversationId::new(),
                            format!("journal ready; local identity replay warning: {error}"),
                        ),
                    };
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
                let live_mirrored_conversations = live_mirror_catalog
                    .iter()
                    .map(|entry| entry.local_conversation_id)
                    .collect();
                let (account_bridge, account_bridge_provider, account_bridge_status) =
                    start_account_bridge();
                let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
                let (persist_tx, persist_rx) = mpsc::channel();
                let (notice_tx, notice_rx) = mpsc::channel();
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
                        events,
                        historical_catalog,
                        selected_historical_conversation: None,
                        loaded_historical_conversation: None,
                        historical_messages: Vec::new(),
                        historical_load_pending: None,
                        live_mirrored_conversations,
                        live_mirror_catalog,
                        live_mirror_pending: None,
                        remote_discovery_pending: None,
                        live_mirror_truncated_before: false,
                        remote_conversation_catalog: Vec::new(),
                        remote_conversation_total: None,
                        history_discovery_started: false,
                        history_list_pending: false,
                        history_bridge_authenticated: false,
                        journal_path,
                        persist_tx: Some(persist_tx),
                        notice_rx: Some(notice_rx),
                        worker: Some(worker),
                        status: startup_status,
                        account_bridge,
                        account_bridge_provider,
                        account_bridge_status,
                        live_mirror_fetch_tx,
                        live_mirror_fetch_rx,
                        remote: siwc_bridge::BridgeRuntime::start(repaint),
                        remote_session: siwc_bridge::SessionState::default(),
                        remote_models: Vec::new(),
                        model_list_pending: false,
                        selected_model: None,
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

        Self {
            draft,
            draft_revision: 0,
            saved_revision: 0,
            next_commit_request: 1,
            commit_in_flight: None,
            evidence: TurnEvidence::default(),
            local_conversation_id: projected_local_conversation_id(&events)
                .ok()
                .flatten()
                .unwrap_or_default(),
            historical_catalog: latest_historical_conversation_catalog(&events).unwrap_or_default(),
            events,
            selected_historical_conversation: None,
            loaded_historical_conversation: None,
            historical_messages: Vec::new(),
            historical_load_pending: None,
            live_mirrored_conversations,
            live_mirror_catalog,
            live_mirror_pending: None,
            remote_discovery_pending: None,
            live_mirror_truncated_before: false,
            remote_conversation_catalog: Vec::new(),
            remote_conversation_total: None,
            history_discovery_started: false,
            history_list_pending: false,
            history_bridge_authenticated: false,
            journal_path,
            persist_tx: None,
            notice_rx: None,
            worker: None,
            status,
            account_bridge,
            account_bridge_provider,
            account_bridge_status,
            live_mirror_fetch_tx,
            live_mirror_fetch_rx,
            remote: siwc_bridge::BridgeRuntime::start(repaint),
            remote_session: siwc_bridge::SessionState::default(),
            remote_models: Vec::new(),
            model_list_pending: false,
            selected_model: None,
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
        self.historical_load_pending = None;
        self.live_mirror_truncated_before = false;
        self.status = "local conversation selected".to_owned();
    }

    fn select_historical_conversation(&mut self, local_conversation_id: LocalConversationId) {
        if self.selected_historical_conversation == Some(local_conversation_id)
            && self.loaded_historical_conversation == Some(local_conversation_id)
        {
            return;
        }

        self.selected_historical_conversation = Some(local_conversation_id);
        self.loaded_historical_conversation = None;
        self.historical_messages.clear();
        self.historical_load_pending = Some(local_conversation_id);

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

    fn start_history_discovery(&mut self, repaint: &egui::Context) {
        if self.history_list_pending {
            return;
        }
        self.history_discovery_started = true;

        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.account_bridge_status = "listener unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        self.history_list_pending = true;
        self.account_bridge_status = "listener ready · checking browser…".to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-history-discovery".to_owned())
            .spawn(move || {
                use chatarium_core::authenticated_session::SessionAuthenticationEvidence;

                match provider.probe_authentication() {
                    Ok(SessionAuthenticationEvidence::Authenticated) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryAuthenticated);
                    }
                    Ok(SessionAuthenticationEvidence::Unauthenticated) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryUnauthenticated);
                        repaint.request_repaint();
                        return;
                    }
                    Ok(SessionAuthenticationEvidence::Unknown) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryAuthenticationUnknown);
                        repaint.request_repaint();
                        return;
                    }
                    Err(error) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryProbeFailed { error });
                        repaint.request_repaint();
                        return;
                    }
                }

                match provider.list_recent_conversations() {
                    Ok(page) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryListLoaded { page });
                    }
                    Err(error) => {
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryListFailed {
                            error: error.to_string(),
                        });
                    }
                }
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
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
        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.status = "cannot open remote chat: history bridge unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        self.remote_discovery_pending = Some(remote_conversation_id.clone());
        self.status = "fetching exact remote ChatGPT conversation…".to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-remote-history-open".to_owned())
            .spawn(move || {
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(body) => LiveMirrorFetchNotice::DiscoveredFetched {
                        remote_conversation_id,
                        body,
                    },
                    Err(error) => LiveMirrorFetchNotice::DiscoveredFailed {
                        remote_conversation_id,
                        error: error.to_string(),
                    },
                };
                let _ = notices.send(notice);
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
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
        self.status = "fetching exact ChatGPT conversation through browser session…".to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-live-mirror-fetch".to_owned())
            .spawn(move || {
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(body) => LiveMirrorFetchNotice::Fetched {
                        local_conversation_id,
                        remote_conversation_id,
                        body,
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
                LiveMirrorFetchNotice::HistoryAuthenticated => {
                    self.history_bridge_authenticated = true;
                    self.account_bridge_status = "browser connected · authenticated".to_owned();
                }
                LiveMirrorFetchNotice::HistoryUnauthenticated => {
                    self.history_list_pending = false;
                    self.history_bridge_authenticated = false;
                    self.account_bridge_status = "browser connected · not signed in".to_owned();
                }
                LiveMirrorFetchNotice::HistoryAuthenticationUnknown => {
                    self.history_list_pending = false;
                    self.history_bridge_authenticated = false;
                    self.account_bridge_status =
                        "browser connected · authentication unknown".to_owned();
                }
                LiveMirrorFetchNotice::HistoryProbeFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_authenticated = false;
                    self.account_bridge_status = history_probe_failure_status(&error);
                }
                LiveMirrorFetchNotice::HistoryListLoaded { page } => {
                    self.history_list_pending = false;
                    self.history_bridge_authenticated = true;
                    self.remote_conversation_total = Some(page.total);
                    self.remote_conversation_catalog = page.items;
                    self.account_bridge_status = format!(
                        "browser authenticated · {} recent chat{}",
                        self.remote_conversation_catalog.len(),
                        if self.remote_conversation_catalog.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    );
                }
                LiveMirrorFetchNotice::HistoryListFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_authenticated = true;
                    self.account_bridge_status =
                        format!("browser authenticated · history unavailable: {error}");
                }
                LiveMirrorFetchNotice::DiscoveredFetched {
                    remote_conversation_id,
                    body,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        self.remote_discovery_pending = None;
                        self.status = "remote conversation fetched, but persistence is unavailable"
                            .to_owned();
                        continue;
                    };
                    if let Err(error) = sender.send(PersistCommand::PromoteDiscoveredLiveMirror {
                        expected_remote_conversation_id: remote_conversation_id,
                        body,
                    }) {
                        self.remote_discovery_pending = None;
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
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.status = format!("remote ChatGPT conversation fetch failed: {error}");
                }
                LiveMirrorFetchNotice::Fetched {
                    local_conversation_id,
                    remote_conversation_id,
                    body,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        self.live_mirror_pending = None;
                        self.status =
                            "live conversation fetched, but persistence is unavailable".to_owned();
                        continue;
                    };
                    if let Err(error) = sender.send(PersistCommand::PromoteHistoricalLiveMirror {
                        local_conversation_id,
                        expected_remote_conversation_id: remote_conversation_id,
                        body,
                    }) {
                        self.live_mirror_pending = None;
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
                self.commit_remote_intents.insert(request_id, model);
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
                PersistNotice::DraftSaved { revision, event } => {
                    self.saved_revision = self.saved_revision.max(revision);
                    self.events.push(event);
                    if self.saved_revision == self.draft_revision
                        && self.commit_in_flight.is_none()
                        && self.active_remote_turn.is_none()
                    {
                        self.status = "draft durable".to_owned();
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

                        if let Some(model) = self.commit_remote_intents.remove(&request_id) {
                            let remote_request_id = message.turn_id.to_string();
                            let input = responses_input(&projected_local_display_messages(
                                &self.events,
                                self.local_conversation_id,
                            ));
                            self.pending_remote_turn = Some(PendingRemoteTurn {
                                turn_id: message.turn_id,
                                request_id: remote_request_id.clone(),
                                model: model.clone(),
                                input,
                            });
                            let payload = remote_turn_payload(
                                message.turn_id,
                                &remote_request_id,
                                Some(&model),
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
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.selected_historical_conversation = Some(local_conversation_id);
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.historical_load_pending = None;
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
                            "remote ChatGPT conversation mirrored locally at event #{snapshot_sequence}"
                        );
                    }
                }
                PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                    remote_conversation_id,
                    error,
                } => {
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.status = format!("remote mirror creation failed: {error}");
                }
                PersistNotice::Failed {
                    operation,
                    revision,
                    request_id,
                    turn_id,
                    error,
                } => {
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
                            self.selected_model = None;
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
                        self.selected_model = None;
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
                    }
                    self.remote_models = models;
                    self.remote_status = if self.remote_models.is_empty() {
                        "connected; no models reported".to_owned()
                    } else {
                        format!("connected · {} models", self.remote_models.len())
                    };
                }
                siwc_bridge::BridgeEvent::Delta { request_id, delta } => {
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
}

impl eframe::App for ChatariumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_notices();
        self.process_live_mirror_fetch_notices();
        self.process_remote_notices();
        if !self.history_discovery_started {
            self.start_history_discovery(ctx);
        }

        let local_display_messages =
            projected_local_display_messages(&self.events, self.local_conversation_id);
        let local_conversation_title = derived_conversation_title(&local_display_messages);
        let historical_mode = self.selected_historical_conversation.is_some();
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
        let conversation_title = selected_live_entry
            .map(|entry| entry.title.clone())
            .or_else(|| selected_historical_entry.and_then(|entry| entry.title.clone()))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| {
                if historical_mode {
                    "Imported ChatGPT conversation".to_owned()
                } else {
                    local_conversation_title.clone()
                }
            });
        let mut select_local_requested = false;
        let mut select_historical_requested = None;
        let mut sync_live_requested = None;
        let mut open_remote_requested = None;
        let mut refresh_history_requested = false;

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

                        ui.add_space(26.0);
                        ui.label(
                            egui::RichText::new("CONVERSATIONS")
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                        );
                        ui.add_space(6.0);

                        egui::Frame::default()
                            .fill(if historical_mode {
                                egui::Color32::from_rgb(26, 28, 33)
                            } else {
                                egui::Color32::from_rgb(31, 33, 39)
                            })
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(10, 9))
                            .show(ui, |ui| {
                                if ui
                                    .selectable_label(
                                        !historical_mode,
                                        egui::RichText::new(local_conversation_title.as_str())
                                            .strong(),
                                    )
                                    .clicked()
                                {
                                    select_local_requested = true;
                                }
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} local message{}",
                                        local_display_messages.len(),
                                        if local_display_messages.len() == 1 {
                                            ""
                                        } else {
                                            "s"
                                        }
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                            });

                        if !self.remote_conversation_catalog.is_empty() {
                            ui.add_space(16.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "CHATGPT HISTORY · {}/{}",
                                    self.remote_conversation_catalog.len(),
                                    self.remote_conversation_total
                                        .unwrap_or(self.remote_conversation_catalog.len() as u64)
                                ))
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 176, 137)),
                            );
                            ui.add_space(6.0);
                            for entry in &self.remote_conversation_catalog {
                                let live_local = self
                                    .live_mirror_catalog
                                    .iter()
                                    .find(|live| live.remote_conversation_id == entry.id)
                                    .map(|live| live.local_conversation_id);
                                let imported_local = self
                                    .historical_catalog
                                    .iter()
                                    .find(|historical| {
                                        historical.remote_conversation_id == entry.id
                                    })
                                    .map(|historical| historical.local_conversation_id);
                                let local = live_local.or(imported_local);
                                let selected = local.is_some()
                                    && self.selected_historical_conversation == local;
                                let title = entry
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
                                    if let Some(local) = local {
                                        select_historical_requested = Some(local);
                                    } else {
                                        open_remote_requested = Some(entry.id.clone());
                                    }
                                }
                                ui.label(
                                    egui::RichText::new(if live_local.is_some() {
                                        "remote · mirrored locally"
                                    } else if imported_local.is_some() {
                                        "remote · historical backup available"
                                    } else {
                                        "remote · click to mirror"
                                    })
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(112, 116, 126)),
                                );
                                ui.add_space(4.0);
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
                            "History bridge",
                            self.account_bridge_status.as_str(),
                            self.history_bridge_authenticated,
                        );
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

        if select_local_requested {
            self.select_local_conversation();
        } else if let Some(local_conversation_id) = select_historical_requested {
            self.select_historical_conversation(local_conversation_id);
        } else if let Some(remote_conversation_id) = open_remote_requested {
            self.open_discovered_remote_conversation(remote_conversation_id, ctx);
        }
        if refresh_history_requested {
            self.start_history_discovery(ctx);
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
                            egui::RichText::new(if selected_live_mirror {
                                "Validated live ChatGPT mirror · read-only"
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
                                    egui::RichText::new(if selected_live_mirror {
                                        "LIVE MIRROR"
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
                                        if selected_live_mirror {
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
                                egui::RichText::new(if selected_live_mirror {
                                    "Live mirror is read-only for now"
                                } else {
                                    "Historical snapshot is read-only"
                                })
                                .strong()
                                .color(egui::Color32::from_rgb(221, 223, 229)),
                            );
                            ui.label(
                                egui::RichText::new(if selected_live_mirror {
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
                                let can_sync = selected_historical_id.is_some()
                                    && self.persist_tx.is_some()
                                    && self.account_bridge_provider.is_some()
                                    && self.live_mirror_pending.is_none();
                                let label = if pending {
                                    "Syncing…"
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
                                    sync_live_requested = selected_historical_id;
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
                let remote_ready_for_send =
                    !self.remote_connected() || (self.selected_model.is_some() && remote_turn_idle);
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

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
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
                                            if selected_live_mirror {
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
                                        egui::RichText::new(if selected_live_mirror {
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
                                        egui::RichText::new(if selected_live_mirror {
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
                            if selected_live_mirror && self.live_mirror_truncated_before {
                                ui.label(
                                    egui::RichText::new(
                                        "Older messages exist before this fetched page; Chatarium is not guessing across the pagination boundary.",
                                    )
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(190, 166, 112)),
                                );
                                ui.add_space(10.0);
                            }
                            for message in display_messages {
                                match message.role {
                                    DisplayRole::User => {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Min),
                                            |ui| {
                                                transcript_bubble(
                                                    ui,
                                                    &message,
                                                    egui::Color32::from_rgb(38, 42, 52),
                                                    "You",
                                                );
                                            },
                                        );
                                    }
                                    DisplayRole::Assistant => {
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Min),
                                            |ui| {
                                                transcript_bubble(
                                                    ui,
                                                    &message,
                                                    egui::Color32::from_rgb(29, 31, 36),
                                                    "Assistant",
                                                );
                                            },
                                        );
                                    }
                                }
                                ui.add_space(12.0);
                            }
                        }
                    });
            });

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
) {
    egui::Frame::default()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(14, 11))
        .show(ui, |ui| {
            ui.set_max_width(660.0);
            ui.label(
                egui::RichText::new(label)
                    .size(10.0)
                    .strong()
                    .color(egui::Color32::from_rgb(133, 138, 149)),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(message.text.as_str())
                    .size(14.0)
                    .color(egui::Color32::from_rgb(232, 234, 239)),
            );
            ui.add_space(5.0);
            ui.label(
                egui::RichText::new(
                    message
                        .provenance_label
                        .clone()
                        .unwrap_or_else(|| format!("event #{}", message.sequence)),
                )
                .size(10.0)
                .color(egui::Color32::from_rgb(116, 121, 133)),
            );
        });
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

fn history_probe_failure_status(error: &account_bridge::BrowserBridgeError) -> String {
    match error {
        account_bridge::BrowserBridgeError::Timeout => {
            "userscript did not reach the loopback listener. Edge 153 + Tampermonkey 5.5.0 has a known GM networking stall; bridge v0.3 also tries direct page loopback, which may require allowing ChatGPT local network access in Edge."
                .to_owned()
        }
        _ => format!("listener ready · browser not confirmed: {error}"),
    }
}

impl Drop for ChatariumApp {
    fn drop(&mut self) {
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

fn persistence_worker(
    mut store: JsonlEventStore,
    data_dir: PathBuf,
    commands: Receiver<PersistCommand>,
    notices: Sender<PersistNotice>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            PersistCommand::SaveDraft { revision, text } => {
                match store.append(EventKind::DraftChanged, text) {
                    Ok(_) => {
                        if let Some(event) = store.events().last().cloned() {
                            let _ = notices.send(PersistNotice::DraftSaved { revision, event });
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
                let before = store.events().len();
                match promote_historical_live_mirror_body(
                    &mut store,
                    local_conversation_id,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
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
                let before = store.events().len();
                match promote_discovered_live_mirror_body(
                    &mut store,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
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
                        let _ = notices.send(PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                            remote_conversation_id: expected_remote_conversation_id,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::Shutdown => break,
        }
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
                "listener ready · waiting for browser".to_owned(),
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
        let entry = LiveMirrorCatalogEntry {
            local_conversation_id: record.local_conversation_id,
            remote_conversation_id: record.remote_conversation_id.as_str().to_owned(),
            title: if record.envelope.title.trim().is_empty() {
                "Untitled ChatGPT conversation".to_owned()
            } else {
                record.envelope.title.clone()
            },
            snapshot_sequence: record.imported_sequence,
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

fn responses_input(messages: &[DisplayMessage]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(|message| {
                serde_json::json!({
                    "role": match message.role {
                        DisplayRole::User => "user",
                        DisplayRole::Assistant => "assistant",
                    },
                    "content": message.text,
                })
            })
            .collect(),
    )
}

fn projected_working_draft(events: &[EventEnvelope]) -> String {
    let latest_draft = events
        .iter()
        .rev()
        .find(|event| event.scope.is_none() && event.kind == EventKind::DraftChanged);
    let latest_commit_sequence = events
        .iter()
        .rev()
        .find(|event| event.kind == EventKind::UserMessageCommitted)
        .map(|event| event.sequence)
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Chatarium",
        options,
        Box::new(|creation_context| {
            configure_ui(&creation_context.egui_ctx);
            Ok(Box::new(ChatariumApp::new(&creation_context.egui_ctx)))
        }),
    )
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
            PersistNotice::Failed { .. } => "failed",
        }
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
    fn history_probe_timeout_explains_dual_loopback_recovery() {
        let status = history_probe_failure_status(&account_bridge::BrowserBridgeError::Timeout);
        assert!(status.contains("Tampermonkey 5.5.0"));
        assert!(status.contains("Edge 153"));
        assert!(status.contains("direct page loopback"));
        assert!(status.contains("local network access"));
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

        let input = responses_input(&projected);
        let serialized = input.to_string();
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
                provenance_label: None,
            },
            DisplayMessage {
                role: DisplayRole::User,
                text: "  a useful local title\nwith whitespace  ".to_owned(),
                sequence: 2,
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
        let events = vec![
            EventEnvelope {
                sequence: 1,
                at_unix_ms: 1,
                scope: None,
                kind: EventKind::DraftChanged,
                payload: "old draft".to_owned(),
            },
            EventEnvelope {
                sequence: 2,
                at_unix_ms: 2,
                scope: Some("local-turn:test".to_owned()),
                kind: EventKind::UserMessageCommitted,
                payload: "committed".to_owned(),
            },
        ];

        assert_eq!(projected_working_draft(&events), "");
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
    fn responses_input_uses_durable_transcript_order() {
        let messages = vec![
            DisplayMessage {
                role: DisplayRole::User,
                text: "one".to_owned(),
                sequence: 1,
                provenance_label: None,
            },
            DisplayMessage {
                role: DisplayRole::Assistant,
                text: "two".to_owned(),
                sequence: 2,
                provenance_label: None,
            },
            DisplayMessage {
                role: DisplayRole::User,
                text: "three".to_owned(),
                sequence: 3,
                provenance_label: None,
            },
        ];

        assert_eq!(
            responses_input(&messages),
            serde_json::json!([
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
            }
        ));
        assert!(remote_error_is_observed_failure(
            &siwc_bridge::BridgeError {
                code: "model_not_found".to_owned(),
                message: "bad model".to_owned(),
                retryable: false,
                status: None,
            }
        ));
        assert!(remote_error_is_observed_failure(
            &siwc_bridge::BridgeError {
                code: "response_incomplete".to_owned(),
                message: "server reported incomplete".to_owned(),
                retryable: true,
                status: None,
            }
        ));
        assert!(remote_error_is_observed_failure(
            &siwc_bridge::BridgeError {
                code: "sharing_not_enabled".to_owned(),
                message: "sharing disabled before request".to_owned(),
                retryable: false,
                status: None,
            }
        ));
        assert!(!remote_error_is_observed_failure(
            &siwc_bridge::BridgeError {
                code: "network_error".to_owned(),
                message: "socket closed".to_owned(),
                retryable: true,
                status: None,
            }
        ));
    }
}
