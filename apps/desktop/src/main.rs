mod siwc_bridge;

use chatarium_core::{
    AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId, LocalTurnId, TurnEvidence,
};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, decode_user_message_commit,
};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
use std::collections::BTreeMap;
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
    Shutdown,
}

enum PersistNotice {
    DraftSaved {
        revision: u64,
        event: EventEnvelope,
    },
    MessageCommitted {
        request_id: u64,
        event: EventEnvelope,
    },
    Failed {
        operation: &'static str,
        revision: Option<u64>,
        request_id: Option<u64>,
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
    sign_in_pending: bool,
}

impl ChatariumApp {
    fn new() -> Self {
        let journal_path = default_journal_path();
        match JsonlEventStore::open(&journal_path) {
            Ok(store) => {
                let events = store.events().to_vec();
                let draft = projected_working_draft(&events);
                let (local_conversation_id, startup_status) =
                    match projected_local_conversation_id(&events) {
                        Ok(Some(id)) => (id, "journal ready".to_owned()),
                        Ok(None) => (LocalConversationId::new(), "journal ready".to_owned()),
                        Err(error) => (
                            LocalConversationId::new(),
                            format!("journal ready; local identity replay warning: {error}"),
                        ),
                    };
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
                        remote: siwc_bridge::BridgeRuntime::start(),
                        remote_session: siwc_bridge::SessionState::default(),
                        remote_models: Vec::new(),
                        selected_model: None,
                        remote_status: "starting sign-in runtime…".to_owned(),
                        sign_in_pending: false,
                    },
                    Err(error) => Self::without_persistence(
                        journal_path,
                        draft,
                        events,
                        format!("failed to start persistence worker: {error}"),
                    ),
                }
            }
            Err(error) => Self::without_persistence(
                journal_path,
                String::new(),
                Vec::new(),
                format!("failed to open journal: {error}"),
            ),
        }
    }

    fn without_persistence(
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
            remote: siwc_bridge::BridgeRuntime::start(),
            remote_session: siwc_bridge::SessionState::default(),
            remote_models: Vec::new(),
            selected_model: None,
            remote_status: "starting sign-in runtime…".to_owned(),
            sign_in_pending: false,
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
        match sender.send(PersistCommand::CommitMessage {
            request_id,
            message,
        }) {
            Ok(()) => {
                self.commit_in_flight = Some(request_id);
                self.status = "committing exact user message to local journal…".to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue commit: {error}");
            }
        }
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
                    if self.saved_revision == self.draft_revision && self.commit_in_flight.is_none()
                    {
                        self.status = "draft durable".to_owned();
                    }
                }
                PersistNotice::MessageCommitted { request_id, event } => {
                    let sequence = event.sequence;
                    self.events.push(event);
                    if self.commit_in_flight == Some(request_id) {
                        self.commit_in_flight = None;
                        self.evidence.commit_local_message();
                        self.status =
                            format!("user message durably committed as event #{sequence}");
                        self.draft.clear();
                        self.queue_draft_snapshot();
                    }
                }
                PersistNotice::Failed {
                    operation,
                    revision,
                    request_id,
                    error,
                } => {
                    if request_id.is_some() && request_id == self.commit_in_flight {
                        self.commit_in_flight = None;
                    }
                    if let Some(revision) = revision {
                        self.saved_revision = self.saved_revision.min(revision.saturating_sub(1));
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
                    self.remote_status = "sign-in runtime ready".to_owned();
                    let _ = self
                        .remote
                        .send(siwc_bridge::BridgeCommand::RefreshSession);
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
                siwc_bridge::BridgeEvent::Failed { error, .. } => {
                    self.sign_in_pending = false;
                    self.remote_status = format!("{}: {}", error.code, error.message);
                }
                siwc_bridge::BridgeEvent::RuntimeUnavailable(detail) => {
                    self.sign_in_pending = false;
                    self.remote_status = detail;
                }
                siwc_bridge::BridgeEvent::Delta { .. }
                | siwc_bridge::BridgeEvent::ResponseCompleted { .. }
                | siwc_bridge::BridgeEvent::CommandSucceeded { .. } => {}
            }
        }
    }

    fn start_chatgpt_sign_in(&mut self) {
        self.sign_in_pending = true;
        self.remote_status = "opening ChatGPT sign-in…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::SignIn) {
            self.sign_in_pending = false;
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

        let display_messages = projected_display_messages(&self.events);
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
                    if ui
                        .add_enabled(
                            !self.sign_in_pending,
                            egui::Button::new(
                                egui::RichText::new("Continue with ChatGPT").strong(),
                            )
                            .min_size(egui::vec2(196.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.start_chatgpt_sign_in();
                    }
                    if self.sign_in_pending {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                egui::RichText::new("Finish in your browser")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(151, 154, 163)),
                            );
                        });
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

                let can_commit = self.persist_tx.is_some()
                    && self.commit_in_flight.is_none()
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
                                egui::Button::new(egui::RichText::new("Commit locally").strong())
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
                                    egui::RichText::new(
                                        "Messages committed here survive restarts. Remote ChatGPT transport is the next vertical slice.",
                                    )
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

        if self.saved_revision < self.draft_revision || self.commit_in_flight.is_some() {
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
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::CommitMessage {
                request_id,
                message,
            } => match commit_user_message(&mut store, &message) {
                Ok(_) => {
                    if let Some(event) = store.events().last().cloned() {
                        let _ = notices.send(PersistNotice::MessageCommitted { request_id, event });
                    }
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "message commit",
                        revision: None,
                        request_id: Some(request_id),
                        error: error.to_string(),
                    });
                }
            },
            PersistCommand::Shutdown => break,
        }
    }
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
        DisplayRole::Assistant => &["/details/observed_id"],
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
            Ok(Box::new(ChatariumApp::new()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
