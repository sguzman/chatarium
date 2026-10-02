use chatarium_core::{
    AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId, LocalTurnId, TurnEvidence,
};
use chatarium_store::authored::{
    DecodedUserMessageCommit, commit_user_message, decode_user_message_commit,
};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
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
}

impl eframe::App for ChatariumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_notices();

        let committed_messages = self
            .events
            .iter()
            .filter(|event| event.kind == EventKind::UserMessageCommitted)
            .cloned()
            .collect::<Vec<_>>();
        let conversation_title = derived_conversation_title(&committed_messages);

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
                                "{} committed message{}",
                                committed_messages.len(),
                                if committed_messages.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                }
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
                status_row(ui, "Remote", "not connected", false);

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
                            egui::RichText::new(
                                "Durable on this machine · remote ChatGPT connection not wired yet",
                            )
                            .size(11.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(48, 42, 26))
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(10, 5))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new("LOCAL ONLY")
                                        .size(10.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(225, 194, 108)),
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

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(match self.draft_state() {
                            "durable" => "Draft saved locally",
                            "saving…" => "Saving draft…",
                            _ => "Draft is not durable",
                        })
                        .size(11.0)
                        .color(egui::Color32::from_rgb(132, 136, 145)),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_commit = self.persist_tx.is_some()
                            && self.commit_in_flight.is_none()
                            && !self.draft.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_commit,
                                egui::Button::new(egui::RichText::new("Commit locally").strong())
                                    .min_size(egui::vec2(124.0, 34.0)),
                            )
                            .clicked()
                        {
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
                        if committed_messages.is_empty() {
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
                            for event in committed_messages {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Min),
                                    |ui| {
                                        egui::Frame::default()
                                            .fill(egui::Color32::from_rgb(38, 42, 52))
                                            .corner_radius(egui::CornerRadius::same(12))
                                            .inner_margin(egui::Margin::symmetric(14, 11))
                                            .show(ui, |ui| {
                                                ui.set_max_width(620.0);
                                                ui.label(
                                                    egui::RichText::new(event_text(&event.payload))
                                                        .size(14.0)
                                                        .color(egui::Color32::from_rgb(
                                                            232, 234, 239,
                                                        )),
                                                );
                                                ui.add_space(5.0);
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "local event #{}",
                                                        event.sequence
                                                    ))
                                                    .size(10.0)
                                                    .color(egui::Color32::from_rgb(
                                                        126, 131, 143,
                                                    )),
                                                );
                                            });
                                    },
                                );
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

fn derived_conversation_title(events: &[EventEnvelope]) -> String {
    let Some(first) = events.first() else {
        return "New local conversation".to_owned();
    };

    let text = event_text(&first.payload);
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
