//! Existing egui application frame; extracted without changing behavior.
//! Split into smaller UI panels in roadmap item R13.
use super::*;

mod composer;
mod coordination_window;
mod header;
mod reader;
mod sidebar;

impl eframe::App for ChatariumApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgb(23, 24, 29).to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_notices();
        self.network_health.poll(ctx);
        // Only rate-limit queues auto-release; human approval and manual
        // queues remain frozen until explicitly decided.
        if self.coordination_last_queue_check.elapsed() >= Duration::from_secs(2) {
            self.coordination_last_queue_check = Instant::now();
            let before = self.coordination_workspace.clone();
            if self
                .coordination_workspace
                .advance_rate_queues(unix_now_ms())
                > 0
            {
                if let Err(error) = self
                    .coordination_workspace
                    .save_atomic(&self.coordination_store_path())
                {
                    self.coordination_workspace = before;
                    self.status = format!("coordination queue delivery not saved: {error}");
                }
            }
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        if ACCOUNT_MIRRORING_ENABLED {
            self.process_mirror_controller_notices();
            self.process_live_mirror_fetch_notices();
        }
        self.process_remote_notices();
        self.process_due_delivery_retry();
        self.maybe_wake_open_coordination_worker();
        self.maybe_dispatch_ready_openviking_write();
        self.maybe_generate_openviking_summary();
        self.maybe_retrieve_openviking_context();
        self.maybe_retrieve_project_openviking_context();
        // Visibility preferences are local-only and never mutate the durable
        // audit. Cache them in egui state to avoid disk reads each frame.
        let display_preferences_path = self.journal_path.with_file_name("display-preferences.json");
        let display_preferences_id = egui::Id::new("chatarium-display-preferences");
        let mut display_preferences = ctx.data_mut(|data| {
            data.get_temp::<display_preferences::DisplayPreferences>(display_preferences_id)
                .unwrap_or_else(|| {
                    display_preferences::DisplayPreferences::load(&display_preferences_path)
                })
        });
        ctx.data_mut(|data| data.insert_temp(display_preferences_id, display_preferences.clone()));

        // Consume global native chat navigation before resolving conversation
        // title, visible transcript, search projection or reader state. Doing
        // it after projection rendered the old chat despite a successful
        // catalog activation, until an unrelated event such as a resize.
        let (new_chat_shortcut, cycle_shortcut) =
            ctx.input_mut(conversation_keybindings::consume_native_conversation_shortcuts);
        if new_chat_shortcut {
            let before = self.local_conversation_id;
            self.create_local_conversation();
            if self.local_conversation_id != before {
                self.local_conversation_search_query.clear();
                self.local_conversation_search_selection = None;
                ctx.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new("chatarium-message-composer"))
                });
                ctx.request_repaint();
            }
        } else if let Some(direction) = cycle_shortcut {
            let native_selected = self.selected_historical_conversation.is_none()
                && self.selected_remote_catalog_id.is_none();
            let current = native_selected.then_some(self.local_conversation_id);
            if let Some(target) = conversation_navigation::cycle_target(
                &self.local_conversation_catalog.entries(),
                current,
                direction,
            ) {
                let can_switch = !self.local_conversation_busy();
                let trace_cycle =
                    std::env::var("CHATARIUM_QA_SHORTCUT_TRACE").as_deref() == Ok("1");
                if trace_cycle {
                    eprintln!(
                        "CHATARIUM_QA_CYCLE_BEFORE busy={} unsaved_draft={} pending_commit={} pending_remote={} active_remote={} same_target={}",
                        !can_switch,
                        self.saved_revision < self.draft_revision,
                        self.commit_in_flight.is_some(),
                        self.pending_remote_turn.is_some(),
                        self.active_remote_turn.is_some(),
                        self.local_conversation_id == target
                    );
                }
                let before = self.local_conversation_id;
                self.activate_local_conversation(target);
                let selected = self.local_conversation_id == target;
                if can_switch && selected {
                    // A hidden search filter must not hide the cycled-to row.
                    self.local_conversation_search_query.clear();
                    self.local_conversation_search_selection = None;
                }
                if before != self.local_conversation_id {
                    ctx.request_repaint();
                }
                if trace_cycle {
                    eprintln!("CHATARIUM_QA_CYCLE_AFTER selected={selected}");
                }
            }
        }

        // Search and the active chat reader share the same cached native
        // message corpus. Avoid an extra full-journal scan and a clone of
        // every visible transcript message on each egui redraw.
        let native_search_index = native_conversation_search::cached_index(ctx, &self.events);
        let local_display_messages = native_search_index.messages(self.local_conversation_id);
        let local_conversation_title = self
            .local_conversation_catalog
            .entry(self.local_conversation_id)
            .and_then(|entry| entry.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| derived_conversation_title(local_display_messages));
        let sidebar = self.render_sidebar(
            ctx,
            &native_search_index,
            local_display_messages,
            &local_conversation_title,
            &mut display_preferences,
            &display_preferences_path,
            display_preferences_id,
        );
        let historical_mode = sidebar.historical_mode;
        let remote_catalog_selected = sidebar.remote_catalog_selected;
        let selected_live_mirror = sidebar.selected_live_mirror;
        let selected_historical_id = sidebar.selected_historical_id;
        let header_conversation_title = sidebar.header_conversation_title;
        let mut mirror_remote_requested = sidebar.mirror_remote_requested;
        let mut sync_live_requested = sidebar.sync_live_requested;
        self.render_coordination_window(ctx);

        if rename_local_requested {
            self.rename_current_local_conversation();
        }
        if save_project_directory_requested {
            self.save_current_project_directory();
        }
        if let Some((source, allow)) = project_memory_permission_request {
            self.change_project_memory_permission(source, allow);
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

        self.render_conversation_header(
            ctx,
            historical_mode,
            &header_conversation_title,
            remote_catalog_selected,
            selected_live_mirror,
            &local_conversation_title,
            local_display_messages,
        );

        self.render_composer_panel(
            ctx,
            historical_mode,
            remote_catalog_selected,
            selected_live_mirror,
            selected_historical_id,
            &display_preferences,
            &mut mirror_remote_requested,
            &mut sync_live_requested,
        );

        if let Some(local_conversation_id) = sync_live_requested {
            self.sync_historical_conversation(local_conversation_id, ctx);
        }

        self.render_conversation_reader(
            ctx,
            historical_mode,
            selected_live_mirror,
            remote_catalog_selected,
            &display_preferences,
            local_display_messages,
            &native_search_index,
        );

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
