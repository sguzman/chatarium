//! Nonblocking GUI bootstrap for authoritative journal loading and validation.
//!
//! Disk IO and full source-quarantine replay happen on a named worker thread.
//! The resulting single-writer journal moves to the GUI thread only after
//! validation. GUI construction, egui and application runtime setup never run
//! on the loader thread.

use chatarium_store::JsonlEventStore;
use chatarium_store::effective_event_store::EffectiveEventStore;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use crate::local_conversations::LocalConversationCatalog;
use crate::native_conversation_search::NativeConversationSearchIndex;
use crate::{ChatariumApp, EventEnvelope, LocalConversationId, default_journal_path};

/// Completed off-thread snapshot; UI receives already reconciled native state.
pub(crate) struct LoadedJournal {
    pub(crate) store: EffectiveEventStore<JsonlEventStore>,
    pub(crate) events: Vec<EventEnvelope>,
    pub(crate) native_index: Arc<NativeConversationSearchIndex>,
    pub(crate) recovery: Result<usize, String>,
    pub(crate) workspace: LoadedWorkspace,
}

pub(crate) struct LoadedWorkspace {
    pub(crate) catalog: LocalConversationCatalog,
    pub(crate) conversation_id: LocalConversationId,
    pub(crate) status: String,
    pub(crate) draft: String,
}

type JournalLoad = Result<LoadedJournal, String>;

pub(crate) struct BootstrapApp {
    journal_path: PathBuf,
    load_result: Option<Receiver<JournalLoad>>,
    start_failure: Option<String>,
    desktop: Option<ChatariumApp>,
}

impl BootstrapApp {
    pub(crate) fn new() -> Self {
        let journal_path = default_journal_path();
        let started = launch_journal_loader(journal_path.clone());
        let (load_result, start_failure) = match started {
            Ok(receiver) => (Some(receiver), None),
            Err(error) => (
                None,
                Some(format!("failed to start journal loader: {error}")),
            ),
        };
        Self {
            journal_path,
            load_result,
            start_failure,
            desktop: None,
        }
    }
}

fn launch_journal_loader(path: PathBuf) -> std::io::Result<Receiver<JournalLoad>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("chatarium-journal-bootstrap".to_owned())
        .spawn(move || {
            let result = ChatariumApp::load_journal_for_bootstrap(&path);
            // App shutdown while bootstrapping simply drops the receiver.
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}

impl eframe::App for BootstrapApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgb(23, 24, 29).to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.desktop.is_none() {
            let completed = if let Some(error) = self.start_failure.take() {
                Some(Err(error))
            } else if let Some(receiver) = self.load_result.as_ref() {
                match receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err(
                        "journal loader terminated before delivering a result".to_owned(),
                    )),
                    Err(TryRecvError::Empty) => None,
                }
            } else {
                Some(Err("journal bootstrap receiver is unavailable".to_owned()))
            };

            if let Some(result) = completed {
                self.load_result = None;
                self.desktop = Some(ChatariumApp::from_loaded_journal(
                    ctx,
                    self.journal_path.clone(),
                    result,
                ));
            } else {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(120.0);
                        ui.heading("Loading local archive");
                        ui.add_space(14.0);
                        ui.label("Checking journal integrity and source quarantine decisions…");
                        ui.spinner();
                    });
                });
                ctx.request_repaint_after(Duration::from_millis(40));
                return;
            }
        }

        if let Some(desktop) = self.desktop.as_mut() {
            eframe::App::update(desktop, ctx, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::EventKind;
    use chatarium_store::EventStore;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn journal_io_stays_on_worker_and_preserves_single_writer_handoff() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-background-bootstrap-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("journal.jsonl");
        let mut initial = JsonlEventStore::open(&path).unwrap();
        initial
            .append(EventKind::DraftChanged, "native event".to_owned())
            .unwrap();
        drop(initial);

        let receiver = launch_journal_loader(path.clone()).unwrap();
        let mut loaded = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("loader result")
            .expect("validated journal");
        assert_eq!(loaded.events[0].payload, "native event");
        assert_eq!(loaded.store.events(), loaded.events);
        assert_eq!(loaded.store.authoritative_events(), loaded.store.events());
        assert_eq!(loaded.recovery, Ok(0));
        assert!(
            loaded
                .native_index
                .messages(loaded.workspace.conversation_id)
                .is_empty()
        );
        assert!(loaded.workspace.status.starts_with("journal ready"));
        assert!(root.join("local-conversations.json").exists());
        // The background loader's lease is handed to the consumer, without a
        // concurrent re-open, so a second writer cannot enter until drop.
        assert_eq!(
            JsonlEventStore::open(&path).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        loaded
            .store
            .append(EventKind::DraftChanged, "second".to_owned())
            .unwrap();
        assert_eq!(loaded.store.events().last().unwrap().sequence, 2);
        drop(loaded);
        let reopened = JsonlEventStore::open(&path).unwrap();
        assert_eq!(reopened.events().len(), 2);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
