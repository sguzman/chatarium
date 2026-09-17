use chatarium_core::{EventKind, TurnEvidence};
use chatarium_store::{EventEnvelope, EventStore, JsonlEventStore};
use eframe::egui;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

enum PersistCommand {
    SaveDraft { revision: u64, text: String },
    CommitMessage { request_id: u64, text: String },
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
                        events,
                        journal_path,
                        persist_tx: Some(persist_tx),
                        notice_rx: Some(notice_rx),
                        worker: Some(worker),
                        status: "journal ready".to_owned(),
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
        let text = self.draft.clone();
        match sender.send(PersistCommand::CommitMessage { request_id, text }) {
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

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Chatarium");
                ui.separator();
                ui.label("local journal active; remote protocol intentionally disabled");
                ui.separator();
                ui.monospace(self.draft_state());
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Local-first conversation surface");
            ui.label(
                "Nothing here is sent to ChatGPT yet. This surface is proving the durability contract before networking is allowed to depend on it.",
            );
            ui.add_space(8.0);
            ui.small(format!("journal: {}", self.journal_path.display()));
            ui.small(format!("status: {}", self.status));
            ui.add_space(12.0);

            ui.heading("Locally committed messages");
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    let mut found = false;
                    for event in self
                        .events
                        .iter()
                        .filter(|event| event.kind == EventKind::UserMessageCommitted)
                    {
                        found = true;
                        ui.group(|ui| {
                            let scope = event.scope.as_deref().unwrap_or("native/unscoped");
                            ui.strong(format!(
                                "You · local event #{} · {scope}",
                                event.sequence
                            ));
                            ui.label(event_text(&event.payload));
                        });
                        ui.add_space(4.0);
                    }
                    if !found {
                        ui.weak("No committed local messages yet.");
                    }
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Composer");
                ui.label(format!("draft: {}", self.draft_state()));
            });

            let editor = egui::TextEdit::multiline(&mut self.draft)
                .desired_rows(8)
                .hint_text("Type here. Every edit is queued immediately for the append-only journal.");
            let response = ui.add_enabled(self.commit_in_flight.is_none(), editor);
            if response.changed() {
                self.evidence = TurnEvidence::default();
                self.queue_draft_snapshot();
            }

            ui.horizontal(|ui| {
                let can_commit = self.persist_tx.is_some()
                    && self.commit_in_flight.is_none()
                    && !self.draft.trim().is_empty();
                if ui
                    .add_enabled(can_commit, egui::Button::new("Commit locally (no network)"))
                    .clicked()
                {
                    self.commit_current_message();
                }
                if self.commit_in_flight.is_some() {
                    ui.spinner();
                    ui.label("waiting for fsync acknowledgement");
                }
            });

            ui.add_space(8.0);
            ui.monospace(format!("current turn evidence: {:?}", self.evidence));

            ui.add_space(8.0);
            egui::CollapsingHeader::new(format!(
                "Durable event journal ({} events)",
                self.events.len()
            ))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for event in self.events.iter().rev().take(100).rev() {
                            ui.horizontal_wrapped(|ui| {
                                let scope = event.scope.as_deref().unwrap_or("-");
                                ui.monospace(format!(
                                    "#{:05} {:>13} {:>28} [{scope}]",
                                    event.sequence,
                                    event.at_unix_ms,
                                    event.kind.stable_name()
                                ));
                                ui.label(payload_preview(&event.payload));
                            });
                        }
                    });
            });
        });

        if self.saved_revision < self.draft_revision || self.commit_in_flight.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
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
            PersistCommand::CommitMessage { request_id, text } => {
                match store.append(EventKind::UserMessageCommitted, text) {
                    Ok(_) => {
                        if let Some(event) = store.events().last().cloned() {
                            let _ =
                                notices.send(PersistNotice::MessageCommitted { request_id, event });
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
                }
            }
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
        .find(|event| event.scope.is_none() && event.kind == EventKind::UserMessageCommitted)
        .map(|event| event.sequence)
        .unwrap_or_default();

    match latest_draft {
        Some(event) if event.sequence > latest_commit_sequence => event_text(&event.payload),
        _ => String::new(),
    }
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

fn event_text(payload: &str) -> String {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|value| value.get("text").and_then(Value::as_str).map(ToOwned::to_owned))
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Chatarium",
        options,
        Box::new(|_creation_context| Ok(Box::new(ChatariumApp::new()))),
    )
}
