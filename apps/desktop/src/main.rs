mod siwc_bridge;

use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId,
    LocalTurnId, RemoteEvidence, TurnEvidence,
};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, decode_user_message_commit, local_turn_scope,
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
    Failed {
        operation: &'static str,
        revision: Option<u64>,
        request_id: Option<u64>,
        turn_id: Option<LocalTurnId>,
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

struct ChatariumApp {
    draft: String,
    draft_revision: u64,
    saved_revision: u64,
    next_commit_request: u64,
    commit_in_flight: Option<u64>,
    evidence: TurnEvidence,
    local_conversation_id: LocalConversationId,
    events: Vec<EventEnvelope>,
    journal_path: PathBuf,
    persist_tx: Option<Sender<PersistCommand>>,
    notice_rx: Option<Receiver<PersistNotice>>,
    worker: Option<JoinHandle<()>>,
    status: String,
    remote: siwc_bridge::BridgeRuntime,
    remote_session: siwc_bridge::SessionState,
    remote_models: Vec<siwc_bridge::Model>,
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
                let (persist_tx, persist_rx) = mpsc::channel();
                let (notice_tx, notice_rx) = mpsc::channel();
                let worker = thread::Builder::new()
                    .name("chatarium-persistence".to_owned())
                    .spawn(move || persistence_worker(store, persist_rx, notice_tx));

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
                        journal_path,
                        persist_tx: Some(persist_tx),
                        notice_rx: Some(notice_rx),
                        worker: Some(worker),
                        status: startup_status,
                        remote: siwc_bridge::BridgeRuntime::start(repaint),
                        remote_session: siwc_bridge::SessionState::default(),
                        remote_models: Vec::new(),
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
            events,
            journal_path,
            persist_tx: None,
            notice_rx: None,
            worker: None,
            status,
            remote: siwc_bridge::BridgeRuntime::start(repaint),
            remote_session: siwc_bridge::SessionState::default(),
            remote_models: Vec::new(),
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
                                self.remote_status = format!("sending with {}", pending.model);
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
                    self.sign_in_pending = session.status == "connecting";
                    self.remote_session = session;
                    if self.remote_session.status == "connected" && self.remote_session.sharing {
                        self.remote_status = "ChatGPT plan connected".to_owned();
                        let _ = self.remote.send(siwc_bridge::BridgeCommand::ListModels);
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
                        self.remote_status = "not connected".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::Models(models) => {
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
                    self.remote_status = "ChatGPT is responding…".to_owned();
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
                    self.remote_status = "ChatGPT response complete".to_owned();
                }
                siwc_bridge::BridgeEvent::Failed { request_id, error } => {
                    self.sign_in_pending = false;
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
        self.process_remote_notices();

        let display_messages =
            projected_local_display_messages(&self.events, self.local_conversation_id);
        let conversation_title = derived_conversation_title(&display_messages);

        egui::SidePanel::left("sidebar")
            .exact_width(236.0)
            .resizable(false)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(18, 19, 23))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
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
                    .fill(egui::Color32::from_rgb(31, 33, 39))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(10, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(conversation_title.as_str())
                                    .strong()
                                    .color(egui::Color32::from_rgb(229, 231, 236)),
                            );
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} transcript message{}",
                                display_messages.len(),
                                if display_messages.len() == 1 { "" } else { "s" }
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                    });

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
                            .width(196.0)
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
                        .add_sized([196.0, 30.0], egui::Button::new("Disconnect ChatGPT"))
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
                                .min_size(egui::vec2(196.0, 34.0)),
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
                            .add_sized([196.0, 28.0], egui::Button::new("Cancel sign-in"))
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
                        egui::RichText::new(format!("journal\n{}", self.journal_path.display()))
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
                            egui::RichText::new(if self.remote_connected() {
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
                            .fill(if connected {
                                egui::Color32::from_rgb(24, 52, 37)
                            } else {
                                egui::Color32::from_rgb(48, 42, 26)
                            })
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(10, 5))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(if connected {
                                        "CHATGPT CONNECTED"
                                    } else {
                                        "LOCAL ONLY"
                                    })
                                    .size(10.0)
                                    .strong()
                                    .color(if connected {
                                        egui::Color32::from_rgb(126, 210, 156)
                                    } else {
                                        egui::Color32::from_rgb(225, 194, 108)
                                    }),
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
                                ui.label(
                                    egui::RichText::new("Start a local conversation")
                                        .size(24.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(221, 223, 229)),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(if self.remote_connected() {
                                        "Write below to send a durable turn through your ChatGPT plan."
                                    } else {
                                        "Messages committed here survive restarts. Connect ChatGPT to enable remote turns."
                                    })
                                    .size(13.0)
                                    .color(egui::Color32::from_rgb(137, 141, 150)),
                                );
                            });
                        } else {
                            ui.add_space(8.0);
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
                egui::RichText::new(format!("event #{}", message.sequence))
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(10.0)
                    .color(egui::Color32::from_rgb(126, 130, 139)),
            );
        });
    });
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
            PersistCommand::Shutdown => break,
        }
    }
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
        });
    }

    messages
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
            },
            DisplayMessage {
                role: DisplayRole::User,
                text: "  a useful local title\nwith whitespace  ".to_owned(),
                sequence: 2,
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
        let worker = thread::spawn(move || persistence_worker(store, command_rx, notice_tx));

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
            },
            DisplayMessage {
                role: DisplayRole::Assistant,
                text: "two".to_owned(),
                sequence: 2,
            },
            DisplayMessage {
                role: DisplayRole::User,
                text: "three".to_owned(),
                sequence: 3,
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
