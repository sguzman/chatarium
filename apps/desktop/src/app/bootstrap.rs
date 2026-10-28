//! Desktop bootstrap helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn configure_ui(ctx: &egui::Context) {
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

pub(crate) fn run_production_mirror_acceptance() {
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
    let store = match chatarium_store::effective_event_store::EffectiveEventStore::new(store) {
        Ok(store) => store,
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({
                    "terminal_state": "quarantine_replay_failed",
                    "error": error,
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
