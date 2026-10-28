//! Existing egui application frame; extracted without changing behavior.
//! Split into smaller UI panels in roadmap item R13.
use super::*;

mod composer;
mod coordination_window;
mod header;

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
        let remote_catalog_selected = self.selected_remote_catalog_id.is_some();
        let historical_mode =
            self.selected_historical_conversation.is_some() || remote_catalog_selected;
        if !historical_mode && self.commit_in_flight.is_none() {
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect::<Vec<_>>()
            });
            if dropped.len() > 1 {
                self.status = "only one attachment per turn; select one file".to_owned();
            } else if let Some(path) = dropped.first() {
                self.stage_attachment(path);
            }
        }
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
        // Local autogenerated catalog titles are intentionally capped at 42
        // scalars for sidebars. Do not let that storage preview cap force an
        // ellipsis in a wide conversation header. This is display-only and
        // never changes an explicitly renamed or imported title.
        let header_conversation_title = if historical_mode {
            conversation_title.clone()
        } else {
            expanded_native_conversation_header_title(&conversation_title, local_display_messages)
        };
        let mut select_local_requested: Option<LocalConversationId> = None;
        let mut create_local_requested = false;
        let mut visible_native_search_matches = 0_usize;
        let mut native_search_matches = Vec::<LocalConversationId>::new();
        let mut stage_native_memory_requested: Option<(LocalConversationId, usize)> = None;
        let mut native_reader_target_message: Option<(LocalConversationId, usize)> = None;
        let mut native_reader_source_stale = false;
        let mut native_copy_candidates = Vec::<(LocalConversationId, Option<usize>)>::new();
        let mut native_message_hit_conversations = HashSet::<LocalConversationId>::new();
        let mut rename_local_requested = false;
        let mut save_project_directory_requested = false;
        let mut project_memory_permission_request: Option<(LocalConversationId, bool)> = None;
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
        // Account-history catalog entries are not native Chatarium chats. Do
        // not even query their index while the mirroring surface is paused.
        let archive_results = if ACCOUNT_MIRRORING_ENABLED {
            self.local_archive_search_index.search(
                &self.archive_search_query,
                self.archive_search_mode,
                self.archive_state_filter,
            )
        } else {
            Vec::new()
        };
        if self
            .archive_search_selection
            .is_some_and(|selection| selection >= archive_results.len())
        {
            self.archive_search_selection = None;
        }
        let archive_search_id = egui::Id::new("local-archive-search");
        let native_search_id = egui::Id::new("native-conversation-search");
        let mut native_search_has_focus = false;
        let mut focus_native_search = false;
        let mut archive_search_has_focus = false;
        let mut archive_result_clicked = None;
        let mut open_reader_from_transcript_search = false;
        let mut focus_archive_search = false;
        let mut clear_archive_search_focus = false;
        // Ctrl+K always searches authored local conversations now.
        // The old account-catalog search remains parked with mirroring.
        if ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::K)) {
            focus_native_search = true;
        }

        // Ctrl+L moves focus to the native composer without changing or
        // submitting its draft. Mirrored/remote history has no local editor.
        let focus_native_composer =
            conversation_keybindings::may_focus_native_composer(!historical_mode)
                && ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::L));

        egui::SidePanel::left("sidebar")
            .default_width(290.0)
            .min_width(220.0)
            .max_width(580.0)
            .resizable(true)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(20, 25, 34))
                    .stroke(egui::Stroke::new(
                        1.5,
                        egui::Color32::from_rgb(73, 91, 118),
                    ))
                    .inner_margin(egui::Margin::same(14)),
            )
            .show(ctx, |ui| {
                // Give the scroll area's CONTENT a fixed width from this frame's
                // allocated sidebar, rather than letting long conversation titles,
                // rename fields and status labels force the outer panel wider.
                // Recompute each frame: the splitter must still resize both ways.
                let sidebar_width = ui.available_width();
                // Pin global navigation above the chat sidebar scroll region.
                ui.label(
                    egui::RichText::new("Chatarium")
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::from_rgb(238, 239, 244)),
                );
                let coordination_pending = self
                    .coordination_workspace
                    .containers
                    .iter()
                    .flat_map(|container| container.messages.iter())
                    .filter(|message| {
                        matches!(message.status.as_str(), "delivered" | "queued" | "approval")
                    })
                    .count();
                let coordination_title = if coordination_pending == 0 {
                    "Coordination / messages".to_owned()
                } else {
                    format!("Coordination / messages · {coordination_pending} pending")
                };
                if ui
                    .add_sized(
                        [sidebar_width.max(1.0), 30.0],
                        egui::Button::new(coordination_title),
                    )
                    .clicked()
                {
                    self.coordination_window_open = true;
                }
                let conversation = self.local_conversation_id.to_string();
                let worker_exists = self.coordination_workspace.member_for(&conversation).is_some();
                let mut wake_this_chat =
                    self.coordination_auto_wake_targets.contains(&self.local_conversation_id);
                if ui
                    .add_enabled(
                        worker_exists,
                        egui::Checkbox::new(
                            &mut wake_this_chat,
                            "Auto-wake worker (max 8 combined this session)",
                        ),
                    )
                    .on_hover_text("Arm this individual worker. Its saved lease can wake it when another chat is selected; other workers require their own opt-in.")
                    .changed()
                {
                    // Revocation must fail closed even if the workspace disk
                    // write fails: a stop must immediately block dispatch.
                    if !wake_this_chat {
                        self.coordination_auto_wake_targets.remove(&self.local_conversation_id);
                    }
                    let path = self.coordination_store_path();
                    let outcome = if wake_this_chat {
                        self.coordination_workspace.grant_wake_lease_durable(
                            &path,
                            &conversation,
                            8,
                        )
                    } else {
                        self.coordination_workspace.stop_wake_lease_durable(
                            &path,
                            &conversation,
                        )
                    };
                    match outcome {
                        Ok(()) => {
                            if wake_this_chat {
                                self.coordination_auto_wake_targets.insert(self.local_conversation_id);
                            }
                        }
                        Err(error) => {
                            self.status = if wake_this_chat {
                                format!("coordination wake permission not saved: {error}")
                            } else {
                                format!("worker auto-wake stopped locally; durable stop failed: {error}")
                            };
                        }
                    }
                }
                if self.coordination_auto_wake_targets.contains(&self.local_conversation_id) {
                    let budget = self.coordination_auto_wake_count;
                    let tool_mode = if !self.conversation_mcp_enabled {
                        "MCP is off; tool replies cannot run"
                    } else if mcp_agent_defaults::tool_policy(
                        &self.mcp_agent_defaults.policy_rules,
                        "chatarium.builtin",
                        "coordination.send",
                    ) != Ok(mcp_agent_defaults::ToolPolicy::Automatic) {
                        "Coordination.send is not automatic"
                    } else {
                        "Coordination send permitted by policy"
                    };
                    ui.label(egui::RichText::new(format!(
                        "Auto-wake: ON · {budget}/8 this session · {tool_mode}"
                    )).size(10.0).color(egui::Color32::from_rgb(225, 194, 108)));
                    if let Some(status) = self.latest_coordination_wake_status() {
                        ui.label(egui::RichText::new(status).size(10.0)
                            .color(egui::Color32::from_rgb(186, 199, 220)));
                    }
                }
                if !self.coordination_auto_wake_targets.is_empty()
                    && ui.small_button("Stop all worker auto-wakes").clicked()
                {
                    // Disarm every worker before touching disk. A failed
                    // durable stop can never leave automatic inference enabled.
                    let targets = std::mem::take(&mut self.coordination_auto_wake_targets);
                    let path = self.coordination_store_path();
                    for target in targets {
                        if let Err(error) = self.coordination_workspace.stop_wake_lease_durable(
                            &path,
                            &target.to_string(),
                        ) {
                            self.status = format!(
                                "worker auto-wakes stopped locally; durable stop failed for {target}: {error}"
                            );
                        }
                    }
                }
                ui.separator();
                // Overflowing horizontal content belongs to the scroll area,
                // not to the user-resizable panel. A vertical-only scroll area
                // allows a long child to increase the panel's intrinsic width.
                egui::ScrollArea::both()
                    .id_salt("sidebar-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(sidebar_width);
                        let sidebar_control_width = ui.available_width();
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Local-first workspace")
                                .size(12.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "Build {} · native conversations only",
                                env!("CHATARIUM_BUILD_REVISION"),
                            ))
                            .size(9.0)
                            .color(egui::Color32::from_rgb(112, 176, 137)),
                        );

                        ui.label(
                            egui::RichText::new(format!(
                                "OpenViking · {}",
                                self.openviking_checkpoint_status
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(163, 186, 205)),
                        );
                        ui.add_space(10.0);
                        // Conversation controls belong in the left workspace,
                        // not between the transcript and the composer.
                let mut inference_controls_changed = false;
                let mut behavior_profile_changed = false;
                egui::CollapsingHeader::new("Display · messages and diagnostics")
                    .id_salt("chatarium-diagnostics-display")
                    .default_open(false)
                    .show(ui, |ui| {
                        let mut changed = false;
                        ui.label("Coordination messages use their own colors, separately from user, agent, and MCP messages.");
                        egui::CollapsingHeader::new("Coordination message colors")
                            .id_salt("chatarium-coordination-message-colors")
                            .default_open(true)
                            .show(ui, |ui| {
                                ui.label("Background colors for consumed coordination bubbles, by message kind.");
                                for (index, kind) in display_preferences::DisplayPreferences::COORDINATION_KINDS.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(*kind);
                                        changed |= ui.color_edit_button_srgb(
                                            &mut display_preferences.coordination_colors[index]
                                        ).changed();
                                    });
                                }
                                if ui.button("Restore coordination colors").clicked() {
                                    display_preferences.coordination_colors =
                                        display_preferences::DisplayPreferences::default().coordination_colors;
                                    changed = true;
                                }
                            });
                        ui.separator();
                        changed |= ui.checkbox(
                            &mut display_preferences.render_markdown,
                            "Render Markdown in user and agent messages",
                        ).changed();
                        ui.label("Markdown formatting is enabled by default. Copy and storage always preserve the original source text.");
                        ui.separator();
                        changed |= ui.checkbox(
                            &mut display_preferences.show_transport_trace,
                            "Show transport traces in chat",
                        ).changed();
                        changed |= ui.checkbox(
                            &mut display_preferences.show_delivery_history,
                            "Show delivery history below chat",
                        ).changed();
                        ui.label("Both views are debugging diagnostics. Hiding them does not discard their underlying audit events.");
                        ui.separator();
                        egui::CollapsingHeader::new("Chat message colors")
                            .id_salt("chatarium-standard-message-colors")
                            .default_open(false)
                            .show(ui, |ui| {
                                for (index, kind) in display_preferences::DisplayPreferences::MESSAGE_KINDS.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(*kind);
                                        changed |= ui.color_edit_button_srgb(
                                            &mut display_preferences.message_colors[index]
                                        ).changed();
                                    });
                                }
                                if ui.button("Restore standard message colors").clicked() {
                                    display_preferences.message_colors =
                                        display_preferences::DisplayPreferences::default().message_colors;
                                    changed = true;
                                }
                            });
                        if changed {
                            if let Err(error) = display_preferences.save(&display_preferences_path) {
                                self.status = format!("Cannot save display settings: {error}");
                            }
                            ctx.data_mut(|data| {
                                data.insert_temp(display_preferences_id, display_preferences.clone())
                            });
                            ctx.request_repaint();
                        }
                    });
                // Context tools can be much taller than the window. Bound them to
                // their own scroll viewport so the transcript and composer stay usable.
                egui::ScrollArea::vertical()
                    .id_salt(("inference-controls-scroll", self.local_conversation_id))
                    .max_height((ctx.content_rect().height() * 0.42).clamp(180.0, 450.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new("Advanced · context, MCP tools and inference")
                            .id_salt(("inference-controls", self.local_conversation_id))
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
                                egui::RichText::new(local_inference_contract::capability_unavailable_reason(
                                    self.local_inference_contract.as_ref(),
                                    self.remote_session.profile_id.as_deref(),
                                    self.selected_model.as_deref(),
                                    "reasoning",
                                ))
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
                                egui::RichText::new(local_inference_contract::capability_unavailable_reason(
                                    self.local_inference_contract.as_ref(),
                                    self.remote_session.profile_id.as_deref(),
                                    self.selected_model.as_deref(),
                                    "verbosity",
                                ))
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
                                egui::RichText::new(local_inference_contract::capability_unavailable_reason(
                                    self.local_inference_contract.as_ref(),
                                    self.remote_session.profile_id.as_deref(),
                                    self.selected_model.as_deref(),
                                    "web_search",
                                ))
                                .size(9.0)
                                .color(egui::Color32::from_rgb(166, 139, 112)),
                            );
                            let cannot_probe = capability_probe_block_reason(
                                self.remote_connected(),
                                self.selected_model.is_some(),
                                self.pending_remote_turn.is_some(),
                                self.active_remote_turn.is_some(),
                                self.capability_probe.running(),
                            );
                            if let Some(reason) = cannot_probe {
                                ui.label(
                                    egui::RichText::new(format!("Verification unavailable: {reason}"))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(166, 139, 112)),
                                );
                            } else if self.remote_session.profile_id.is_none() {
                                ui.label(
                                    egui::RichText::new("Verification requires a connected profile identity.")
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(166, 139, 112)),
                                );
                            } else if ui.button("Verify + enable web search").clicked() {
                                // Only verify actual web-search execution for this
                                // exact conversation, model, and signed-in profile.
                                let scope = (
                                    self.local_conversation_id,
                                    self.selected_model.clone().expect("model checked"),
                                    self.remote_session.profile_id.clone().expect("profile checked"),
                                );
                                self.start_web_search_probe();
                                if self.capability_probe.running() {
                                    self.pending_web_search_enable = Some(scope);
                                    self.capability_probe.status =
                                        "running one real web-search probe for this model/profile".to_owned();
                                }
                            }
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
                                    "Delivered routed messages are provenance-bearing local inbox items. Context admission is explicit and reversible. Admitted items enter the next request at user trust level with a visible Chatarium provenance envelope; they never become user-authored transcript messages.",
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

                        ui.collapsing("Local memory", |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Immutable explicit memory artifacts. Recording does not add context. Admission is per conversation and snapshotted at Send. Supersession is explicit: stale predecessors remain visible but are mechanically excluded; successors are never auto-admitted.",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );

                            let mut record_clicked = false;
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add_enabled(
                                        !self.local_memory_command_pending,
                                        egui::TextEdit::multiline(&mut self.local_memory_draft)
                                            .desired_width(360.0)
                                            .desired_rows(2)
                                            .hint_text("Exact local memory text"),
                                    )
                                    .changed()
                                {
                                    // An edited draft is no longer an exact
                                    // copy of the staged source message.
                                    self.staged_local_memory_origin = None;
                                }
                                let can_record = !self.local_memory_command_pending
                                    && !self.local_memory_draft.trim().is_empty()
                                    && self.persist_tx.is_some();
                                if ui
                                    .add_enabled(
                                        can_record,
                                        egui::Button::new("Record memory"),
                                    )
                                    .clicked()
                                {
                                    record_clicked = true;
                                }
                                if self.local_memory_command_pending {
                                    ui.spinner();
                                }
                            });
                            if let Some((source, sequence)) = self.staged_local_memory_origin {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Staged from native conversation {source}, message event #{sequence}. Record preserves the source conversation; no context admission occurs.",
                                    ))
                                    .size(10.0),
                                );
                                if ui.button("Discard staged draft").clicked() {
                                    self.local_memory_draft.clear();
                                    self.staged_local_memory_origin = None;
                                }
                            }
                            if record_clicked {
                                self.record_local_memory();
                            }

                            ui.horizontal_wrapped(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.local_memory_search_query,
                                    )
                                    .desired_width(280.0)
                                    .hint_text("Search local memory"),
                                );
                                ui.checkbox(
                                    &mut self.show_superseded_local_memory,
                                    "Show superseded history",
                                );
                            });

                            match replay_local_memory_audit(&self.events) {
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "local memory projection blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Ok(artifacts) if artifacts.is_empty() => {
                                    ui.label(
                                        egui::RichText::new("no local memory artifacts")
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                }
                                Ok(artifacts) => {
                                    let context_records =
                                        match replay_local_memory_context_audit(&self.events) {
                                            Ok(records) => Some(records),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "local memory context projection blocked: {error}"
                                                    ))
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        186, 108, 108,
                                                    )),
                                                );
                                                None
                                            }
                                        };
                                    let supersessions =
                                        match replay_local_memory_supersession_audit(&self.events) {
                                            Ok(records) => Some(records),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "local memory supersession projection blocked: {error}"
                                                    ))
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        186, 108, 108,
                                                    )),
                                                );
                                                None
                                            }
                                        };
                                    let label_facets = match local_memory_label_facets(
                                        &self.events,
                                        self.show_superseded_local_memory,
                                    ) {
                                        Ok(facets) => Some(facets),
                                        Err(error) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "local memory label facets blocked: {error}"
                                                ))
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    186, 108, 108,
                                                )),
                                            );
                                            None
                                        }
                                    };

                                    if let Some(facets) = label_facets.as_ref() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new("Label facet")
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    )),
                                            );
                                            if ui
                                                .selectable_label(
                                                    self.local_memory_label_filter.is_none(),
                                                    "All",
                                                )
                                                .clicked()
                                            {
                                                self.local_memory_label_filter = None;
                                            }
                                            for facet in facets {
                                                let selected = self
                                                    .local_memory_label_filter
                                                    .as_ref()
                                                    == Some(&facet.label);
                                                if ui
                                                    .selectable_label(
                                                        selected,
                                                        format!(
                                                            "{} ({})",
                                                            facet.label,
                                                            facet.artifact_count,
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    self.local_memory_label_filter =
                                                        Some(facet.label.clone());
                                                }
                                            }
                                        });
                                    }

                                    let source_facets = match local_memory_source_facets(
                                        &self.events,
                                        self.show_superseded_local_memory,
                                    ) {
                                        Ok(facets) => Some(facets),
                                        Err(error) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "local memory source facets blocked: {error}"
                                                ))
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    186, 108, 108,
                                                )),
                                            );
                                            None
                                        }
                                    };
                                    if let Some(facets) = source_facets.as_ref() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new("Source facet")
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    )),
                                            );
                                            if ui
                                                .selectable_label(
                                                    self.local_memory_source_filter.is_none(),
                                                    "All",
                                                )
                                                .clicked()
                                            {
                                                self.local_memory_source_filter = None;
                                            }
                                            for facet in facets {
                                                let source_title =
                                                    local_conversation_display_title(
                                                        &self.local_conversation_catalog,
                                                        facet.source_conversation_id,
                                                        &self.events,
                                                    );
                                                let selected = self.local_memory_source_filter
                                                    == Some(facet.source_conversation_id);
                                                if ui
                                                    .selectable_label(
                                                        selected,
                                                        format!(
                                                            "{} ({})",
                                                            source_title,
                                                            facet.artifact_count,
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    self.local_memory_source_filter =
                                                        Some(facet.source_conversation_id);
                                                }
                                            }
                                        });
                                    }

                                    let search_items = match search_local_memory_faceted(
                                        &self.events,
                                        self.local_memory_search_query.as_str(),
                                        self.show_superseded_local_memory,
                                        self.local_memory_label_filter.as_ref(),
                                        self.local_memory_source_filter,
                                    ) {
                                        Ok(items) => Some(items),
                                        Err(error) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "local memory search blocked: {error}"
                                                ))
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    186, 108, 108,
                                                )),
                                            );
                                            None
                                        }
                                    };

                                    if search_items
                                        .as_ref()
                                        .is_some_and(|items| items.is_empty())
                                    {
                                        ui.label(
                                            egui::RichText::new(
                                                "no memories match the current discovery view",
                                            )
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                        );
                                    }

                                    egui::ScrollArea::vertical()
                                        .id_salt("local-memory-artifacts")
                                        .max_height(240.0)
                                        .show(ui, |ui| {
                                            for artifact in search_items
                                                .as_deref()
                                                .unwrap_or_default()
                                            {
                                                let source_title =
                                                    local_conversation_display_title(
                                                        &self.local_conversation_catalog,
                                                        artifact.source_conversation_id,
                                                        &self.events,
                                                    );
                                                let current_decision = context_records
                                                    .as_ref()
                                                    .and_then(|records| {
                                                        records.iter().find(|record| {
                                                            record.memory_id
                                                                == artifact.memory_id
                                                                && record
                                                                    .destination_conversation_id
                                                                    == self.local_conversation_id
                                                        })
                                                    })
                                                    .map(|record| record.decision);
                                                let superseded_by =
                                                    artifact.superseded_by;
                                                let one_shot_selected = self
                                                    .local_memory_one_shot_selections
                                                    .get(&self.local_conversation_id)
                                                    .is_some_and(|selected| {
                                                        selected.contains(&artifact.memory_id)
                                                    });
                                                let successor_candidates = supersessions
                                                    .as_ref()
                                                    .map(|records| {
                                                        artifacts
                                                            .iter()
                                                            .filter(|candidate| {
                                                                candidate.source_conversation_id
                                                                    == artifact
                                                                        .source_conversation_id
                                                                    && candidate.recorded_sequence
                                                                        > artifact
                                                                            .recorded_sequence
                                                                    && !records.iter().any(
                                                                        |record| {
                                                                            record
                                                                                .successor_memory_id
                                                                                == candidate
                                                                                    .memory_id
                                                                        },
                                                                    )
                                                            })
                                                            .collect::<Vec<_>>()
                                                    })
                                                    .unwrap_or_default();

                                                ui.group(|ui| {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "MEMORY {} · source {source_title} · artifact #{}",
                                                                artifact.memory_id.get(),
                                                                artifact.recorded_sequence,
                                                            ))
                                                            .monospace()
                                                            .size(9.0),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(
                                                                if superseded_by.is_some() {
                                                                    "CONTEXT: EXCLUDED · SUPERSEDED"
                                                                } else {
                                                                    match current_decision {
                                                                        Some(
                                                                            LocalMemoryContextDecision::Admit,
                                                                        ) => "CONTEXT: ADMITTED",
                                                                        Some(
                                                                            LocalMemoryContextDecision::Exclude,
                                                                        ) => "CONTEXT: EXCLUDED",
                                                                        None => {
                                                                            "CONTEXT: EXCLUDED · DEFAULT"
                                                                        }
                                                                    }
                                                                },
                                                            )
                                                            .monospace()
                                                            .size(9.0),
                                                        );
                                                        if one_shot_selected {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    "NEXT REQUEST: SELECTED",
                                                                )
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                        }
                                                        if let Some(successor_memory_id) =
                                                            superseded_by
                                                        {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "SUPERSEDED BY MEMORY {}",
                                                                    successor_memory_id.get(),
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                        }
                                                    });

                                                    ui.label(
                                                        egui::RichText::new(
                                                            artifact.text.as_str(),
                                                        )
                                                        .size(10.0),
                                                    );

                                                    if !artifact.labels.is_empty() {
                                                        let mut remove_label = None;
                                                        ui.horizontal_wrapped(|ui| {
                                                            ui.label(
                                                                egui::RichText::new("Labels")
                                                                    .size(9.0)
                                                                    .color(
                                                                        egui::Color32::from_rgb(
                                                                            139, 143, 153,
                                                                        ),
                                                                    ),
                                                            );
                                                            for label in &artifact.labels {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        label.as_str(),
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                                if ui
                                                                    .add_enabled(
                                                                        !self
                                                                            .local_memory_command_pending
                                                                            && self
                                                                                .persist_tx
                                                                                .is_some(),
                                                                        egui::Button::new("×"),
                                                                    )
                                                                    .on_hover_text(format!(
                                                                        "Remove label '{}'",
                                                                        label
                                                                    ))
                                                                    .clicked()
                                                                {
                                                                    remove_label =
                                                                        Some(label.clone());
                                                                }
                                                            }
                                                        });
                                                        if let Some(label) = remove_label {
                                                            self.remove_local_memory_label(
                                                                artifact.memory_id,
                                                                label,
                                                            );
                                                        }
                                                    }

                                                    let mut add_label_clicked = false;
                                                    ui.horizontal_wrapped(|ui| {
                                                        let draft = self
                                                            .local_memory_label_drafts
                                                            .entry(artifact.memory_id)
                                                            .or_default();
                                                        ui.add(
                                                            egui::TextEdit::singleline(draft)
                                                                .desired_width(180.0)
                                                                .hint_text("Add exact label"),
                                                        );
                                                        if ui
                                                            .add_enabled(
                                                                !self.local_memory_command_pending
                                                                    && !draft.is_empty()
                                                                    && self.persist_tx.is_some(),
                                                                egui::Button::new("Add label"),
                                                            )
                                                            .clicked()
                                                        {
                                                            add_label_clicked = true;
                                                        }
                                                    });
                                                    if add_label_clicked {
                                                        self.add_local_memory_label(
                                                            artifact.memory_id,
                                                        );
                                                    }

                                                    ui.horizontal_wrapped(|ui| {
                                                        let can_change =
                                                            !self.local_memory_command_pending
                                                                && context_records.is_some()
                                                                && self.persist_tx.is_some();
                                                        if ui
                                                            .add_enabled(
                                                                can_change
                                                                    && superseded_by.is_none()
                                                                    && current_decision
                                                                        != Some(
                                                                            LocalMemoryContextDecision::Admit,
                                                                        ),
                                                                egui::Button::new(
                                                                    "Admit memory",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.decide_local_memory_context(
                                                                artifact.memory_id,
                                                                LocalMemoryContextDecision::Admit,
                                                            );
                                                        }
                                                        if ui
                                                            .add_enabled(
                                                                can_change
                                                                    && current_decision
                                                                        == Some(
                                                                            LocalMemoryContextDecision::Admit,
                                                                        ),
                                                                egui::Button::new(
                                                                    "Exclude memory",
                                                                ),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.decide_local_memory_context(
                                                                artifact.memory_id,
                                                                LocalMemoryContextDecision::Exclude,
                                                            );
                                                        }

                                                        let selection_mutable =
                                                            self.commit_in_flight.is_none();
                                                        if one_shot_selected {
                                                            if ui
                                                                .add_enabled(
                                                                    selection_mutable,
                                                                    egui::Button::new(
                                                                        "Remove one-shot",
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                self.deselect_local_memory_for_next_request(
                                                                    artifact.memory_id,
                                                                );
                                                            }
                                                        } else if ui
                                                            .add_enabled(
                                                                selection_mutable
                                                                    && superseded_by.is_none()
                                                                    && current_decision
                                                                        != Some(
                                                                            LocalMemoryContextDecision::Admit,
                                                                        ),
                                                                egui::Button::new(
                                                                    "Use once next request",
                                                                ),
                                                            )
                                                            .on_hover_text(
                                                                "Include this memory in the next authored request only; does not change persistent Admit state.",
                                                            )
                                                            .clicked()
                                                        {
                                                            self.select_local_memory_for_next_request(
                                                                artifact.memory_id,
                                                            );
                                                        }
                                                    });

                                                    if superseded_by.is_none()
                                                        && !successor_candidates.is_empty()
                                                    {
                                                        ui.collapsing("Supersede with…", |ui| {
                                                            for candidate in &successor_candidates {
                                                                ui.horizontal_wrapped(|ui| {
                                                                    if ui
                                                                        .add_enabled(
                                                                            !self
                                                                                .local_memory_command_pending
                                                                                && self
                                                                                    .persist_tx
                                                                                    .is_some(),
                                                                            egui::Button::new(
                                                                                format!(
                                                                                    "Memory {} · #{}",
                                                                                    candidate
                                                                                        .memory_id
                                                                                        .get(),
                                                                                    candidate
                                                                                        .recorded_sequence,
                                                                                ),
                                                                            ),
                                                                        )
                                                                        .clicked()
                                                                    {
                                                                        self.supersede_local_memory(
                                                                            artifact.memory_id,
                                                                            candidate.memory_id,
                                                                        );
                                                                    }
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            candidate
                                                                                .text
                                                                                .as_str(),
                                                                        )
                                                                        .size(9.0),
                                                                    );
                                                                });
                                                            }
                                                        });
                                                    }
                                                });
                                            }
                                        });
                                }
                            }
                        });

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.collapsing("Tools / MCP · restricted local adapter", |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "MCP agent calls run through exact saved tool policies and durable execution audits. Built-in read-only discovery and hello can run automatically. External providers require registration and Linux sandbox activation; unsupported host, browser, filesystem and network access remain disabled.",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );

                            ui.horizontal_wrapped(|ui| {
                                if ui.checkbox(&mut self.conversation_mcp_enabled, "Enable MCP tools in this conversation").changed() {
                                    self.inference_settings.mark_mcp_activation_explicit(self.local_conversation_id);
                                    if self.conversation_mcp_enabled {
                                        let bootstrap = self.mcp_agent_defaults.bootstrap.clone();
                                        if !self.conversation_developer_context.contains(&bootstrap) {
                                            if !self.conversation_developer_context.is_empty() {
                                                self.conversation_developer_context.push_str("\n\n");
                                            }
                                            self.conversation_developer_context.push_str(&bootstrap);
                                        }
                                    }
                                    self.persist_current_inference_settings();
                                }
                            });
                            ui.label(if self.conversation_mcp_enabled {
                                "MCP ON · this conversation has the agent tool-call protocol."
                            } else {
                                "MCP OFF · enable above to allow model-proposed tool calls."
                            });
                            ui.collapsing("Host shell · Fish / Bash / Zsh / Sh", |ui| {
                                ui.label(egui::RichText::new(
                                    "UNRESTRICTED HOST ACCESS: commands run with your Linux user identity and Fish configuration. The blacklist is a convenience filter, NOT a security sandbox. A malicious model instruction or search result could cause damaging commands when the MCP policy is automatic."
                                ).color(egui::Color32::from_rgb(230, 170, 104)));
                                ui.checkbox(&mut self.host_shell_settings.enabled, "Enable host shell tool");
                                egui::ComboBox::from_label("Shell")
                                    .selected_text(&self.host_shell_settings.shell)
                                    .show_ui(ui, |ui| {
                                        for shell in ["fish", "bash", "zsh", "sh"] {
                                            ui.selectable_value(&mut self.host_shell_settings.shell, shell.to_owned(), shell);
                                        }
                                    });
                                ui.label("Blocked command names (one per line, optional):");
                                ui.add(egui::TextEdit::multiline(&mut self.host_shell_settings.blocked_commands)
                                    .desired_rows(4).desired_width(f32::INFINITY));
                                if ui.button("Save host shell configuration").clicked() {
                                    self.save_host_shell_settings();
                                }
                                ui.label("Agent tool: chatarium.builtin::shell.command with {\"command\":\"fish syntax\",\"cwd\":\"/absolute/path\"}. Default: Fish, no blocked commands. Change automatic/ask/disabled in Agent defaults.");
                            });
                            ui.collapsing("Agent defaults · bootstrap / tool policies", |ui| {
                                ui.label("Saved MCP bootstrap and exact per-tool execution permissions.");
                                ui.checkbox(
                                    &mut self.mcp_agent_defaults.seed_new_conversations,
                                    "Enable MCP automatically in new local conversations",
                                ).on_hover_text("New local conversations inherit this saved developer bootstrap and enable MCP. Exact automatic-policy tools run; ask-mode tools require user approval.");
                                ui.label("Model-facing MCP instructions (editable):");
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.mcp_agent_defaults.bootstrap)
                                        .desired_rows(5)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.label("Requested policies · provider::operation = automatic|ask|manual|disabled");
                                ui.collapsing("Local HTTP tools · automatic by default · editable", |ui| {
                                    ui.label("Change an individual policy, then press Save MCP defaults. Mathematica code can run with your Linux user privileges.");
                                    for (provider, operation, _) in local_http_mcp::registered_tools() {
                                        let current = mcp_agent_defaults::tool_policy(
                                            &self.mcp_agent_defaults.policy_rules,
                                            provider,
                                            operation,
                                        ).unwrap_or(mcp_agent_defaults::ToolPolicy::Ask);
                                        let mut selected = current;
                                        ui.horizontal(|ui| {
                                            ui.label(operation);
                                            egui::ComboBox::from_id_salt(("local-http-policy", provider, operation))
                                                .selected_text(selected.name())
                                                .show_ui(ui, |ui| {
                                                    for policy in [
                                                        mcp_agent_defaults::ToolPolicy::Automatic,
                                                        mcp_agent_defaults::ToolPolicy::Ask,
                                                        mcp_agent_defaults::ToolPolicy::Manual,
                                                        mcp_agent_defaults::ToolPolicy::Disabled,
                                                    ] {
                                                        ui.selectable_value(&mut selected, policy, policy.name());
                                                    }
                                                });
                                        });
                                        if selected != current {
                                            self.mcp_agent_defaults.policy_rules =
                                                mcp_agent_defaults::set_tool_policy_rule(
                                                    &self.mcp_agent_defaults.policy_rules,
                                                    provider,
                                                    operation,
                                                    selected,
                                                );
                                        }
                                    }
                                });
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.mcp_agent_defaults.policy_rules)
                                        .desired_rows(4)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.horizontal(|ui| {
                                    ui.label("Future per-turn tool-call limit");
                                    ui.add(egui::DragValue::new(&mut self.mcp_agent_defaults.max_calls)
                                        .range(1..=32));
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Save MCP defaults").clicked() {
                                        self.save_mcp_agent_defaults();
                                    }
                                    if ui.button("Copy bootstrap").clicked() {
                                        ctx.copy_text(self.mcp_agent_defaults.bootstrap.clone());
                                    }
                                    if ui.button("Enable MCP in this chat").clicked() {
                                        self.inference_settings.mark_mcp_activation_explicit(self.local_conversation_id);
                                        let bootstrap = self.mcp_agent_defaults.bootstrap.clone();
                                        if !self.conversation_developer_context.contains(&bootstrap) {
                                            if !self.conversation_developer_context.is_empty() {
                                                self.conversation_developer_context.push_str("\n\n");
                                            }
                                            self.conversation_developer_context.push_str(&bootstrap);
                                            self.persist_current_inference_settings();
                                        }
                                        self.conversation_mcp_enabled = true;
                                        self.persist_current_inference_settings();
                                        self.status = "MCP proposal protocol enabled for this chat. Exact standing tool policies may execute automatically; ask-mode tools pause for approval.".to_owned();
                                    }
                                });
                                ui.label("Automatic execution requires a saved exact provider::tool rule. Unmatched tools ask for approval. The agent resumes after a durable result.");
                            });

                            if ui.add_enabled(
                                cfg!(target_os = "linux")
                                    && !self.tool_command_pending
                                    && self.persist_tx.is_some(),
                                egui::Button::new("Check Linux sandbox host · no tool call"),
                            ).on_hover_text(
                                "Runs only the fixed root-owned /usr/bin/true under the actual network-isolated namespace and resource-limit policy. No configured provider launches; no journal dispatch or permission is consumed.",
                            ).clicked() {
                                self.check_sandbox_host_readiness();
                            }
                            if let Some(observation) = &self.last_sandbox_host_check {
                                match observation {
                                    Ok(()) => {
                                        ui.label(
                                            egui::RichText::new("Last Linux sandbox check: READY · fixed /usr/bin/true only")
                                                .color(egui::Color32::from_rgb(106, 169, 128)),
                                        );
                                    }
                                    Err(detail) => {
                                        let category = local_stdio_host_diagnostics::failure_category(detail);
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Last Linux sandbox check: NOT READY · {category}"
                                            ))
                                            .color(egui::Color32::from_rgb(186, 108, 108)),
                                        );
                                        ui.label(local_stdio_host_diagnostics::failure_explanation(category));
                                        ui.collapsing("Bounded host failure detail · diagnostic only", |ui| {
                                            ui.label(egui::RichText::new(detail).monospace());
                                        });
                                    }
                                }
                                ui.label(
                                    egui::RichText::new(
                                        "Point-in-time observation only. No provider launched, journal written, or route permission consumed. Readiness cannot authorize an MCP call."
                                    )
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                            }
                            match replay_tool_provider_audit(&self.events) {
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "tool provider projection blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Ok(providers) => {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.tool_provider_name_draft,
                                            )
                                            .desired_width(180.0)
                                            .hint_text("Provider name"),
                                        );
                                        if ui
                                            .add_enabled(
                                                !self.tool_command_pending
                                                    && !self
                                                        .tool_provider_name_draft
                                                        .is_empty()
                                                    && self.persist_tx.is_some(),
                                                egui::Button::new("Register provider"),
                                            )
                                            .clicked()
                                        {
                                            self.register_tool_provider();
                                        }
                                    });

                                    if ui
                                        .button("Fill hello / time / shell MCP demo")
                                        .on_hover_text(
                                            "Prefills local drafts only. Does not install a server, register, activate, approve, or run anything.",
                                        )
                                        .clicked()
                                    {
                                        self.tool_provider_name_draft = "chatarium.demo".to_owned();
                                        self.tool_stdio_executable_draft =
                                            "/usr/bin/chatarium-mcp-demo".to_owned();
                                        self.tool_stdio_argv_draft.clear();
                                        self.tool_stdio_operations_draft =
                                            "hello\ntime.now\nshell.command\nchatarium.internal.tools-list"
                                                .to_owned();
                                        self.tool_operation_draft = "hello".to_owned();
                                        self.tool_arguments_draft = "{}".to_owned();
                                        self.status = "MCP demo fields prepared. Register a provider, bind its endpoint, configure and activate it, then manually Record / Allow / Run. The trusted /usr/bin demo binary must be installed separately.".to_owned();
                                    }

                                    if providers.is_empty() {
                                        ui.label(
                                            egui::RichText::new(
                                                "No external MCP providers registered. Built-in tools are available to MCP-enabled conversations.",
                                            )
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                        );
                                    } else {
                                        if self.tool_selected_provider.is_none()
                                            || !providers.iter().any(|provider| {
                                                Some(provider.provider_id)
                                                    == self.tool_selected_provider
                                            })
                                        {
                                            self.tool_selected_provider =
                                                Some(providers[0].provider_id);
                                        }

                                        let selected_text = self
                                            .tool_selected_provider
                                            .and_then(|provider_id| {
                                                providers.iter().find(|provider| {
                                                    provider.provider_id == provider_id
                                                })
                                            })
                                            .map(|provider| {
                                                format!(
                                                    "{} · provider {}",
                                                    provider.name.as_str(),
                                                    provider.provider_id.get()
                                                )
                                            })
                                            .unwrap_or_else(|| "Select provider".to_owned());

                                        egui::ComboBox::from_id_salt("tool-provider-select")
                                            .selected_text(selected_text)
                                            .show_ui(ui, |ui| {
                                                for provider in &providers {
                                                    ui.selectable_value(
                                                        &mut self.tool_selected_provider,
                                                        Some(provider.provider_id),
                                                        format!(
                                                            "{} · provider {}",
                                                            provider.name.as_str(),
                                                            provider.provider_id.get()
                                                        ),
                                                    );
                                                }
                                            });

                                        // Setup advice is observational: failed projections cannot
                                        // silently claim any permission or readiness.
                                        if let Some(selected) = self.tool_selected_provider.and_then(|id| {
                                            providers.iter().find(|provider| provider.provider_id == id)
                                        }) {
                                            let builtin = selected.name.as_str()
                                                == local_tool_adapter::BUILTIN_PROVIDER_NAME;
                                            let setup = if builtin {
                                                Some(mcp_provider_workflow::ProviderWorkflowFacts {
                                                    builtin: true,
                                                    endpoint_bound: selected.endpoint_binding.is_some(),
                                                    transport_configured: false,
                                                    activated: false,
                                                    source_addressable: false,
                                                    catalog_inspection_allowlisted: false,
                                                })
                                            } else {
                                                match (
                                                    replay_tool_transport_config_audit(&self.events),
                                                    replay_tool_provider_activation_audit(&self.events),
                                                    replay_local_routing_directory(&self.events),
                                                ) {
                                                    (Ok(configs), Ok(activations), Ok(directory)) => {
                                                        let config = configs.iter().find(|record| {
                                                            record.provider_id == selected.provider_id
                                                        });
                                                        Some(mcp_provider_workflow::ProviderWorkflowFacts {
                                                            builtin: false,
                                                            endpoint_bound: selected.endpoint_binding.is_some(),
                                                            transport_configured: config.is_some(),
                                                            activated: activations.iter().find(|record| {
                                                                record.provider_id == selected.provider_id
                                                            }).is_some_and(|record| record.active),
                                                            source_addressable: directory.iter().any(|entry| {
                                                                entry.conversation_id == self.local_conversation_id
                                                                    && entry.current_session_phase.accepts_ordinary_turns()
                                                            }),
                                                            catalog_inspection_allowlisted: config.is_some_and(|record| {
                                                                record.config.allowed_operations().iter().any(|operation| {
                                                                    operation.as_str() == MCP_LIST_TOOLS_OPERATION
                                                                })
                                                            }),
                                                        })
                                                    }
                                                    _ => None,
                                                }
                                            };
                                            ui.group(|ui| {
                                                ui.label(egui::RichText::new("Selected provider · setup guidance (read-only)").strong());
                                                if let Some(facts) = setup {
                                                    let step = mcp_provider_workflow::next_step(facts);
                                                    ui.label(egui::RichText::new(step.heading()).strong());
                                                    ui.label(step.instruction());
                                                    let note = mcp_provider_workflow::catalog_note(facts);
                                                    if !note.is_empty() {
                                                        ui.label(egui::RichText::new(note).small());
                                                    }
                                                } else {
                                                    ui.label("Setup guidance unavailable: a provider, activation, or routing audit could not be replayed. Review the detailed audit errors below.");
                                                }
                                                ui.label(egui::RichText::new(
                                                    "Guidance does not bind endpoints, activate providers, record calls, approve routes, or execute MCP tools."
                                                ).small());
                                            });
                                        }

                                        ui.collapsing("Provider registry", |ui| {
                                            for provider in &providers {
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{} · provider {}",
                                                            provider.name.as_str(),
                                                            provider.provider_id.get(),
                                                        ))
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                    match provider.endpoint_binding {
                                                        Some(binding) => {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "endpoint {} · registered",
                                                                    binding.endpoint_id().get()
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                        }
                                                        None => {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    "NOT ADDRESSABLE",
                                                                )
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                            if ui
                                                                .add_enabled(
                                                                    !self.tool_command_pending
                                                                        && self.persist_tx.is_some(),
                                                                    egui::Button::new(
                                                                        "Bind routing endpoint",
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                self.bind_tool_provider_endpoint(
                                                                    provider.provider_id,
                                                                );
                                                            }
                                                        }
                                                    }
                                                });
                                            }
                                        });

                                        ui.collapsing("External MCP stdio · configure / activate", |ui| {
                                            match (
                                                replay_tool_transport_config_audit(&self.events),
                                                replay_tool_provider_activation_audit(&self.events),
                                            ) {
                                                (Ok(configs), Ok(activations)) => {
                                                    if let Some(provider_id) = self.tool_selected_provider {
                                                        if let Some(provider) = providers.iter().find(|p| p.provider_id == provider_id) {
                                                            if provider.name.as_str() == local_tool_adapter::BUILTIN_PROVIDER_NAME {
                                                                ui.label("Local builtins hello and tools.list cannot use external transport.");
                                                            } else if provider.endpoint_binding.is_none() {
                                                                ui.label("Bind this provider's routing endpoint first.");
                                                            } else if let Some(configured) = configs.iter().find(|r| r.provider_id == provider_id) {
                                                                ui.label(egui::RichText::new(format!(
                                                                    "Executable: {}", configured.config.executable()
                                                                )).monospace());
                                                                ui.label(egui::RichText::new(format!(
                                                                    "argv: {:?}", configured.config.args()
                                                                )).monospace());
                                                                ui.label(egui::RichText::new(format!(
                                                                    "Allowlist: {}", configured.config.allowed_operations().iter()
                                                                        .map(|op| op.as_str()).collect::<Vec<_>>().join(", ")
                                                                )).monospace());
                                                                let active = activations.iter()
                                                                    .find(|r| r.provider_id == provider_id)
                                                                    .is_some_and(|r| r.active);
                                                                ui.label(if active {
                                                                    "ACTIVATED (individual calls still require Allow and one-shot Run)"
                                                                } else {
                                                                    "INACTIVE (execution disabled)"
                                                                });
                                                                if ui.add_enabled(
                                                                    !self.tool_command_pending && self.persist_tx.is_some(),
                                                                    egui::Button::new(if active {
                                                                        "Deactivate provider"
                                                                    } else {
                                                                        "Activate · inspect Linux executable"
                                                                    }),
                                                                ).on_hover_text(
                                                                    "Explicit user decision with Linux metadata inspection on the persistence worker. No process is spawned or tool call dispatched.",
                                                                ).clicked() {
                                                                    self.decide_stdio_provider_activation(
                                                                        provider_id,
                                                                        configured.configured_sequence,
                                                                        if active {
                                                                            ProviderActivationDecision::Deactivate
                                                                        } else {
                                                                            ProviderActivationDecision::Activate
                                                                        },
                                                                    );
                                                                }
                                                            } else {
                                                                ui.label("Configure immutable Linux stdio transport. No execution occurs.");
                                                                ui.label("Executable · absolute Linux path:");
                                                                ui.add(egui::TextEdit::singleline(
                                                                    &mut self.tool_stdio_executable_draft
                                                                ).desired_width(330.0).hint_text("/usr/bin/example-mcp"));
                                                                ui.label("argv · one exact argument per line (no shell):");
                                                                ui.add(egui::TextEdit::multiline(
                                                                    &mut self.tool_stdio_argv_draft
                                                                ).desired_width(330.0).desired_rows(2));
                                                                ui.label("Allowed operations · one name per line:");
                                                                ui.add(egui::TextEdit::multiline(
                                                                    &mut self.tool_stdio_operations_draft
                                                                ).desired_width(330.0).desired_rows(2).hint_text("search"));
                                                                if ui.add_enabled(
                                                                    !self.tool_command_pending
                                                                        && self.persist_tx.is_some()
                                                                        && !self.tool_stdio_executable_draft.is_empty()
                                                                        && !self.tool_stdio_operations_draft.is_empty(),
                                                                    egui::Button::new("Save inert stdio configuration"),
                                                                ).clicked() {
                                                                    self.configure_stdio_tool_provider(provider_id);
                                                                }
                                                                ui.label("One-shot immutable configuration; activation is a separate approval.");
                                                            }
                                                        }
                                                    }
                                                }
                                                (Err(error), _) | (_, Err(error)) => {
                                                    ui.label(format!("External provider audit blocked: {error}"));
                                                }
                                            }
                                        });

                                        let provider_addressable = self
                                            .tool_selected_provider
                                            .and_then(|provider_id| {
                                                providers.iter().find(|provider| {
                                                    provider.provider_id == provider_id
                                                })
                                            })
                                            .is_some_and(|provider| {
                                                provider.endpoint_binding.is_some()
                                            });
                                        let source_addressable =
                                            replay_local_routing_directory(&self.events)
                                                .map(|directory| {
                                                    directory.iter().any(|entry| {
                                                        entry.conversation_id
                                                            == self.local_conversation_id
                                                            && entry
                                                                .current_session_phase
                                                                .accepts_ordinary_turns()
                                                    })
                                                })
                                                .unwrap_or(false);

                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new("Immutable call intent").strong(),
                                        );
                                        ui.add(
                                            egui::TextEdit::singleline(
                                                &mut self.tool_operation_draft,
                                            )
                                            .desired_width(220.0)
                                            .hint_text("Operation name"),
                                        );
                                        ui.add(
                                            egui::TextEdit::multiline(
                                                &mut self.tool_arguments_draft,
                                            )
                                            .desired_width(360.0)
                                            .desired_rows(3)
                                            .hint_text(
                                                "Exact JSON arguments or legacy <tool_call> envelope",
                                            ),
                                        );
                                        let list_allowed = self
                                            .tool_selected_provider
                                            .and_then(|id| {
                                                replay_tool_transport_config_audit(&self.events)
                                                    .ok()
                                                    .and_then(|configs| {
                                                        configs.into_iter().find(|cfg| {
                                                            cfg.provider_id == id
                                                        })
                                                    })
                                            })
                                            .is_some_and(|cfg| {
                                                cfg.config.allowed_operations().iter().any(|op| {
                                                    op.as_str() == MCP_LIST_TOOLS_OPERATION
                                                })
                                            });
                                        if ui
                                            .add_enabled(
                                                list_allowed && !self.tool_command_pending,
                                                egui::Button::new("Prepare approved tools/list inspection"),
                                            )
                                            .on_hover_text(
                                                "Only prepares an exact MCP 2026 tools/list request. This provider must explicitly allowlist chatarium.internal.tools-list. Record, Allow and Run are still separate user actions. No provider is executed by this button.",
                                            )
                                            .clicked()
                                        {
                                            self.tool_operation_draft =
                                                MCP_LIST_TOOLS_OPERATION.to_owned();
                                            self.tool_arguments_draft = "{}".to_owned();
                                            self.status =
                                                "tools/list draft prepared; record, Allow and Run separately".to_owned();
                                        }
                                        if self.tool_arguments_draft.trim_start().starts_with("<tool_call")
                                            && ui
                                                .add_enabled(
                                                    !self.tool_command_pending,
                                                    egui::Button::new("Read legacy envelope"),
                                                )
                                                .on_hover_text(
                                                    "Validate the recovered XML-like tool call and fill its operation name. Keeps exact original text. No durable call or execution occurs.",
                                                )
                                                .clicked()
                                        {
                                            match parse_legacy_tool_call(&self.tool_arguments_draft) {
                                                Ok(parsed) => {
                                                    self.tool_operation_draft = parsed.name;
                                                    self.status = format!(
                                                        "legacy tool envelope {} parsed · no call recorded or executed",
                                                        parsed.id,
                                                    );
                                                }
                                                Err(error) => {
                                                    self.status = format!(
                                                        "legacy tool envelope rejected: {error:?}",
                                                    );
                                                }
                                            }
                                        }
                                        if ui
                                            .add_enabled(
                                                !self.tool_command_pending
                                                    && provider_addressable
                                                    && source_addressable
                                                    && !self.tool_operation_draft.is_empty()
                                                    && self.persist_tx.is_some(),
                                                egui::Button::new(
                                                    "Record call + propose approval route",
                                                ),
                                            )
                                            .on_hover_text(
                                                "Records immutable local intent and a RequireApproval ToolCall route. Does not execute the tool.",
                                            )
                                            .clicked()
                                        {
                                            self.propose_tool_call();
                                        }
                                        if !source_addressable {
                                            ui.label(
                                                egui::RichText::new(
                                                    "Current conversation needs an active routing endpoint before it can originate a tool call.",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(139, 143, 153)),
                                            );
                                        }
                                    }

                                    ui.add_space(5.0);
                                    ui.label(
                                        egui::RichText::new("Tool call audit").strong().size(10.0),
                                    );
                                    match replay_tool_call_audit(&self.events) {
                                        Err(error) => {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "tool call projection blocked: {error}"
                                                ))
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(186, 108, 108)),
                                            );
                                        }
                                        Ok(calls) if calls.is_empty() => {
                                            ui.label(
                                                egui::RichText::new(
                                                    "no immutable tool calls recorded",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(139, 143, 153)),
                                            );
                                        }
                                        Ok(calls) => {
                                            match replay_unresolved_external_tool_dispatches(&self.events) {
                                                Ok(unresolved) if !unresolved.is_empty() => {
                                                    ui.collapsing(
                                                        format!("UNRESOLVED EXTERNAL DISPATCHES · {}", unresolved.len()),
                                                        |ui| {
                                                            ui.label(
                                                                "A route permit was durably consumed, but no tool outcome is recorded. A process may or may not have run. No automatic retries.",
                                                            );
                                                            for item in &unresolved {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "call {} · route {} · provider {} · dispatched #{}",
                                                                        item.call_id.get(),
                                                                        item.route_id.get(),
                                                                        item.provider_id.get(),
                                                                        item.dispatch_sequence,
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                            }
                                                        },
                                                    );
                                                }
                                                Err(error) => {
                                                    ui.label(format!(
                                                        "Unresolved tool dispatch audit blocked: {error}"
                                                    ));
                                                }
                                                Ok(_) => {}
                                            }
                                            let routes =
                                                replay_routing_audit(&self.events).ok();
                                            let outcomes =
                                                match replay_tool_call_outcome_audit(&self.events) {
                                                    Ok(outcomes) => Some(outcomes),
                                                    Err(error) => {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "tool outcome projection blocked: {error}"
                                                            ))
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            )),
                                                        );
                                                        None
                                                    }
                                                };
                                            let tool_context_records =
                                                match replay_tool_result_context_audit(&self.events) {
                                                    Ok(records) => Some(records),
                                                    Err(error) => {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "tool result context projection blocked: {error}"
                                                            ))
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            )),
                                                        );
                                                        None
                                                    }
                                                };
                                            // Focus is a read-only projection of durable records.
                                            // Execution is still guarded independently by the
                                            // original approval, activation and sandbox gates.
                                            if let (Some(routes), Some(outcomes)) =
                                                (&routes, &outcomes)
                                            {
                                                // Build bounded per-frame lookups once. The
                                                // focus must not compare every call to every
                                                // historical outcome while rendering.
                                                let indexed_routes: BTreeMap<_, _> = routes
                                                    .iter()
                                                    .map(|route| (route.request.id, route))
                                                    .collect();
                                                let observed_calls: BTreeSet<_> = outcomes
                                                    .iter()
                                                    .map(|outcome| outcome.call_id)
                                                    .collect();
                                                let next_review = calls.iter().filter(
                                                    |call| self.tool_selected_provider == Some(call.provider_id),
                                                ).filter_map(|call| {
                                                    let route_id = call.route_id?;
                                                    let route = indexed_routes.get(&route_id).copied()?;
                                                    let observed = observed_calls.contains(&call.call_id);
                                                    let stage = mcp_call_review::classify(
                                                        call.route_bound_sequence.is_some(),
                                                        route.latest_user_decision,
                                                        route.dispatch_sequence.is_some(),
                                                        observed,
                                                    );
                                                    stage.actionable().then_some((call, route, stage))
                                                }).max_by_key(|(call, _, _)| call.recorded_sequence);
                                                ui.group(|ui| {
                                                    ui.label(egui::RichText::new(
                                                        "NEXT MANUAL MCP CALL REVIEW"
                                                    ).strong());
                                                    if let Some((call, route, stage)) = next_review {
                                                        ui.label(format!(
                                                            "Call {} · route {} · {} · {}",
                                                            call.call_id.get(),
                                                            route.request.id.get(),
                                                            call.operation.as_str(),
                                                            stage.label(),
                                                        ));
                                                        ui.label(format!(
                                                            "Provider {} · source session {} · recorded #{}",
                                                            call.provider_id.get(),
                                                            call.source_session_id.get(),
                                                            call.recorded_sequence,
                                                        ));
                                                        ui.collapsing(
                                                            format!("Inspect exact recorded intent · {} bytes", call.arguments_text.len()),
                                                            |ui| {
                                                                ui.label(egui::RichText::new(call.arguments_text.as_str()).monospace());
                                                            },
                                                        );
                                                        match stage {
                                                            mcp_call_review::CallReviewStage::AwaitingDecision => {
                                                                ui.label("Review the immutable intent. Allow does not run the tool; execution needs a separate deliberate action.");
                                                                let enabled = !self.tool_command_pending
                                                                    && self.persist_tx.is_some()
                                                                    && !route.gate_state.is_dispatched();
                                                                ui.horizontal(|ui| {
                                                                    if ui.add_enabled(
                                                                        enabled, egui::Button::new("Allow this route")
                                                                    ).clicked() {
                                                                        self.decide_tool_call_route(
                                                                            route.request.id, RouteUserDecision::Allow
                                                                        );
                                                                    }
                                                                    if ui.add_enabled(
                                                                        enabled, egui::Button::new("Deny this route")
                                                                    ).clicked() {
                                                                        self.decide_tool_call_route(
                                                                            route.request.id, RouteUserDecision::Deny
                                                                        );
                                                                    }
                                                                });
                                                            }
                                                            mcp_call_review::CallReviewStage::ApprovedForSeparateReview => {
                                                                let builtin_hello = providers.iter().find(|provider| {
                                                                    provider.provider_id == call.provider_id
                                                                }).is_some_and(|provider| {
                                                                    local_tool_adapter::supports_builtin(
                                                                        provider.name.as_str(),
                                                                        call.operation.as_str(),
                                                                    )
                                                                });
                                                                if builtin_hello {
                                                                    ui.label(if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                                        "Host shell runs as your Linux user, using your real home and PATH. This is NOT sandboxed."
                                                                    } else {
                                                                        "Approved local builtin. Run separately."
                                                                    });
                                                                    if ui.add_enabled(
                                                                        matches!(
                                                                            route.gate_state,
                                                                            RouteGateState::Allowed {
                                                                                by: DecisionAuthority::User,
                                                                            }
                                                                        )
                                                                            && !self.tool_command_pending
                                                                            && self.persist_tx.is_some(),
                                                                        egui::Button::new(if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                                            "Run approved host shell · once"
                                                                        } else {
                                                                            "Run approved local builtin · once"
                                                                        }),
                                                                    ).clicked() {
                                                                        if call.operation.as_str() == host_shell_mcp::OPERATION {
                                                                            self.dispatch_host_shell(route.request.id);
                                                                        } else {
                                                                            self.dispatch_builtin_hello(route.request.id);
                                                                        }
                                                                    }
                                                                } else if providers.iter().find(|provider| {
                                                                    provider.provider_id == call.provider_id
                                                                }).is_some_and(|provider| local_http_mcp::allowed_operation(
                                                                    provider.name.as_str(), call.operation.as_str()
                                                                )) {
                                                                    ui.label("Fixed loopback Streamable HTTP MCP. An approved call uses the shared local server, not the network-isolated stdio sandbox. It may perform side effects (Mathematica executes Wolfram code).");
                                                                    if ui.add_enabled(
                                                                        matches!(route.gate_state, RouteGateState::Allowed { by: DecisionAuthority::User })
                                                                            && !self.tool_command_pending && self.persist_tx.is_some(),
                                                                        egui::Button::new("Run approved localhost MCP call · once"),
                                                                    ).clicked() {
                                                                        self.dispatch_local_http_mcp(route.request.id);
                                                                    }
                                                                } else {
                                                                    ui.label("User Allow is recorded. Inspect the exact wire request before a separate one-shot Run.");
                                                                    ui.collapsing("Review approved wire request and choose Run", |ui| {
                                                                    match preview_activated_stdio_tool_invocation(
                                                                        &self.events, call.call_id
                                                                    ) {
                                                                        Ok(preview) => {
                                                                            ui.label(format!(
                                                                                "Activation #{} · route {}",
                                                                                preview.activation_sequence,
                                                                                preview.invocation.route_id.get(),
                                                                            ));
                                                                            ui.label(egui::RichText::new(
                                                                                preview.invocation.request_frame.as_str()
                                                                            ).monospace());
                                                                            match local_stdio_runner::plan_confined_stdio_launch(
                                                                                preview.invocation.executable.as_str(),
                                                                                &preview.invocation.argv,
                                                                            ) {
                                                                                Ok(_) => {
                                                                                    ui.label("Restricted network-isolated sandbox. Run permanently consumes this approved route; no automatic retries.");
                                                                                    if ui.add_enabled(
                                                                                        cfg!(target_os = "linux")
                                                                                            && !self.tool_command_pending
                                                                                            && self.persist_tx.is_some(),
                                                                                        egui::Button::new("Run approved MCP call · one shot"),
                                                                                    ).clicked() {
                                                                                        self.execute_approved_tool(call.call_id);
                                                                                    }
                                                                                }
                                                                                Err(error) => {
                                                                                    ui.label(format!("Sandbox plan blocked: {error}"));
                                                                                }
                                                                            }
                                                                        }
                                                                        Err(error) => {
                                                                            ui.label(format!("Run not eligible: {error}"));
                                                                        }
                                                                    }
                                                                    });
                                                                }
                                                            }
                                                            _ => {}
                                                        }
                                                    } else {
                                                        ui.label("No unconsumed calls await review for the selected provider.");
                                                    }
                                                    ui.label(egui::RichText::new(
                                                        "This focus grants no permission. Allow/Deny and Run remain separate explicit actions."
                                                    ).small());
                                                });
                                            } else {
                                                ui.label("Call review focus unavailable: route or outcome audit blocked.");
                                            }

                                            // Terminal evidence is independently reviewed for
                                            // context admission, never for execution authority.
                                            if let (Some(outcomes), Some(context_records)) =
                                                (&outcomes, &tool_context_records)
                                            {
                                                if let Some(provider_id) = self.tool_selected_provider {
                                                    ui.collapsing("RECENT MCP RESULTS · separate context review", |ui| {
                                                        match mcp_result_review::recent_result_queue(
                                                            outcomes,
                                                            context_records,
                                                            provider_id,
                                                            self.local_conversation_id,
                                                            |outcome| tool_outcome_owning_conversation(&self.events, outcome),
                                                        ) {
                                                            Ok(queue) if !queue.is_empty() => {
                                                                if let Some((call_id, decision)) =
                                                                    mcp_result_review::render_result_queue(
                                                                        ui,
                                                                        &queue,
                                                                        &calls,
                                                                        !self.tool_command_pending && self.persist_tx.is_some(),
                                                                    )
                                                                {
                                                                    self.decide_tool_result_context(call_id, decision);
                                                                }
                                                            }
                                                            Ok(_) => {
                                                                ui.label("No results owned by this conversation among the 16 most recent observations for the selected provider. Historical audit remains available.");
                                                            }
                                                            Err(error) => {
                                                                ui.label(format!(
                                                                    "Result review blocked by historical ownership or audit evidence: {error}"
                                                                ));
                                                            }
                                                        }
                                                    });
                                                }
                                            } else {
                                                ui.label("Result review unavailable: outcome or context-decision audit cannot be replayed.");
                                            }

                                            egui::ScrollArea::vertical()
                                                .id_salt("tool-call-audit")
                                                .max_height(240.0)
                                                .show(ui, |ui| {
                                                    for call in calls.iter().rev() {
                                                        let provider_label = providers
                                                            .iter()
                                                            .find(|provider| {
                                                                provider.provider_id
                                                                    == call.provider_id
                                                            })
                                                            .map(|provider| {
                                                                provider.name.as_str().to_owned()
                                                            })
                                                            .unwrap_or_else(|| {
                                                                format!(
                                                                    "provider {}",
                                                                    call.provider_id.get()
                                                                )
                                                            });
                                                        let route = call.route_id.and_then(
                                                            |route_id| {
                                                                routes.as_ref().and_then(|routes| {
                                                                    routes.iter().find(|route| {
                                                                        route.request.id == route_id
                                                                    })
                                                                })
                                                            },
                                                        );

                                                        ui.group(|ui| {
                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "CALL {} · {} · {} · source session {}",
                                                                        call.call_id.get(),
                                                                        provider_label,
                                                                        call.operation.as_str(),
                                                                        call.source_session_id.get(),
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                                if let Some(route) = route {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            format!(
                                                                                "route {} · {}",
                                                                                route.request.id.get(),
                                                                                route_gate_state_label(
                                                                                    route.gate_state,
                                                                                ),
                                                                            ),
                                                                        )
                                                                        .monospace()
                                                                        .size(9.0),
                                                                    );

                                                                    if local_http_mcp::allowed_operation(provider_label.as_str(), call.operation.as_str())
                                                                        && route.dispatch_sequence.is_none()
                                                                    {
                                                                        ui.label("Known localhost MCP service · exact approval still required.");
                                                                        if ui.add_enabled(
                                                                            matches!(route.gate_state, RouteGateState::Allowed { by: DecisionAuthority::User })
                                                                                && !self.tool_command_pending && self.persist_tx.is_some(),
                                                                            egui::Button::new("Run local HTTP MCP · one shot"),
                                                                        ).clicked() {
                                                                            self.dispatch_local_http_mcp(route.request.id);
                                                                        }
                                                                    }
                                                                    if !local_tool_adapter::supports_hello(
                                                                        provider_label.as_str(),
                                                                        call.operation.as_str(),
                                                                    ) && !local_http_mcp::known_provider(provider_label.as_str())
                                                                        && route.dispatch_sequence.is_none() {
                                                                        ui.collapsing("Activation-aware MCP review and Run", |ui| {
                                                                            match preview_activated_stdio_tool_invocation(&self.events, call.call_id) {
                                                                                Ok(preview) => {
                                                                                    ui.label(format!(
                                                                                        "Active provider · activation #{} · approved route {}",
                                                                                        preview.activation_sequence,
                                                                                        preview.invocation.route_id.get(),
                                                                                    ));
                                                                                    ui.label(egui::RichText::new(
                                                                                        preview.invocation.request_frame.as_str()
                                                                                    ).monospace());
                                                                                    ui.label("Preview alone grants NO execution authority.");
                                                                                    match local_stdio_runner::plan_confined_stdio_launch(
                                                                                        preview.invocation.executable.as_str(),
                                                                                        &preview.invocation.argv,
                                                                                    ) {
                                                                                        Ok(_) => {
                                                                                            ui.label(
                                                                                                "One-shot confinement: private network, no home, read-only /usr, 32 MiB scratch, 10s wall-time, 8s CPU, 512 MiB memory. Requires root-owned /usr/bin executable and installed bubblewrap + prlimit.",
                                                                                            );
                                                                                            if ui.add_enabled(
                                                                                                cfg!(target_os = "linux")
                                                                                                    && !self.tool_command_pending
                                                                                                    && self.persist_tx.is_some(),
                                                                                                egui::Button::new("Run sandboxed MCP tool · one shot"),
                                                                                            ).on_hover_text(
                                                                                                "Separate deliberate execution after provider activation and explicit ToolCall Allow. Consumes the route permanently before a sandboxed process can start. No automatic retries.",
                                                                                            ).clicked() {
                                                                                                self.execute_approved_tool(call.call_id);
                                                                                            }
                                                                                        }
                                                                                        Err(reason) => {
                                                                                            ui.label(format!("Sandbox execution unavailable: {reason}"));
                                                                                        }
                                                                                    }
                                                                                }
                                                                                Err(error) => { ui.label(format!("Not eligible: {error}")); }
                                                                            }
                                                                        });
                                                                    }
                                                                    let can_decide =
                                                                        !self.tool_command_pending
                                                                            && !route
                                                                                .gate_state
                                                                                .is_dispatched()
                                                                            && self
                                                                                .persist_tx
                                                                                .is_some();
                                                                    if ui
                                                                        .add_enabled(
                                                                            can_decide
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
                                                                        self.decide_tool_call_route(
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
                                                                        self.decide_tool_call_route(
                                                                            route.request.id,
                                                                            RouteUserDecision::Deny,
                                                                        );
                                                                    }
                                                                    if ui
                                                                        .add_enabled(
                                                                            local_tool_adapter::supports_hello(
                                                                                provider_label.as_str(),
                                                                                call.operation.as_str(),
                                                                            )
                                                                                && matches!(
                                                                                    route.gate_state,
                                                                                    RouteGateState::Allowed {
                                                                                        by: DecisionAuthority::User,
                                                                                    }
                                                                                )
                                                                                && !self.tool_command_pending
                                                                                && self.persist_tx.is_some(),
                                                                            egui::Button::new(
                                                                                "Run local hello",
                                                                            ),
                                                                        )
                                                                        .on_hover_text(
                                                                            "Side-effect-free builtin hello only. Requires explicit Allow and consumes the route once. No shell, file, network, or arbitrary MCP execution.",
                                                                        )
                                                                        .clicked()
                                                                    {
                                                                        self.dispatch_builtin_hello(
                                                                            route.request.id,
                                                                        );
                                                                    }
                                                                } else {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            "UNBOUND CALL INTENT",
                                                                        )
                                                                        .monospace()
                                                                        .size(9.0),
                                                                    );
                                                                }
                                                            });
                                                            ui.collapsing(
                                                                format!(
                                                                    "Exact opaque arguments · {} bytes",
                                                                    call.arguments_text.len()
                                                                ),
                                                                |ui| {
                                                                    if call.arguments_text.is_empty()
                                                                    {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                "<empty>",
                                                                            )
                                                                            .monospace()
                                                                            .size(9.0),
                                                                        );
                                                                    } else {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                call.arguments_text
                                                                                    .as_str(),
                                                                            )
                                                                            .size(10.0),
                                                                        );
                                                                    }
                                                                },
                                                            );
                                                            if let Some(outcome) = outcomes
                                                                .as_ref()
                                                                .and_then(|outcomes| {
                                                                    outcomes.iter().find(|outcome| {
                                                                        outcome.call_id == call.call_id
                                                                    })
                                                                })
                                                            {
                                                                ui.collapsing(
                                                                    format!(
                                                                        "ADAPTER {} · route {} · dispatched #{} · observed #{} · {} bytes",
                                                                        outcome.kind.stable_name(),
                                                                        outcome.route_id.get(),
                                                                        outcome.dispatch_sequence,
                                                                        outcome.observed_sequence,
                                                                        outcome.text.len(),
                                                                    ),
                                                                    |ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                outcome.text.as_str(),
                                                                            )
                                                                            .size(10.0),
                                                                        );
                                                                    },
                                                                );
                                                                if call.operation.as_str() == MCP_LIST_TOOLS_OPERATION
                                                                    && outcome.kind == ToolCallOutcomeKind::Result
                                                                {
                                                                    ui.collapsing("Provider-advertised tools · untrusted inspection", |ui| {
                                                                        match decode_stdio_response(&outcome.text, call.call_id.get()) {
                                                                            Ok(McpResponse::Complete(result)) => match parse_tools_list_page(&result) {
                                                                                Ok(page) => {
                                                                                    ui.label(format!("{} advertised tools in this page; no permissions or allowlist changes", page.tools.len()));
                                                                                    let inspection_owned_here =
                                                                                        tool_outcome_owning_conversation(&self.events, outcome)
                                                                                            .ok()
                                                                                            .flatten()
                                                                                            == Some(self.local_conversation_id);
                                                                                    let source_accepts_calls =
                                                                                        replay_local_routing_directory(&self.events)
                                                                                            .ok()
                                                                                            .is_some_and(|directory| {
                                                                                                directory.iter().any(|entry| {
                                                                                                    entry.conversation_id == self.local_conversation_id
                                                                                                        && entry.current_session_phase.accepts_ordinary_turns()
                                                                                                })
                                                                                            });
                                                                                    let currently_active =
                                                                                        replay_tool_provider_activation_audit(&self.events)
                                                                                            .ok()
                                                                                            .is_some_and(|records| {
                                                                                                records.iter().any(|record| {
                                                                                                    record.provider_id == call.provider_id
                                                                                                        && record.active
                                                                                                })
                                                                                            });
                                                                                    let configured_operations =
                                                                                        replay_tool_transport_config_audit(&self.events)
                                                                                            .ok()
                                                                                            .and_then(|records| {
                                                                                                records
                                                                                                    .into_iter()
                                                                                                    .find(|record| record.provider_id == call.provider_id)
                                                                                            })
                                                                                            .map(|record| {
                                                                                                record.config.allowed_operations()
                                                                                                    .iter()
                                                                                                    .map(|operation| operation.as_str().to_owned())
                                                                                                    .collect::<BTreeSet<_>>()
                                                                                            })
                                                                                            .unwrap_or_default();
                                                                                    for advertised in &page.tools {
                                                                                        ui.label(egui::RichText::new(format!(
                                                                                            "{} · {}",
                                                                                            advertised.name,
                                                                                            advertised.title.as_deref().unwrap_or("unnamed tool"),
                                                                                        )).monospace());
                                                                                        if let Some(description) = &advertised.description {
                                                                                            ui.label(description.as_str());
                                                                                        }
                                                                                        mcp_output_audit::render_catalog_output_schema(ui, advertised);
                                                                                        ui.collapsing(
                                                                                            format!("Review required-field JSON draft · {}", advertised.name),
                                                                                            |ui| {
                                                                                                match draft_required_arguments(&advertised.input_schema) {
                                                                                                    Ok(draft) => {
                                                                                                        ui.label("Only required properties are included. Placeholders are NOT validated values; edit and review before recording.");
                                                                                                        ui.label(egui::RichText::new(draft.arguments_json.as_str()).monospace());
                                                                                                        for note in &draft.review_notes {
                                                                                                            ui.label(note.as_str());
                                                                                                        }
                                                                                                        let allowed_operation =
                                                                                                            ToolOperationName::new(advertised.name.clone())
                                                                                                                .is_ok()
                                                                                                                && configured_operations.contains(advertised.name.as_str());
                                                                                                        let can_prepare = inspection_owned_here
                                                                                                            && source_accepts_calls
                                                                                                            && currently_active
                                                                                                            && allowed_operation
                                                                                                            && !self.tool_command_pending;
                                                                                                        if ui.add_enabled(
                                                                                                            can_prepare,
                                                                                                            egui::Button::new("Fill editable tool-call draft · no execution"),
                                                                                                        ).on_hover_text(
                                                                                                            "Copies only bounded required-field placeholders into the local draft. Requires same owning conversation, current provider activation, and an existing immutable operation allowlist entry. Record, Allow and Run remain separate.",
                                                                                                        ).clicked() {
                                                                                                            self.tool_selected_provider = Some(call.provider_id);
                                                                                                            self.tool_operation_draft = advertised.name.clone();
                                                                                                            self.tool_arguments_draft = draft.arguments_json;
                                                                                                            self.status = format!(
                                                                                                                "drafted {} from untrusted catalog; review {} placeholders, then Record / Allow / Run separately",
                                                                                                                advertised.name,
                                                                                                                draft.review_notes.len(),
                                                                                                            );
                                                                                                        }
                                                                                                        if !allowed_operation {
                                                                                                            ui.label("Unavailable: this tool is not in the provider's immutable allowed operations.");
                                                                                                        } else if !inspection_owned_here || !source_accepts_calls {
                                                                                                            ui.label("Unavailable: catalog and active source conversation must match.");
                                                                                                        } else if !currently_active {
                                                                                                            ui.label("Unavailable: provider is not currently active.");
                                                                                                        }
                                                                                                    }
                                                                                                    Err(reason) => {
                                                                                                        ui.label(format!("Cannot construct bounded JSON draft: {reason}"));
                                                                                                        ui.label("You may still manually author an exact JSON object for a separately allowlisted operation.");
                                                                                                    }
                                                                                                }
                                                                                            },
                                                                                        );
                                                                                    }
                                                                                    if let Some(cursor) = &page.next_cursor {
                                                                                        ui.label("Another page is advertised; continuation is NEVER automatic.");
                                                                                        if ui.add_enabled(
                                                                                            !self.tool_command_pending,
                                                                                            egui::Button::new("Prepare next catalog page"),
                                                                                        ).on_hover_text(
                                                                                            "Only fills a new immutable call draft for the same provider. It does not dispatch or grant permission.",
                                                                                        ).clicked() {
                                                                                            self.tool_selected_provider = Some(call.provider_id);
                                                                                            self.tool_operation_draft = MCP_LIST_TOOLS_OPERATION.to_owned();
                                                                                            self.tool_arguments_draft = serde_json::json!({"cursor":cursor}).to_string();
                                                                                            self.status = "next catalog page drafted; requires a new approval and Run".to_owned();
                                                                                        }
                                                                                    }
                                                                                }
                                                                                Err(error) => {
                                                                                    ui.label(format!("Invalid catalog result: {error:?}"));
                                                                                }
                                                                            },
                                                                            _ => {
                                                                                ui.label("Catalog result cannot be decoded.");
                                                                            }
                                                                        }
                                                                    });
                                                                }
                                                                mcp_output_audit::render_snapshot_comparisons(
                                                                    ui,
                                                                    &self.events,
                                                                    &calls,
                                                                    outcomes.as_deref().unwrap_or(&[]),
                                                                    call,
                                                                    outcome,
                                                                    self.local_conversation_id,
                                                                );
                                                                if let Some(records) = &tool_context_records {
                                                                    match tool_outcome_owning_conversation(&self.events, outcome) {
                                                                        Ok(Some(owner))
                                                                            if owner == self.local_conversation_id =>
                                                                        {
                                                                            let latest = records
                                                                                .iter()
                                                                                .find(|record| record.call_id == call.call_id)
                                                                                .map(|record| record.decision);
                                                                            ui.horizontal_wrapped(|ui| {
                                                                                ui.label(
                                                                                    egui::RichText::new(
                                                                                        match latest {
                                                                                            Some(ToolResultContextDecision::Admit) => "CONTEXT: ADMITTED",
                                                                                            Some(ToolResultContextDecision::Exclude) => "CONTEXT: EXCLUDED",
                                                                                            None => "CONTEXT: EXCLUDED · DEFAULT",
                                                                                        }
                                                                                    )
                                                                                    .monospace()
                                                                                    .size(9.0),
                                                                                );
                                                                                let can_change =
                                                                                    !self.tool_command_pending
                                                                                    && self.persist_tx.is_some();
                                                                                if ui.add_enabled(
                                                                                    can_change
                                                                                        && outcome.text.len() <= MAX_CONTEXT_TOOL_RESULT_BYTES
                                                                                        && latest != Some(ToolResultContextDecision::Admit),
                                                                                    egui::Button::new("Admit result"),
                                                                                ).clicked() {
                                                                                    self.decide_tool_result_context(
                                                                                        call.call_id,
                                                                                        ToolResultContextDecision::Admit,
                                                                                    );
                                                                                }
                                                                                if ui.add_enabled(
                                                                                    can_change
                                                                                        && latest == Some(ToolResultContextDecision::Admit),
                                                                                    egui::Button::new("Exclude result"),
                                                                                ).clicked() {
                                                                                    self.decide_tool_result_context(
                                                                                        call.call_id,
                                                                                        ToolResultContextDecision::Exclude,
                                                                                    );
                                                                                }
                                                                            });
                                                                            if outcome.text.len() > MAX_CONTEXT_TOOL_RESULT_BYTES {
                                                                                ui.label("Result is too large for context admission; exact audit remains available.");
                                                                            }
                                                                        }
                                                                        Ok(Some(_)) => {
                                                                            ui.label("Tool result belongs to another local conversation.");
                                                                        }
                                                                        Ok(None) => {
                                                                            ui.label("Tool result has no local conversation owner; context admission disabled.");
                                                                        }
                                                                        Err(error) => {
                                                                            ui.label(format!("tool result ownership blocked: {error}"));
                                                                        }
                                                                    }
                                                                }
                                                            } else if route.is_some_and(|route| {
                                                                route.gate_state.is_dispatched()
                                                            }) {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        "DISPATCHED · NO OBSERVED ADAPTER OUTCOME",
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                            }
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    if local_tool_adapter::supports_hello(provider_label.as_str(), call.operation.as_str()) {
                                                                        "LOCAL HELLO ONLY · explicit Allow and separate one-shot Run required."
                                                                    } else {
                                                                        "EXECUTION DISABLED · no general MCP or external tool adapter is attached."
                                                                    },
                                                                )
                                                                .monospace()
                                                                .size(9.0)
                                                                .color(
                                                                    egui::Color32::from_rgb(
                                                                        139, 143, 153,
                                                                    ),
                                                                ),
                                                            );
                                                        });
                                                    }
                                                });
                                        }
                                    }
                                }
                            }

                            if self.tool_command_pending {
                                ui.spinner();
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
                                match local_worker_session_alignment(
                                    &self.events,
                                    self.local_conversation_id,
                                    binding.worker_id,
                                ) {
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "worker session projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(186, 108, 108)),
                                        );
                                    }
                                    Ok((current_session_id, active_session_id)) => {
                                        ui.horizontal_wrapped(|ui| {
                                            match active_session_id {
                                                None => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "worker session · UNBOUND · current {}",
                                                            current_session_id.get(),
                                                        ))
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                    if ui
                                                        .add_enabled(
                                                            !self.supervision_command_pending
                                                                && self.persist_tx.is_some(),
                                                            egui::Button::new(
                                                                "Bind worker to current session",
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.bind_worker_to_current_session(
                                                            binding.worker_id,
                                                        );
                                                    }
                                                }
                                                Some(active)
                                                    if active == current_session_id =>
                                                {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "worker session {} · ACTIVE",
                                                            active.get(),
                                                        ))
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                }
                                                Some(active) => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "worker session {} → current {} · HANDOFF REQUIRED",
                                                            active.get(),
                                                            current_session_id.get(),
                                                        ))
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                    if ui
                                                        .add_enabled(
                                                            !self.supervision_command_pending
                                                                && self.persist_tx.is_some(),
                                                            egui::Button::new(
                                                                "Advance worker to current session",
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.bind_worker_to_current_session(
                                                            binding.worker_id,
                                                        );
                                                    }
                                                }
                                            }
                                            if self.supervision_command_pending {
                                                ui.spinner();
                                            }
                                        });
                                    }
                                }

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

                        ui.collapsing("Worker control inbox", |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "Delivered controller controls are durable command provenance only. Acknowledge records that this worker conversation saw the command. STATUS snapshots existing lifecycle without mutation. START/RESUME and STOP mutate lifecycle only through an explicit crash-recoverable Apply control action. CONTINUE starts its own non-authored execution turn; remote inference is not dispatched by that start yet.",
                                )
                                .size(9.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            match replay_worker_control_inbox_for_conversation(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "worker control inbox projection blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                                Ok(items) if items.is_empty() => {
                                    ui.label(
                                        egui::RichText::new("no delivered worker controls")
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                }
                                Ok(items) => {
                                    let status_results =
                                        match replay_worker_control_status_results(&self.events) {
                                            Ok(results) => Some(results),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "worker status result projection blocked: {error}"
                                                    ))
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        186, 108, 108,
                                                    )),
                                                );
                                                None
                                            }
                                        };
                                    let action_records =
                                        match replay_worker_control_action_audit(&self.events) {
                                            Ok(records) => Some(records),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "worker control action projection blocked: {error}"
                                                    ))
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        186, 108, 108,
                                                    )),
                                                );
                                                None
                                            }
                                        };
                                    let continuation_executions =
                                        match replay_worker_continuation_execution_audit(
                                            &self.events,
                                        ) {
                                            Ok(records) => Some(records),
                                            Err(error) => {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "continuation execution projection blocked: {error}"
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
                                        .id_salt("worker-control-inbox")
                                        .max_height(260.0)
                                        .show(ui, |ui| {
                                            for item in items {
                                                ui.group(|ui| {
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "{} · control {} · goal {}",
                                                                worker_control_kind_label(item.kind),
                                                                item.control_id.get(),
                                                                item.goal_id.get(),
                                                            ))
                                                            .strong()
                                                            .size(10.0),
                                                        );
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "worker {} · worker session {} · controller session {} · route {} · dispatched #{} · delivered #{}",
                                                                item.worker_id.get(),
                                                                item.worker_session_id.get(),
                                                                item.controller_session_id.get(),
                                                                item.route_id.get(),
                                                                item.dispatch_sequence,
                                                                item.delivered_sequence,
                                                            ))
                                                            .monospace()
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                139, 143, 153,
                                                            )),
                                                        );
                                                        if let Some(sequence) =
                                                            item.acknowledged_sequence
                                                        {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "ACKNOWLEDGED · event #{sequence}"
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                        } else if ui
                                                            .add_enabled(
                                                                !self
                                                                    .worker_control_command_pending
                                                                    && self.persist_tx.is_some(),
                                                                egui::Button::new("Acknowledge"),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.acknowledge_worker_control(
                                                                item.route_id,
                                                            );
                                                        }

                                                        if item.kind
                                                            == WorkerControlKind::StatusRequest
                                                            && item.acknowledged_sequence.is_some()
                                                        {
                                                            let status_result = status_results
                                                                .as_ref()
                                                                .and_then(|results| {
                                                                    results.iter().find(
                                                                        |record| {
                                                                            record.route_id
                                                                                == item.route_id
                                                                        },
                                                                    )
                                                                });
                                                            if let Some(result) = status_result {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "STATUS RESULT · {} · goal {} · event #{}",
                                                                        worker_phase_label(
                                                                            result.phase,
                                                                        ),
                                                                        result.goal_id.get(),
                                                                        result.recorded_sequence,
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                            } else if ui
                                                                .add_enabled(
                                                                    status_results.is_some()
                                                                        && !self
                                                                            .worker_control_command_pending
                                                                        && self.persist_tx.is_some(),
                                                                    egui::Button::new(
                                                                        "Record status result",
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                self
                                                                    .record_worker_control_status_result(
                                                                        item.route_id,
                                                                    );
                                                            }
                                                        }

                                                        if matches!(
                                                            item.kind,
                                                            WorkerControlKind::StartOrResume
                                                                | WorkerControlKind::Stop
                                                        ) && item.acknowledged_sequence.is_some()
                                                        {
                                                            let action_record = action_records
                                                                .as_ref()
                                                                .and_then(|records| {
                                                                    records.iter().find(
                                                                        |record| {
                                                                            record.route_id
                                                                                == item.route_id
                                                                        },
                                                                    )
                                                                });
                                                            if let Some(record) = action_record
                                                                .filter(|record| {
                                                                    record.is_complete()
                                                                })
                                                            {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "ACTION RESULT · {} · lifecycle #{} · event #{}",
                                                                        worker_phase_label(
                                                                            record
                                                                                .resulting_phase
                                                                                .expect(
                                                                                    "complete action has resulting phase",
                                                                                ),
                                                                        ),
                                                                        record
                                                                            .lifecycle_sequence
                                                                            .expect(
                                                                                "complete action has lifecycle sequence",
                                                                            ),
                                                                        record
                                                                            .result_sequence
                                                                            .expect(
                                                                                "complete action has result sequence",
                                                                            ),
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                            } else {
                                                                let label = if action_record.is_some()
                                                                {
                                                                    "Resume apply control"
                                                                } else {
                                                                    "Apply control"
                                                                };
                                                                if ui
                                                                    .add_enabled(
                                                                        action_records.is_some()
                                                                            && !self
                                                                                .worker_control_command_pending
                                                                            && self
                                                                                .persist_tx
                                                                                .is_some(),
                                                                        egui::Button::new(label),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.apply_worker_control(
                                                                        item.route_id,
                                                                    );
                                                                }
                                                            }
                                                        }
                                                        if matches!(
                                                            item.kind,
                                                            WorkerControlKind::Continue { .. }
                                                        ) && item.acknowledged_sequence.is_some()
                                                        {
                                                            let execution = continuation_executions
                                                                .as_ref()
                                                                .and_then(|records| {
                                                                    records.iter().find(
                                                                        |record| {
                                                                            record.route_id
                                                                                == item.route_id
                                                                        },
                                                                    )
                                                                });
                                                            if let Some(execution) = execution {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "CONTINUATION STARTED · lease {}:{} · turn {} · event #{}",
                                                                        execution.lease_id.get(),
                                                                        execution.permit_ordinal,
                                                                        execution.execution_turn_id,
                                                                        execution.started_sequence,
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );

                                                                match continuation_execution_transport_state(
                                                                    &self.events,
                                                                    execution.execution_turn_id,
                                                                ) {
                                                                    Err(error) => {
                                                                        ui.label(
                                                                            egui::RichText::new(format!(
                                                                                "continuation transport blocked: {error}"
                                                                            ))
                                                                            .size(9.0)
                                                                            .color(
                                                                                egui::Color32::from_rgb(
                                                                                    186, 108, 108,
                                                                                ),
                                                                            ),
                                                                        );
                                                                    }
                                                                    Ok(transport) => {
                                                                        if let Some(sequence) =
                                                                            transport
                                                                                .completion_sequence
                                                                        {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUATION COMPLETED · event #{sequence}"
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        } else if let Some(
                                                                            sequence,
                                                                        ) = transport
                                                                            .failure_sequence
                                                                        {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUATION FAILED · event #{sequence}"
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        } else if let Some(
                                                                            sequence,
                                                                        ) = transport
                                                                            .interruption_sequence
                                                                        {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUATION INTERRUPTED · event #{sequence}"
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        } else if let Some(
                                                                            sequence,
                                                                        ) = transport
                                                                            .dispatch_sequence
                                                                        {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUATION DISPATCHED · event #{sequence}"
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        } else if ui
                                                                            .add_enabled(
                                                                                self.remote_connected()
                                                                                    && self
                                                                                        .selected_model
                                                                                        .is_some()
                                                                                    && self
                                                                                        .pending_remote_turn
                                                                                        .is_none()
                                                                                    && self
                                                                                        .active_remote_turn
                                                                                        .is_none()
                                                                                    && !self
                                                                                        .capability_probe
                                                                                        .running(),
                                                                                egui::Button::new(
                                                                                    "Dispatch continuation inference",
                                                                                ),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            self.dispatch_worker_continuation_execution(
                                                                                item.route_id,
                                                                            );
                                                                        }

                                                                        let scope = local_turn_scope(
                                                                            execution
                                                                                .execution_turn_id,
                                                                        );
                                                                        if let Some(output) = self
                                                                            .events
                                                                            .iter()
                                                                            .rev()
                                                                            .find(|event| {
                                                                                event
                                                                                    .scope
                                                                                    .as_deref()
                                                                                    == Some(
                                                                                        scope
                                                                                            .as_str(),
                                                                                    )
                                                                                    && matches!(
                                                                                        event.kind,
                                                                                        EventKind::AssistantSnapshotObserved
                                                                                            | EventKind::AssistantCompletionObserved
                                                                                    )
                                                                            })
                                                                            .map(|event| {
                                                                                event_text(
                                                                                    &event.payload,
                                                                                )
                                                                            })
                                                                            .filter(|text| {
                                                                                !text
                                                                                    .trim()
                                                                                    .is_empty()
                                                                            })
                                                                        {
                                                                            ui.collapsing(
                                                                                "Continuation output",
                                                                                |ui| {
                                                                                    ui.label(
                                                                                        egui::RichText::new(
                                                                                            output,
                                                                                        )
                                                                                        .size(10.0),
                                                                                    );
                                                                                },
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                            } else if ui
                                                                .add_enabled(
                                                                    continuation_executions
                                                                        .is_some()
                                                                        && !self
                                                                            .worker_control_command_pending
                                                                        && self.persist_tx.is_some(),
                                                                    egui::Button::new(
                                                                        "Start continuation execution",
                                                                    ),
                                                                )
                                                                .clicked()
                                                            {
                                                                self
                                                                    .start_worker_continuation_execution(
                                                                        item.route_id,
                                                                    );
                                                            }
                                                        }

                                                        if self.worker_control_command_pending {
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
                        ui.label(egui::RichText::new("Controller supervision").strong());
                        ui.label(
                            egui::RichText::new(
                                "Coordination provenance only. Controller designation and supervision do not grant dispatch, route-policy, continuation, or lifecycle authority.",
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
                                        "controller topology projection blocked: {error}"
                                    ))
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(186, 108, 108)),
                                );
                            }
                            Ok(None) => {
                                ui.label(
                                    egui::RichText::new(
                                        "enable local orchestration topology before controller supervision",
                                    )
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(139, 143, 153)),
                                );
                            }
                            Ok(Some(controller_topology)) => {
                                match (
                                    replay_session_audit(&self.events),
                                    replay_supervision_audit(&self.events),
                                    replay_local_conversation_topologies(&self.events),
                                    replay_local_conversation_worker_bindings(&self.events),
                                    replay_worker_audit(&self.events),
                                ) {
                                    (
                                        Ok(sessions),
                                        Ok(supervision),
                                        Ok(topologies),
                                        Ok(worker_bindings),
                                        Ok(workers),
                                    ) => {
                                        let current_session = sessions.iter().find(|record| {
                                            record.session_id
                                                == controller_topology.current_session_id
                                        });
                                        let current_is_worker = current_session
                                            .and_then(|record| record.worker_binding)
                                            .is_some();
                                        let designated = supervision.controllers.iter().any(
                                            |record| {
                                                record.designation.session_id()
                                                    == controller_topology.current_session_id
                                            },
                                        );

                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "session {} · {}",
                                                    controller_topology.current_session_id.get(),
                                                    if designated {
                                                        "CONTROLLER"
                                                    } else if current_is_worker {
                                                        "WORKER-BOUND"
                                                    } else {
                                                        "UNASSIGNED ROLE"
                                                    },
                                                ))
                                                .monospace()
                                                .size(9.0),
                                            );
                                            if !designated {
                                                if ui
                                                    .add_enabled(
                                                        !current_is_worker
                                                            && !self
                                                                .supervision_command_pending
                                                            && self.persist_tx.is_some(),
                                                        egui::Button::new(
                                                            "Designate as controller",
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    self.designate_current_session_controller();
                                                }
                                            }
                                            if self.supervision_command_pending {
                                                ui.spinner();
                                            }
                                        });

                                        if designated {
                                            let mut shown = 0_usize;
                                            let controller_conversation_id =
                                                self.local_conversation_id;
                                            for topology in topologies {
                                                if topology.conversation_id
                                                    == controller_conversation_id
                                                {
                                                    continue;
                                                }
                                                let Some(worker_binding) = worker_bindings
                                                    .iter()
                                                    .find(|binding| {
                                                        binding.conversation_id
                                                            == topology.conversation_id
                                                    })
                                                else {
                                                    continue;
                                                };
                                                shown = shown.saturating_add(1);

                                                let active = active_worker_session(
                                                    &sessions,
                                                    worker_binding.worker_id,
                                                );
                                                let active_current = active.is_some_and(
                                                    |record| {
                                                        record.session_id
                                                            == topology.current_session_id
                                                    },
                                                );
                                                let existing = supervision.bindings.iter().find(
                                                    |record| {
                                                        record.binding.worker_session_id()
                                                            == topology.current_session_id
                                                    },
                                                );
                                                let title = local_conversation_display_title(
                                                    &self.local_conversation_catalog,
                                                    topology.conversation_id,
                                                    &self.events,
                                                );

                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{title} · worker {} · session {}",
                                                            worker_binding.worker_id.get(),
                                                            topology.current_session_id.get(),
                                                        ))
                                                        .size(10.0),
                                                    );

                                                    if !active_current {
                                                        let active_label = active
                                                            .map(|record| {
                                                                record.session_id.get().to_string()
                                                            })
                                                            .unwrap_or_else(|| {
                                                                "unbound".to_owned()
                                                            });
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "WORKER SESSION {active_label} · CURRENT LEAF NOT READY"
                                                            ))
                                                            .monospace()
                                                            .size(9.0),
                                                        );
                                                    } else if let Some(existing) = existing {
                                                        if existing
                                                            .binding
                                                            .controller_session_id()
                                                            == controller_topology
                                                                .current_session_id
                                                        {
                                                            ui.label(
                                                                egui::RichText::new("SUPERVISED")
                                                                    .monospace()
                                                                    .size(9.0),
                                                            );

                                                            if let Some(worker) = workers
                                                                .iter()
                                                                .find(|record| {
                                                                    record.worker_id
                                                                        == worker_binding.worker_id
                                                                })
                                                            {
                                                                let phase =
                                                                    worker.lifecycle.phase();
                                                                let has_goal =
                                                                    worker.lifecycle.goal_id().is_some();
                                                                let controls_enabled =
                                                                    !self
                                                                        .controller_control_command_pending
                                                                        && self.persist_tx.is_some();

                                                                if let Some(goal_id) =
                                                                    worker.lifecycle.goal_id()
                                                                {
                                                                    match replay_continuation_audit(
                                                                        &self.events,
                                                                    ) {
                                                                        Err(error) => {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "continuation authority projection blocked: {error}"
                                                                                    ),
                                                                                )
                                                                                .size(9.0)
                                                                                .color(
                                                                                    egui::Color32::from_rgb(
                                                                                        186, 108, 108,
                                                                                    ),
                                                                                ),
                                                                            );
                                                                        }
                                                                        Ok(leases) => {
                                                                            let matching = leases
                                                                                .iter()
                                                                                .filter(|record| {
                                                                                    record.worker_id
                                                                                        == worker_binding
                                                                                            .worker_id
                                                                                        && record
                                                                                            .goal_id
                                                                                            == goal_id
                                                                                })
                                                                                .collect::<Vec<_>>();
                                                                            let unconsumed = matching
                                                                                .iter()
                                                                                .flat_map(
                                                                                    |record| {
                                                                                        record
                                                                                            .permits
                                                                                            .iter()
                                                                                            .filter(
                                                                                                |permit| {
                                                                                                    permit
                                                                                                        .consumed_by
                                                                                                        .is_none()
                                                                                                },
                                                                                            )
                                                                                    },
                                                                                )
                                                                                .count();
                                                                            let live_leases = matching
                                                                                .iter()
                                                                                .filter(|record| {
                                                                                    record.remaining
                                                                                        > 0
                                                                                })
                                                                                .count();
                                                                            let live_authority = matching
                                                                                .iter()
                                                                                .filter(|record| {
                                                                                    record.remaining
                                                                                        > 0
                                                                                        || record
                                                                                            .permits
                                                                                            .iter()
                                                                                            .any(
                                                                                                |permit| {
                                                                                                    permit
                                                                                                        .consumed_by
                                                                                                        .is_none()
                                                                                                },
                                                                                            )
                                                                                })
                                                                                .count();
                                                                            let issued = matching
                                                                                .iter()
                                                                                .map(|record| {
                                                                                    record.issued
                                                                                })
                                                                                .sum::<u32>();
                                                                            let remaining = matching
                                                                                .iter()
                                                                                .map(|record| {
                                                                                    record.remaining
                                                                                })
                                                                                .sum::<u32>();

                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUE AUTH · {} lease{} · {} issued · {} remaining · {} unconsumed",
                                                                                        matching.len(),
                                                                                        if matching.len() == 1 { "" } else { "s" },
                                                                                        issued,
                                                                                        remaining,
                                                                                        unconsumed,
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );

                                                                            let allowance_value = {
                                                                                let allowance = self
                                                                                    .continuation_allowance_drafts
                                                                                    .entry(
                                                                                        topology
                                                                                            .conversation_id,
                                                                                    )
                                                                                    .or_insert(1);
                                                                                *allowance =
                                                                                    (*allowance)
                                                                                        .clamp(1, 100);
                                                                                ui.add(
                                                                                    egui::DragValue::new(
                                                                                        allowance,
                                                                                    )
                                                                                    .range(1..=100)
                                                                                    .prefix(
                                                                                        "lease allowance ",
                                                                                    ),
                                                                                );
                                                                                *allowance
                                                                            };
                                                                            if ui
                                                                                .add_enabled(
                                                                                    controls_enabled
                                                                                        && !phase
                                                                                            .is_terminal()
                                                                                        && live_authority
                                                                                            == 0,
                                                                                    egui::Button::new(
                                                                                        "Create continue lease",
                                                                                    ),
                                                                                )
                                                                                .clicked()
                                                                            {
                                                                                self.create_controller_continuation_lease(
                                                                                    topology
                                                                                        .conversation_id,
                                                                                    allowance_value,
                                                                                );
                                                                            }

                                                                            let can_continue =
                                                                                controls_enabled
                                                                                    && phase
                                                                                        == WorkerPhase::Working
                                                                                    && (unconsumed
                                                                                        == 1
                                                                                        || (unconsumed
                                                                                            == 0
                                                                                            && live_leases
                                                                                                == 1));
                                                                            if ui
                                                                                .add_enabled(
                                                                                    can_continue,
                                                                                    egui::Button::new(
                                                                                        "Controller continue",
                                                                                    ),
                                                                                )
                                                                                .clicked()
                                                                            {
                                                                                self.propose_controller_worker_control(
                                                                                    topology
                                                                                        .conversation_id,
                                                                                    ControllerControlAction::Continue,
                                                                                );
                                                                            }
                                                                        }
                                                                    }
                                                                }

                                                                if matches!(
                                                                    phase,
                                                                    WorkerPhase::Ready
                                                                        | WorkerPhase::NeedsInput
                                                                        | WorkerPhase::Blocked
                                                                ) && ui
                                                                    .add_enabled(
                                                                        controls_enabled && has_goal,
                                                                        egui::Button::new(
                                                                            "Controller start/resume",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.propose_controller_worker_control(
                                                                        topology.conversation_id,
                                                                        ControllerControlAction::StartOrResume,
                                                                    );
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        controls_enabled
                                                                            && has_goal
                                                                            && !phase.is_terminal(),
                                                                        egui::Button::new(
                                                                            "Controller stop",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.propose_controller_worker_control(
                                                                        topology.conversation_id,
                                                                        ControllerControlAction::Stop,
                                                                    );
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        controls_enabled && has_goal,
                                                                        egui::Button::new(
                                                                            "Request status",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.propose_controller_worker_control(
                                                                        topology.conversation_id,
                                                                        ControllerControlAction::StatusRequest,
                                                                    );
                                                                }
                                                            } else {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        "NO DURABLE WORKER LIFECYCLE",
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                            }
                                                        } else {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "SUPERVISED BY SESSION {}",
                                                                    existing
                                                                        .binding
                                                                        .controller_session_id()
                                                                        .get(),
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                        }
                                                    } else if ui
                                                        .add_enabled(
                                                            !self
                                                                .supervision_command_pending
                                                                && self.persist_tx.is_some(),
                                                            egui::Button::new("Supervise"),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.supervise_local_conversation(
                                                            topology.conversation_id,
                                                        );
                                                    }
                                                });
                                            }

                                            if shown == 0 {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "no other local conversations have WorkerIds yet",
                                                    )
                                                    .size(9.0)
                                                    .color(egui::Color32::from_rgb(
                                                        139, 143, 153,
                                                    )),
                                                );
                                            }

                                            ui.add_space(6.0);
                                            ui.label(
                                                egui::RichText::new(
                                                    "Controller control routes",
                                                )
                                                .strong()
                                                .size(10.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    "Typed controls are inert until explicitly approved, one-shot dispatched, and durably delivered to the worker control inbox. Delivery does not itself mutate worker lifecycle.",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    139, 143, 153,
                                                )),
                                            );

                                            match (
                                                replay_control_audit(&self.events),
                                                replay_control_provenance_audit(&self.events),
                                                replay_control_route_audit(&self.events),
                                                replay_routing_audit(&self.events),
                                                replay_worker_control_delivery_audit(&self.events),
                                            ) {
                                                (
                                                    Ok(controls),
                                                    Ok(provenance),
                                                    Ok(bindings),
                                                    Ok(routes),
                                                    Ok(deliveries),
                                                ) => {
                                                    let mut shown_controls = 0_usize;
                                                    for provenance_record in provenance {
                                                        if provenance_record
                                                            .provenance
                                                            .issuer()
                                                            != ControlIssuer::ControllerSession(
                                                                controller_topology
                                                                    .current_session_id,
                                                            )
                                                        {
                                                            continue;
                                                        }
                                                        let control_id = provenance_record
                                                            .provenance
                                                            .control_id();
                                                        let Some(control) = controls.iter().find(
                                                            |record| {
                                                                record.control_id == control_id
                                                            },
                                                        ) else {
                                                            continue;
                                                        };
                                                        let Some(binding) = bindings.iter().find(
                                                            |record| {
                                                                record.binding.control_id()
                                                                    == control_id
                                                            },
                                                        ) else {
                                                            continue;
                                                        };
                                                        let Some(route) = routes.iter().find(
                                                            |record| {
                                                                record.request.id
                                                                    == binding.binding.route_id()
                                                            },
                                                        ) else {
                                                            continue;
                                                        };
                                                        let delivery = deliveries.iter().find(
                                                            |record| {
                                                                record.route_id == route.request.id
                                                            },
                                                        );
                                                        shown_controls =
                                                            shown_controls.saturating_add(1);

                                                        let next_sequence = self
                                                            .events
                                                            .last()
                                                            .map_or(1, |event| {
                                                                event.sequence.saturating_add(1)
                                                            });
                                                        let fresh =
                                                            validate_control_freshness_before(
                                                                &self.events,
                                                                control,
                                                                next_sequence,
                                                            )
                                                            .is_ok();

                                                        ui.group(|ui| {
                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "control {} · {} · worker {} · goal {} · route {} · {}",
                                                                        control.control_id.get(),
                                                                        worker_control_kind_label(
                                                                            control.kind,
                                                                        ),
                                                                        control.worker_id.get(),
                                                                        control.goal_id.get(),
                                                                        route.request.id.get(),
                                                                        route_gate_state_label(
                                                                            route.gate_state,
                                                                        ),
                                                                    ))
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        if fresh {
                                                                            "FRESH"
                                                                        } else {
                                                                            "STALE"
                                                                        },
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );

                                                                let can_update =
                                                                    fresh
                                                                        && !self
                                                                            .controller_control_command_pending
                                                                        && self.persist_tx.is_some()
                                                                        && !route
                                                                            .gate_state
                                                                            .is_dispatched();
                                                                if ui
                                                                    .add_enabled(
                                                                        can_update
                                                                            && route
                                                                                .latest_user_decision
                                                                                != Some(
                                                                                    RouteUserDecision::Allow,
                                                                                ),
                                                                        egui::Button::new("Allow"),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_controller_worker_control_route(
                                                                        route.request.id,
                                                                        RouteUserDecision::Allow,
                                                                    );
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        can_update
                                                                            && route
                                                                                .latest_user_decision
                                                                                != Some(
                                                                                    RouteUserDecision::Deny,
                                                                                ),
                                                                        egui::Button::new("Deny"),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_controller_worker_control_route(
                                                                        route.request.id,
                                                                        RouteUserDecision::Deny,
                                                                    );
                                                                }

                                                                let can_dispatch =
                                                                    delivery.is_none()
                                                                        && !self
                                                                            .controller_control_command_pending
                                                                        && self.persist_tx.is_some()
                                                                        && (matches!(
                                                                            route.gate_state,
                                                                            RouteGateState::Allowed {
                                                                                by: DecisionAuthority::User
                                                                            }
                                                                        ) && fresh
                                                                            || route
                                                                                .gate_state
                                                                                .is_dispatched());
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
                                                                    self.dispatch_controller_worker_control_route(
                                                                        route.request.id,
                                                                    );
                                                                }
                                                                if let Some(delivery) = delivery {
                                                                    ui.label(
                                                                        egui::RichText::new(format!(
                                                                            "DELIVERED · event #{}",
                                                                            delivery
                                                                                .delivered_sequence,
                                                                        ))
                                                                        .monospace()
                                                                        .size(9.0),
                                                                    );
                                                                }
                                                            });
                                                        });
                                                    }

                                                    if shown_controls == 0 {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                "no controller-issued controls yet",
                                                            )
                                                            .size(9.0)
                                                            .color(
                                                                egui::Color32::from_rgb(
                                                                    139, 143, 153,
                                                                ),
                                                            ),
                                                        );
                                                    }
                                                }
                                                (Err(error), _, _, _, _)
                                                | (_, Err(error), _, _, _)
                                                | (_, _, Err(error), _, _)
                                                | (_, _, _, Err(error), _)
                                                | (_, _, _, _, Err(error)) => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "controller control projection blocked: {error}"
                                                        ))
                                                        .size(9.0)
                                                        .color(
                                                            egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            ),
                                                        ),
                                                    );
                                                }
                                            }

                                            ui.add_space(6.0);
                                            ui.label(
                                                egui::RichText::new(
                                                    "Controller result inbox",
                                                )
                                                .strong()
                                                .size(10.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    "Durable results from controls issued by this controller conversation. Result visibility is separate from context use: every result is excluded by default until explicitly admitted.",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(
                                                    139, 143, 153,
                                                )),
                                            );
                                            match replay_controller_worker_results_for_conversation(
                                                &self.events,
                                                self.local_conversation_id,
                                            ) {
                                                Err(error) => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "controller result projection blocked: {error}"
                                                        ))
                                                        .size(9.0)
                                                        .color(
                                                            egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            ),
                                                        ),
                                                    );
                                                }
                                                Ok(results) if results.is_empty() => {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "no terminal worker results yet",
                                                        )
                                                        .size(9.0)
                                                        .color(
                                                            egui::Color32::from_rgb(
                                                                139, 143, 153,
                                                            ),
                                                        ),
                                                    );
                                                }
                                                Ok(results) => {
                                                    let context_records =
                                                        match replay_controller_worker_result_context_audit(
                                                            &self.events,
                                                        ) {
                                                            Ok(records) => Some(records),
                                                            Err(error) => {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "controller result context projection blocked: {error}"
                                                                    ))
                                                                    .size(9.0)
                                                                    .color(
                                                                        egui::Color32::from_rgb(
                                                                            186, 108, 108,
                                                                        ),
                                                                    ),
                                                                );
                                                                None
                                                            }
                                                        };
                                                    egui::ScrollArea::vertical()
                                                        .id_salt("controller-result-inbox")
                                                        .max_height(220.0)
                                                        .show(ui, |ui| {
                                                            for result in results {
                                                                let worker_title =
                                                                    local_conversation_display_title(
                                                                        &self
                                                                            .local_conversation_catalog,
                                                                        result
                                                                            .worker_conversation_id,
                                                                        &self.events,
                                                                    );
                                                                ui.group(|ui| {
                                                                    ui.horizontal_wrapped(|ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                format!(
                                                                                    "{worker_title} · control {} · route {} · worker {} · goal {} · result #{}",
                                                                                    result
                                                                                        .control_id
                                                                                        .get(),
                                                                                    result
                                                                                        .route_id
                                                                                        .get(),
                                                                                    result
                                                                                        .worker_id
                                                                                        .get(),
                                                                                    result
                                                                                        .goal_id
                                                                                        .get(),
                                                                                    result
                                                                                        .result_sequence,
                                                                                ),
                                                                            )
                                                                            .monospace()
                                                                            .size(9.0),
                                                                        );
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                format!(
                                                                                    "issuer session {}",
                                                                                    result
                                                                                        .controller_session_id
                                                                                        .get(),
                                                                                ),
                                                                            )
                                                                            .monospace()
                                                                            .size(9.0)
                                                                            .color(
                                                                                egui::Color32::from_rgb(
                                                                                    139, 143, 153,
                                                                                ),
                                                                            ),
                                                                        );
                                                                    });

                                                                    match &result.detail {
                                                                        ControllerWorkerResultDetail::Status {
                                                                            phase,
                                                                            acknowledged_sequence,
                                                                        } => {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "STATUS · {} · acknowledged #{}",
                                                                                        worker_phase_label(
                                                                                            *phase
                                                                                        ),
                                                                                        acknowledged_sequence,
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        }
                                                                        ControllerWorkerResultDetail::Action {
                                                                            kind,
                                                                            from_phase,
                                                                            resulting_phase,
                                                                            started_sequence,
                                                                            lifecycle_sequence,
                                                                        } => {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "ACTION · {} · {} → {} · started #{} · lifecycle #{}",
                                                                                        worker_control_kind_label(
                                                                                            *kind
                                                                                        ),
                                                                                        worker_phase_label(
                                                                                            *from_phase
                                                                                        ),
                                                                                        worker_phase_label(
                                                                                            *resulting_phase
                                                                                        ),
                                                                                        started_sequence,
                                                                                        lifecycle_sequence,
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                        }
                                                                        ControllerWorkerResultDetail::Continuation {
                                                                            outcome,
                                                                            execution_turn_id,
                                                                            lease_id,
                                                                            permit_ordinal,
                                                                            started_sequence,
                                                                            terminal_sequence,
                                                                            output_text,
                                                                        } => {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    format!(
                                                                                        "CONTINUATION · {} · lease {}:{} · turn {} · started #{} · terminal #{}",
                                                                                        outcome
                                                                                            .stable_name(),
                                                                                        lease_id
                                                                                            .get(),
                                                                                        permit_ordinal,
                                                                                        execution_turn_id,
                                                                                        started_sequence,
                                                                                        terminal_sequence,
                                                                                    ),
                                                                                )
                                                                                .monospace()
                                                                                .size(9.0),
                                                                            );
                                                                            if let Some(output) =
                                                                                output_text
                                                                            {
                                                                                ui.collapsing(
                                                                                    "Worker continuation output",
                                                                                    |ui| {
                                                                                        ui.label(
                                                                                            egui::RichText::new(
                                                                                                output,
                                                                                            )
                                                                                            .size(10.0),
                                                                                        );
                                                                                    },
                                                                                );
                                                                            }
                                                                        }
                                                                    }

                                                                    let current_decision =
                                                                        context_records
                                                                            .as_ref()
                                                                            .and_then(|records| {
                                                                                records.iter().find(
                                                                                    |record| {
                                                                                        record.route_id
                                                                                            == result
                                                                                                .route_id
                                                                                    },
                                                                                )
                                                                            })
                                                                            .map(|record| {
                                                                                record.decision
                                                                            });
                                                                    ui.horizontal_wrapped(|ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                match current_decision {
                                                                                    Some(
                                                                                        ControllerWorkerResultContextDecision::Admit,
                                                                                    ) => "CONTEXT: ADMITTED",
                                                                                    Some(
                                                                                        ControllerWorkerResultContextDecision::Exclude,
                                                                                    ) => "CONTEXT: EXCLUDED",
                                                                                    None => "CONTEXT: EXCLUDED · DEFAULT",
                                                                                },
                                                                            )
                                                                            .monospace()
                                                                            .size(9.0),
                                                                        );

                                                                        let can_change =
                                                                            context_records
                                                                                .is_some()
                                                                                && !self
                                                                                    .controller_result_context_command_pending
                                                                                && self.persist_tx
                                                                                    .is_some();
                                                                        if ui
                                                                            .add_enabled(
                                                                                can_change
                                                                                    && current_decision
                                                                                        != Some(
                                                                                            ControllerWorkerResultContextDecision::Admit,
                                                                                        ),
                                                                                egui::Button::new(
                                                                                    "Admit to context",
                                                                                ),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            self.decide_controller_worker_result_context(
                                                                                result.route_id,
                                                                                ControllerWorkerResultContextDecision::Admit,
                                                                            );
                                                                        }
                                                                        if ui
                                                                            .add_enabled(
                                                                                can_change
                                                                                    && current_decision
                                                                                        == Some(
                                                                                            ControllerWorkerResultContextDecision::Admit,
                                                                                        ),
                                                                                egui::Button::new(
                                                                                    "Exclude from context",
                                                                                ),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            self.decide_controller_worker_result_context(
                                                                                result.route_id,
                                                                                ControllerWorkerResultContextDecision::Exclude,
                                                                            );
                                                                        }
                                                                        if self
                                                                            .controller_result_context_command_pending
                                                                        {
                                                                            ui.spinner();
                                                                        }
                                                                    });
                                                                });
                                                            }
                                                        });
                                                }
                                            }

                                            ui.add_space(6.0);
                                            ui.label(
                                                egui::RichText::new("Controller coordination")
                                                    .strong()
                                                    .size(10.0),
                                            );
                                            ui.label(
                                                egui::RichText::new(
                                                    "Deliberate non-authored controller reasoning over the exact worker-result set admitted when the coordination turn starts. Starting and dispatching are separate user actions; coordination cannot itself issue controls, mutate lifecycle, or consume continuation authority.",
                                                )
                                                .size(9.0)
                                                .color(egui::Color32::from_rgb(139, 143, 153)),
                                            );

                                            let admitted_result_count =
                                                match replay_admitted_controller_worker_result_context(
                                                    &self.events,
                                                    self.local_conversation_id,
                                                ) {
                                                    Ok(records) => Some(records.len()),
                                                    Err(error) => {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "coordination context projection blocked: {error}"
                                                            ))
                                                            .size(9.0)
                                                            .color(egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            )),
                                                        );
                                                        None
                                                    }
                                                };

                                            match replay_controller_coordination_audit(&self.events) {
                                                Err(error) => {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "controller coordination projection blocked: {error}"
                                                        ))
                                                        .size(9.0)
                                                        .color(egui::Color32::from_rgb(
                                                            186, 108, 108,
                                                        )),
                                                    );
                                                }
                                                Ok(records) => {
                                                    let coordination_context_records =
                                                        match replay_controller_coordination_result_context_audit(
                                                            &self.events,
                                                        ) {
                                                            Ok(records) => Some(records),
                                                            Err(error) => {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "coordination result context projection blocked: {error}"
                                                                    ))
                                                                    .size(9.0)
                                                                    .color(egui::Color32::from_rgb(
                                                                        186, 108, 108,
                                                                    )),
                                                                );
                                                                None
                                                            }
                                                        };
                                                    let coordination_suggestions =
                                                        match replay_controller_coordination_suggestion_audit(
                                                            &self.events,
                                                        ) {
                                                            Ok(records) => Some(records),
                                                            Err(error) => {
                                                                ui.label(
                                                                    egui::RichText::new(format!(
                                                                        "coordination suggestion projection blocked: {error}"
                                                                    ))
                                                                    .size(9.0)
                                                                    .color(egui::Color32::from_rgb(
                                                                        186, 108, 108,
                                                                    )),
                                                                );
                                                                None
                                                            }
                                                        };
                                                    let owned = records
                                                        .into_iter()
                                                        .filter(|record| {
                                                            record.controller_conversation_id
                                                                == self.local_conversation_id
                                                        })
                                                        .collect::<Vec<_>>();
                                                    let latest = owned.last();
                                                    if let Some(record) = latest {
                                                        ui.horizontal_wrapped(|ui| {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "turn {} · controller session {} · {} admitted result{} · started #{}",
                                                                    record.coordination_turn_id,
                                                                    record.controller_session_id.get(),
                                                                    record.admitted_result_routes.len(),
                                                                    if record.admitted_result_routes.len() == 1 { "" } else { "s" },
                                                                    record.started_sequence,
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );

                                                            match controller_coordination_transport_state(
                                                                &self.events,
                                                                record.coordination_turn_id,
                                                            ) {
                                                                Err(error) => {
                                                                    ui.label(
                                                                        egui::RichText::new(format!(
                                                                            "transport blocked: {error}"
                                                                        ))
                                                                        .size(9.0)
                                                                        .color(egui::Color32::from_rgb(
                                                                            186, 108, 108,
                                                                        )),
                                                                    );
                                                                }
                                                                Ok(transport) => {
                                                                    if !transport.was_dispatched()
                                                                        && record
                                                                            .result_sequence
                                                                            .is_none()
                                                                    {
                                                                        let can_dispatch =
                                                                            self.persist_tx.is_some()
                                                                                && self
                                                                                    .pending_remote_turn
                                                                                    .is_none()
                                                                                && self
                                                                                    .active_remote_turn
                                                                                    .is_none()
                                                                                && self
                                                                                    .remote_connected()
                                                                                && self
                                                                                    .selected_model
                                                                                    .is_some()
                                                                                && !self
                                                                                    .capability_probe
                                                                                    .running();
                                                                        if ui
                                                                            .add_enabled(
                                                                                can_dispatch,
                                                                                egui::Button::new(
                                                                                    "Dispatch coordination",
                                                                                ),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            self.dispatch_controller_coordination(
                                                                                record.coordination_turn_id,
                                                                            );
                                                                        }
                                                                    } else if transport.was_dispatched()
                                                                        && !transport.is_terminal()
                                                                    {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                "IN FLIGHT",
                                                                            )
                                                                            .monospace()
                                                                            .size(9.0),
                                                                        );
                                                                    }
                                                                }
                                                            }
                                                        });

                                                        if let Some(outcome) = record.outcome {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "COORDINATION RESULT · {} · terminal #{} · result #{}",
                                                                    outcome.stable_name(),
                                                                    record
                                                                        .terminal_sequence
                                                                        .unwrap_or_default(),
                                                                    record
                                                                        .result_sequence
                                                                        .unwrap_or_default(),
                                                                ))
                                                                .monospace()
                                                                .size(9.0),
                                                            );
                                                            match controller_coordination_output_text(
                                                                &self.events,
                                                                record.coordination_turn_id,
                                                            ) {
                                                                Ok(Some(output)) => {
                                                                    ui.collapsing(
                                                                        "Coordination output",
                                                                        |ui| {
                                                                            ui.label(
                                                                                egui::RichText::new(
                                                                                    output,
                                                                                )
                                                                                .size(10.0),
                                                                            );
                                                                        },
                                                                    );
                                                                }
                                                                Ok(None) => {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            "no durable coordination output text",
                                                                        )
                                                                        .size(9.0)
                                                                        .color(
                                                                            egui::Color32::from_rgb(
                                                                                139, 143, 153,
                                                                            ),
                                                                        ),
                                                                    );
                                                                }
                                                                Err(error) => {
                                                                    ui.label(
                                                                        egui::RichText::new(format!(
                                                                            "coordination output blocked: {error}"
                                                                        ))
                                                                        .size(9.0)
                                                                        .color(egui::Color32::from_rgb(
                                                                            186, 108, 108,
                                                                        )),
                                                                    );
                                                                }
                                                            }

                                                            if outcome
                                                                == ControllerCoordinationOutcome::Completed
                                                            {
                                                                ui.collapsing(
                                                                    "Model suggestion candidates · UNTRUSTED",
                                                                    |ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                "Parsed from the coordination model's strict JSON output. Candidates have no durable identity or authority until you explicitly accept one into the durable suggestion layer.",
                                                                            )
                                                                            .size(9.0)
                                                                            .color(egui::Color32::from_rgb(
                                                                                139, 143, 153,
                                                                            )),
                                                                        );
                                                                        match project_controller_coordination_suggestion_candidates(
                                                                            &self.events,
                                                                            record.coordination_turn_id,
                                                                        ) {
                                                                            Err(error) => {
                                                                                ui.label(
                                                                                    egui::RichText::new(format!(
                                                                                        "candidate output invalid: {error}"
                                                                                    ))
                                                                                    .size(9.0)
                                                                                    .color(egui::Color32::from_rgb(
                                                                                        186, 108, 108,
                                                                                    )),
                                                                                );
                                                                            }
                                                                            Ok(projection) => {
                                                                                ui.label(
                                                                                    egui::RichText::new(format!(
                                                                                        "Summary: {}",
                                                                                        projection.summary
                                                                                    ))
                                                                                    .size(10.0),
                                                                                );
                                                                                if projection.candidates.is_empty() {
                                                                                    ui.label(
                                                                                        egui::RichText::new(
                                                                                            "model proposed no worker actions",
                                                                                        )
                                                                                        .size(9.0)
                                                                                        .color(egui::Color32::from_rgb(
                                                                                            139, 143, 153,
                                                                                        )),
                                                                                    );
                                                                                }
                                                                                for candidate in projection.candidates {
                                                                                    let already_accepted =
                                                                                        coordination_suggestions
                                                                                            .as_ref()
                                                                                            .is_some_and(|suggestions| {
                                                                                                suggestions.iter().any(|suggestion| {
                                                                                                    suggestion.coordination_turn_id
                                                                                                        == record.coordination_turn_id
                                                                                                        && suggestion.basis_result_route_id
                                                                                                            == candidate
                                                                                                                .basis_result_route_id
                                                                                                        && suggestion.suggestion.action()
                                                                                                            == candidate.action
                                                                                                })
                                                                                            });
                                                                                    let worker_title =
                                                                                        local_conversation_display_title(
                                                                                            &self.local_conversation_catalog,
                                                                                            candidate.worker_conversation_id,
                                                                                            &self.events,
                                                                                        );
                                                                                    ui.group(|ui| {
                                                                                        ui.horizontal_wrapped(|ui| {
                                                                                            ui.label(
                                                                                                egui::RichText::new(format!(
                                                                                                    "CANDIDATE {} · {} · {worker_title} · worker {} · goal {} · basis route {} · NO AUTHORITY",
                                                                                                    candidate.candidate_index + 1,
                                                                                                    coordination_suggestion_action_label(
                                                                                                        candidate.action
                                                                                                    ),
                                                                                                    candidate.worker_id.get(),
                                                                                                    candidate.goal_id.get(),
                                                                                                    candidate.basis_result_route_id.get(),
                                                                                                ))
                                                                                                .monospace()
                                                                                                .size(9.0),
                                                                                            );
                                                                                            if already_accepted {
                                                                                                ui.label(
                                                                                                    egui::RichText::new(
                                                                                                        "ACCEPTED → durable suggestion",
                                                                                                    )
                                                                                                    .monospace()
                                                                                                    .size(9.0),
                                                                                                );
                                                                                            } else {
                                                                                                let can_accept = coordination_suggestions
                                                                                                    .is_some()
                                                                                                    && !self
                                                                                                        .controller_coordination_suggestion_command_pending
                                                                                                    && !self
                                                                                                        .controller_control_command_pending
                                                                                                    && self.persist_tx.is_some();
                                                                                                if ui
                                                                                                    .add_enabled(
                                                                                                        can_accept,
                                                                                                        egui::Button::new(
                                                                                                            "Accept candidate",
                                                                                                        ),
                                                                                                    )
                                                                                                    .clicked()
                                                                                                {
                                                                                                    self.record_controller_coordination_suggestion(
                                                                                                        record.coordination_turn_id,
                                                                                                        candidate.basis_result_route_id,
                                                                                                        candidate.action,
                                                                                                    );
                                                                                                }
                                                                                            }
                                                                                        });
                                                                                    });
                                                                                }
                                                                                if self
                                                                                    .controller_coordination_suggestion_command_pending
                                                                                {
                                                                                    ui.spinner();
                                                                                }
                                                                            }
                                                                        }
                                                                    },
                                                                );

                                                                ui.collapsing(
                                                                    "Typed suggestions · NO AUTHORITY",
                                                                    |ui| {
                                                                        ui.label(
                                                                            egui::RichText::new(
                                                                                "Suggestions are durable proposal data only. Recording one does not create a WorkerControl, route, approval, lifecycle transition, continuation permit, or dispatch.",
                                                                            )
                                                                            .size(9.0)
                                                                            .color(egui::Color32::from_rgb(
                                                                                139, 143, 153,
                                                                            )),
                                                                        );

                                                                        match replay_controller_worker_results_for_conversation(
                                                                            &self.events,
                                                                            self.local_conversation_id,
                                                                        ) {
                                                                            Err(error) => {
                                                                                ui.label(
                                                                                    egui::RichText::new(format!(
                                                                                        "suggestion basis projection blocked: {error}"
                                                                                    ))
                                                                                    .size(9.0)
                                                                                    .color(egui::Color32::from_rgb(
                                                                                        186, 108, 108,
                                                                                    )),
                                                                                );
                                                                            }
                                                                            Ok(results) => {
                                                                                for route_id in
                                                                                    &record.admitted_result_routes
                                                                                {
                                                                                    let Some(basis) = results
                                                                                        .iter()
                                                                                        .find(|item| {
                                                                                            item.route_id
                                                                                                == *route_id
                                                                                        })
                                                                                    else {
                                                                                        ui.label(
                                                                                            egui::RichText::new(format!(
                                                                                                "basis route {} missing from controller result inbox",
                                                                                                route_id.get()
                                                                                            ))
                                                                                            .size(9.0)
                                                                                            .color(egui::Color32::from_rgb(
                                                                                                186, 108, 108,
                                                                                            )),
                                                                                        );
                                                                                        continue;
                                                                                    };
                                                                                    let worker_title =
                                                                                        local_conversation_display_title(
                                                                                            &self.local_conversation_catalog,
                                                                                            basis.worker_conversation_id,
                                                                                            &self.events,
                                                                                        );
                                                                                    ui.group(|ui| {
                                                                                        ui.label(
                                                                                            egui::RichText::new(format!(
                                                                                                "{worker_title} · worker {} · goal {} · basis route {}",
                                                                                                basis.worker_id.get(),
                                                                                                basis.goal_id.get(),
                                                                                                route_id.get(),
                                                                                            ))
                                                                                            .monospace()
                                                                                            .size(9.0),
                                                                                        );

                                                                                        let existing = coordination_suggestions
                                                                                            .as_ref()
                                                                                            .map(|records| {
                                                                                                records
                                                                                                    .iter()
                                                                                                    .filter(|suggestion| {
                                                                                                        suggestion.coordination_turn_id
                                                                                                            == record.coordination_turn_id
                                                                                                            && suggestion.basis_result_route_id
                                                                                                                == *route_id
                                                                                                    })
                                                                                                    .collect::<Vec<_>>()
                                                                                            })
                                                                                            .unwrap_or_default();
                                                                                        for suggestion in &existing {
                                                                                            ui.horizontal_wrapped(|ui| {
                                                                                                if let (
                                                                                                    Some(control_id),
                                                                                                    Some(route_id),
                                                                                                    Some(promoted_sequence),
                                                                                                ) = (
                                                                                                    suggestion.promoted_control_id,
                                                                                                    suggestion.promoted_route_id,
                                                                                                    suggestion.promoted_sequence,
                                                                                                ) {
                                                                                                    ui.label(
                                                                                                        egui::RichText::new(format!(
                                                                                                            "SUGGESTION {} · {} · PROMOTED → control {} · route {} · promotion #{} · AWAITING ROUTE APPROVAL",
                                                                                                            suggestion.suggestion.id().get(),
                                                                                                            coordination_suggestion_action_label(
                                                                                                                suggestion.suggestion.action()
                                                                                                            ),
                                                                                                            control_id.get(),
                                                                                                            route_id.get(),
                                                                                                            promoted_sequence,
                                                                                                        ))
                                                                                                        .monospace()
                                                                                                        .size(9.0),
                                                                                                    );
                                                                                                } else {
                                                                                                    ui.label(
                                                                                                        egui::RichText::new(format!(
                                                                                                            "SUGGESTION {} · {} · NO AUTHORITY · event #{}",
                                                                                                            suggestion.suggestion.id().get(),
                                                                                                            coordination_suggestion_action_label(
                                                                                                                suggestion.suggestion.action()
                                                                                                            ),
                                                                                                            suggestion.recorded_sequence,
                                                                                                        ))
                                                                                                        .monospace()
                                                                                                        .size(9.0),
                                                                                                    );
                                                                                                    let can_promote = !self
                                                                                                        .controller_coordination_suggestion_command_pending
                                                                                                        && !self
                                                                                                            .controller_control_command_pending
                                                                                                        && self.persist_tx.is_some();
                                                                                                    if ui
                                                                                                        .add_enabled(
                                                                                                            can_promote,
                                                                                                            egui::Button::new(
                                                                                                                "Promote suggestion",
                                                                                                            ),
                                                                                                        )
                                                                                                        .clicked()
                                                                                                    {
                                                                                                        self.promote_controller_coordination_suggestion(
                                                                                                            suggestion.suggestion.id(),
                                                                                                        );
                                                                                                    }
                                                                                                }
                                                                                            });
                                                                                        }

                                                                                        ui.horizontal_wrapped(|ui| {
                                                                                            for action in [
                                                                                                CoordinationSuggestionAction::StatusRequest,
                                                                                                CoordinationSuggestionAction::StartOrResume,
                                                                                                CoordinationSuggestionAction::Continue,
                                                                                                CoordinationSuggestionAction::Stop,
                                                                                            ] {
                                                                                                let already_exists = existing.iter().any(
                                                                                                    |suggestion| {
                                                                                                        suggestion.suggestion.action()
                                                                                                            == action
                                                                                                    },
                                                                                                );
                                                                                                let enabled = coordination_suggestions
                                                                                                    .is_some()
                                                                                                    && !already_exists
                                                                                                    && !self
                                                                                                        .controller_coordination_suggestion_command_pending
                                                                                                    && !self
                                                                                                        .controller_control_command_pending
                                                                                                    && self.persist_tx.is_some();
                                                                                                if ui
                                                                                                    .add_enabled(
                                                                                                        enabled,
                                                                                                        egui::Button::new(format!(
                                                                                                            "Suggest {}",
                                                                                                            coordination_suggestion_action_label(
                                                                                                                action
                                                                                                            )
                                                                                                        )),
                                                                                                    )
                                                                                                    .clicked()
                                                                                                {
                                                                                                    self.record_controller_coordination_suggestion(
                                                                                                        record.coordination_turn_id,
                                                                                                        *route_id,
                                                                                                        action,
                                                                                                    );
                                                                                                }
                                                                                            }
                                                                                            if self
                                                                                                .controller_coordination_suggestion_command_pending
                                                                                            {
                                                                                                ui.spinner();
                                                                                            }
                                                                                        });
                                                                                    });
                                                                                }
                                                                            }
                                                                        }
                                                                    },
                                                                );
                                                            }

                                                            let current_decision =
                                                                coordination_context_records
                                                                    .as_ref()
                                                                    .and_then(|records| {
                                                                        records.iter().find(
                                                                            |context_record| {
                                                                                context_record
                                                                                    .coordination_turn_id
                                                                                    == record
                                                                                        .coordination_turn_id
                                                                            },
                                                                        )
                                                                    })
                                                                    .map(|context_record| {
                                                                        context_record.decision
                                                                    });
                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    egui::RichText::new(
                                                                        match current_decision {
                                                                            Some(
                                                                                ControllerCoordinationResultContextDecision::Admit,
                                                                            ) => "CONTEXT: ADMITTED",
                                                                            Some(
                                                                                ControllerCoordinationResultContextDecision::Exclude,
                                                                            ) => "CONTEXT: EXCLUDED",
                                                                            None => "CONTEXT: EXCLUDED · DEFAULT",
                                                                        },
                                                                    )
                                                                    .monospace()
                                                                    .size(9.0),
                                                                );

                                                                let can_change =
                                                                    coordination_context_records
                                                                        .is_some()
                                                                        && !self
                                                                            .controller_coordination_result_context_command_pending
                                                                        && self.persist_tx.is_some();
                                                                if ui
                                                                    .add_enabled(
                                                                        can_change
                                                                            && current_decision
                                                                                != Some(
                                                                                    ControllerCoordinationResultContextDecision::Admit,
                                                                                ),
                                                                        egui::Button::new(
                                                                            "Admit result to context",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_controller_coordination_result_context(
                                                                        record.coordination_turn_id,
                                                                        ControllerCoordinationResultContextDecision::Admit,
                                                                    );
                                                                }
                                                                if ui
                                                                    .add_enabled(
                                                                        can_change
                                                                            && current_decision
                                                                                == Some(
                                                                                    ControllerCoordinationResultContextDecision::Admit,
                                                                                ),
                                                                        egui::Button::new(
                                                                            "Exclude result from context",
                                                                        ),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    self.decide_controller_coordination_result_context(
                                                                        record.coordination_turn_id,
                                                                        ControllerCoordinationResultContextDecision::Exclude,
                                                                    );
                                                                }
                                                                if self
                                                                    .controller_coordination_result_context_command_pending
                                                                {
                                                                    ui.spinner();
                                                                }
                                                            });
                                                        }
                                                    }

                                                    let unfinished = latest
                                                        .is_some_and(|record| {
                                                            record.result_sequence.is_none()
                                                        });
                                                    let count =
                                                        admitted_result_count.unwrap_or_default();
                                                    let can_start = count > 0
                                                        && !unfinished
                                                        && !self
                                                            .controller_coordination_command_pending
                                                        && self.persist_tx.is_some();
                                                    if ui
                                                        .add_enabled(
                                                            can_start,
                                                            egui::Button::new(if latest.is_some() {
                                                                "Start another coordination turn"
                                                            } else {
                                                                "Start coordination turn"
                                                            }),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.start_controller_coordination();
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{count} worker result{} currently admitted; a new start freezes exactly this set",
                                                            if count == 1 { "" } else { "s" },
                                                        ))
                                                        .size(9.0)
                                                        .color(egui::Color32::from_rgb(
                                                            139, 143, 153,
                                                        )),
                                                    );
                                                }
                                            }

                                            if self.controller_coordination_command_pending
                                                || self.controller_coordination_suggestion_command_pending
                                                || self.controller_control_command_pending
                                            {
                                                ui.spinner();
                                            }
                                        }
                                    }
                                    (Err(error), _, _, _, _)
                                    | (_, Err(error), _, _, _)
                                    | (_, _, Err(error), _, _)
                                    | (_, _, _, Err(error), _)
                                    | (_, _, _, _, Err(error)) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "supervision projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(186, 108, 108)),
                                        );
                                    }
                                }
                            }
                        }

                        ui.add_space(8.0);
                        ui.collapsing("Admitted MCP evidence · current conversation", |ui| {
                            // Use the exact verified admission replay consumed by the
                            // normal request composer, not the bounded provider review.
                            match replay_admitted_tool_results(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(admitted) => match replay_tool_call_audit(&self.events) {
                                    Ok(calls) => {
                                        match mcp_context_inventory::verified_inventory(
                                            &admitted,
                                            &calls,
                                            self.local_conversation_id,
                                        ) {
                                            Ok(rows) => {
                                                let can_revoke = !self.tool_command_pending
                                                    && self.persist_tx.is_some();
                                                if let Some(call_id) =
                                                    mcp_context_inventory::render_inventory(
                                                        ui,
                                                        &rows,
                                                        can_revoke,
                                                    )
                                                {
                                                    self.decide_tool_result_context(
                                                        call_id,
                                                        ToolResultContextDecision::Exclude,
                                                    );
                                                }
                                            }
                                            Err(error) => {
                                                ui.label(format!(
                                                    "Admitted evidence inventory blocked by inconsistent identities: {error}"
                                                ));
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        ui.label(format!(
                                            "Admitted evidence inventory blocked by tool call audit: {error}"
                                        ));
                                    }
                                },
                                Err(error) => {
                                    ui.label(format!(
                                        "Admitted evidence inventory blocked by admission audit: {error}"
                                    ));
                                }
                            }
                        });
                        ui.add_space(8.0);
                        ui.collapsing("Recent outgoing context snapshots · journaled", |ui| {
                            mcp_dispatch_manifest::render_dispatches(
                                ui,
                                &self.events,
                                self.local_conversation_id,
                            );
                        });
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
                            match admitted_tool_result_context_messages(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(tool_results) => transcript.extend(tool_results),
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "tool result context composition blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            match admitted_local_memory_messages(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(memory_context) => transcript.extend(memory_context),
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "local memory composition blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            // Mirror the frozen Send composition using the same
                            // verified background cache; never perform MCP I/O in preview.
                            transcript.extend(
                                self.openviking_project_cache.admitted_relevant_context(
                                    self.openviking_retrieval_cache
                                        .get(&self.local_conversation_id)
                                        .map(Vec::as_slice),
                                    self.local_conversation_id,
                                    &self.local_conversation_catalog,
                                    &self.project_memory_grants,
                                    &self.draft,
                                ),
                            );
                            let selected_one_shot = self
                                .local_memory_one_shot_selections
                                .get(&self.local_conversation_id)
                                .cloned()
                                .unwrap_or_default();
                            match snapshot_one_shot_local_memory(
                                &self.events,
                                self.local_conversation_id,
                                &selected_one_shot,
                            ) {
                                Ok((one_shot, Some(snapshot_after_sequence))) => {
                                    transcript.extend(one_shot.into_iter().map(|memory| {
                                        context_composer::TranscriptMessage::local_memory_one_shot(
                                            memory.text.as_str(),
                                            memory.memory_id.get(),
                                            memory.source_conversation_id.to_string(),
                                            memory.artifact_sequence,
                                            snapshot_after_sequence,
                                            None,
                                        )
                                    }));
                                }
                                Ok((_, None)) => {}
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "one-shot local memory composition blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            match admitted_controller_worker_result_messages(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(controller_results) => transcript.extend(controller_results),
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "controller result context composition blocked: {error}"
                                        ))
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(186, 108, 108)),
                                    );
                                }
                            }
                            match admitted_controller_coordination_result_messages(
                                &self.events,
                                self.local_conversation_id,
                            ) {
                                Ok(coordination_results) => transcript.extend(coordination_results),
                                Err(error) => {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "coordination result context composition blocked: {error}"
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
                            let context_plan = match context_composer::ContextPlan::compose_bounded(
                                context_composer::ContextPolicy::preview(),
                                self.conversation_instructions.as_str(),
                                self.conversation_developer_context.as_str(),
                                transcript,
                            ) {
                                Ok(plan) => plan,
                                Err(error) => {
                                    ui.label(format!("Bounded context preview unavailable: {error}"));
                                    return;
                                }
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "Context Composer · {} durable transcript message{} · {} admitted routed item{} · {} admitted tool result{} · {} admitted local memor{} · {} verified OpenViking checkpoint{} · {} next-request-only memor{} · {} admitted controller result{} · {} admitted coordination result{} · developer context {} · current draft {}",
                                    context_plan.durable_transcript_count(),
                                    if context_plan.durable_transcript_count() == 1 { "" } else { "s" },
                                    context_plan.routed_context_count(),
                                    if context_plan.routed_context_count() == 1 { "" } else { "s" },
                                    context_plan.tool_result_count(),
                                    if context_plan.tool_result_count() == 1 { "" } else { "s" },
                                    context_plan.local_memory_count(),
                                    if context_plan.local_memory_count() == 1 { "y" } else { "ies" },
                                    context_plan.openviking_checkpoint_count(),
                                    if context_plan.openviking_checkpoint_count() == 1 { "" } else { "s" },
                                    context_plan.local_memory_one_shot_count(),
                                    if context_plan.local_memory_one_shot_count() == 1 { "y" } else { "ies" },
                                    context_plan.controller_worker_result_count(),
                                    if context_plan.controller_worker_result_count() == 1 { "" } else { "s" },
                                    context_plan.controller_coordination_result_count(),
                                    if context_plan.controller_coordination_result_count() == 1 { "" } else { "s" },
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
                    });
                if inference_controls_changed {
                    self.persist_current_inference_settings();
                }
                if behavior_profile_changed {
                    self.persist_current_behavior_profile();
                }


                        ui.add_space(10.0);
                        ui.separator();

                        if ACCOUNT_MIRRORING_ENABLED {
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

                        }
                        ui.add_space(18.0);
                        ui.label(
                            egui::RichText::new("CHATS")
                                .size(10.0)
                                .strong()
                                .color(egui::Color32::from_rgb(112, 116, 126)),
                        );
                        ui.add_space(6.0);
                        let data_usage = local_storage_bytes_cached(ctx, &self.journal_path);
                        ui.label(
                            egui::RichText::new(format!(
                                "Local Chatarium data: {} on disk",
                                readable_byte_size(data_usage),
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(173, 189, 208)),
                        );

                        ui.horizontal(|ui| {
                            if ui.button("+ New chat").clicked() {
                                create_local_requested = true;
                            }
                            ui.checkbox(
                                &mut self.show_archived_local_conversations,
                                "Show archived",
                            );
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            // Reserve the button and item spacing even at the default 290px panel.
                            let input_width = (ui.available_width() - 70.0).max(90.0);
                            let native_search_response = ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.local_conversation_search_query,
                                )
                                .id(native_search_id)
                                .hint_text("Search chats…")
                                .desired_width(input_width),
                            );
                            native_search_has_focus = native_search_response.has_focus();
                            if native_search_response.changed() {
                                self.local_conversation_search_selection = None;
                            }
                            if ui
                                .add_enabled(
                                    !self.local_conversation_search_query.is_empty(),
                                    egui::Button::new("Clear"),
                                )
                                .clicked()
                            {
                                self.local_conversation_search_query.clear();
                                self.local_conversation_search_selection = None;
                            }
                        });
                        ui.add_space(8.0);
                        ui.add_space(4.0);

                        let native_entries = self.local_conversation_catalog.entries();
                        let native_rows = native_conversation_search::cached_rows(
                            ctx,
                            &native_search_index,
                            &native_entries,
                            self.show_archived_local_conversations,
                            &self.local_conversation_search_query,
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} chat{}{}",
                                native_rows.len(),
                                if native_rows.len() == 1 { "" } else { "s" },
                                if self.local_conversation_search_query.trim().is_empty() {
                                    String::new()
                                } else {
                                    format!(" found · {} total", native_entries.len())
                                },
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(139, 143, 153)),
                        );
                        if native_rows.is_empty() {
                            ui.label(
                                "No local chats match. Clear the search or enable Show archived.",
                            );
                        }
                        // Scroll the conversation catalog separately from status/settings.
                        egui::ScrollArea::vertical()
                            .id_salt("local-chats-scroll")
                            .max_height((ctx.content_rect().height() * 0.50).clamp(180.0, 620.0))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_width(sidebar_control_width);
                        for row in native_rows.iter() {
                            let entry = &native_entries[row.catalog_index];
                            let messages = native_search_index.messages(entry.id);
                            let title = row.title.as_str();
                            let rgb = entry.accent_rgb();
                            let accent = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                            // A 42-character unbroken title must not force the
                            // entire resizable sidebar to its maximum width.
                            // Keep the original title as a hover explanation.
                            let title_preview = title.chars().take(24).collect::<String>();
                            let title_preview = if title.chars().count() > 24 {
                                format!("{title_preview}…")
                            } else {
                                title_preview
                            };
                            let match_kind = row.kind;
                            let hit_choice_id =
                                egui::Id::new(("chatarium-native-hit-choice", entry.id));
                            let old_choice = ctx.data_mut(|data| {
                                data.get_temp::<(String, Option<u64>)>(hit_choice_id)
                            });
                            let remembered_sequence = old_choice
                                .as_ref()
                                .filter(|(query, _)| query == &self.local_conversation_search_query)
                                .and_then(|(_, source_sequence)| *source_sequence);
                            let mut hit_ordinal = native_conversation_search::selected_hit_ordinal(
                                row,
                                messages,
                                remembered_sequence,
                            );
                            let mut stale_hit = remembered_sequence.is_some()
                                && native_conversation_search::confirmed_hit_index(
                                    row,
                                    messages,
                                    remembered_sequence,
                                )
                                .is_none();
                            if match_kind
                                == native_conversation_search::NativeSearchMatch::Message
                            {
                                native_message_hit_conversations.insert(entry.id);
                            }
                            visible_native_search_matches += 1;
                            let result_index = native_search_matches.len();
                            native_search_matches.push(entry.id);
                            native_copy_candidates.push((
                                entry.id,
                                if stale_hit {
                                    None
                                } else {
                                    native_conversation_search::selected_message_index(
                                        row,
                                        hit_ordinal,
                                    )
                                },
                            ));
                            let keyboard_selected =
                                self.local_conversation_search_selection == Some(entry.id);
                            let selected =
                                !historical_mode && entry.id == self.local_conversation_id;
                            egui::Frame::default()
                                .fill(if selected || keyboard_selected {
                                    egui::Color32::from_rgb(30, 48, 75)
                                } else {
                                    egui::Color32::from_rgb(27, 32, 42)
                                })
                                .stroke(egui::Stroke::new(
                                    if selected || keyboard_selected { 1.7 } else { 1.0 },
                                    if selected || keyboard_selected {
                                        accent
                                    } else {
                                        egui::Color32::from_rgb(57, 72, 94)
                                    },
                                ))
                                .corner_radius(egui::CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    // Egui Frames normally shrink to their text content.
                                    // Fill every sidebar row to the same usable width.
                                    ui.set_min_width((sidebar_control_width - 20.0).max(120.0));
                                    ui.horizontal(|ui| {
                                        ui.colored_label(accent, "●");
                                        if ui
                                        .selectable_label(
                                            selected || keyboard_selected,
                                            egui::RichText::new(title_preview.as_str())
                                                .color(egui::Color32::from_rgb(244, 245, 249))
                                                .strong(),
                                        )
                                        .on_hover_text(title)
                                        .clicked()
                                    {
                                        select_local_requested = Some(entry.id);
                                        // Opening the conversation is safe, but a
                                        // vanished remembered event must never
                                        // retarget the reader to the first new
                                        // matching message without an explicit
                                        // hit-selection action.
                                        native_reader_source_stale = stale_hit;
                                        native_reader_target_message = if stale_hit {
                                            None
                                        } else {
                                            native_conversation_search::selected_message_index(
                                                row,
                                                hit_ordinal,
                                            )
                                            .map(|index| (entry.id, index))
                                        };
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        if !self.local_conversation_search_query.trim().is_empty() {
                                            ui.label(
                                                egui::RichText::new(match match_kind {
                                                    native_conversation_search::NativeSearchMatch::Title => "TITLE MATCH",
                                                    native_conversation_search::NativeSearchMatch::Message => "MESSAGE MATCH",
                                                })
                                                .size(9.0)
                                                .strong()
                                                .color(egui::Color32::from_rgb(112, 176, 137)),
                                            );
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "{} message{}",
                                                row.summary.message_count,
                                                if row.summary.message_count == 1 { "" } else { "s" }
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
                                    // A compact latest-message excerpt distinguishes
                                    // similarly named chats without opening them. Search
                                    // result excerpts below remain separate and exact.
                                    if self.local_conversation_search_query.trim().is_empty() {
                                        let latest = row.summary.latest_excerpt.as_deref();
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(
                                                    latest.unwrap_or("New conversation")
                                                )
                                                .size(10.0)
                                                .color(egui::Color32::from_rgb(156, 160, 171)),
                                            )
                                            .truncate(),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} words · {} chat text",
                                            row.summary.authored_words,
                                            readable_byte_size(row.summary.authored_bytes as u64)
                                        ))
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(142, 167, 194)),
                                    );
                                    if row.matching_message_indices.len() > 1 {
                                        ui.horizontal(|ui| {
                                            if ui.small_button("Prev hit").clicked() {
                                                hit_ordinal = if hit_ordinal == 0 {
                                                    row.matching_message_indices.len() - 1
                                                } else {
                                                    hit_ordinal - 1
                                                };
                                                stale_hit = false;
                                            }
                                            ui.label(format!(
                                                "Match {} of {}",
                                                hit_ordinal + 1,
                                                row.matching_message_indices.len(),
                                            ));
                                            if ui.small_button("Next hit").clicked() {
                                                hit_ordinal = (hit_ordinal + 1)
                                                    % row.matching_message_indices.len();
                                                stale_hit = false;
                                            }
                                        });
                                    }
                                    if stale_hit
                                        && !row.matching_message_indices.is_empty()
                                        && ui.small_button("Select first match").clicked()
                                    {
                                        // A removed source event must never
                                        // implicitly retarget a copy or memory
                                        // staging action; this is explicit.
                                        hit_ordinal = 0;
                                        stale_hit = false;
                                    }
                                    let selected_message_index = if stale_hit {
                                        None
                                    } else {
                                        native_conversation_search::selected_message_index(
                                            row,
                                            hit_ordinal,
                                        )
                                    };
                                    let selected_source_sequence = if stale_hit {
                                        remembered_sequence
                                    } else {
                                        native_conversation_search::selected_hit_sequence(
                                            row,
                                            messages,
                                            hit_ordinal,
                                        )
                                    };
                                    ctx.data_mut(|data| {
                                        data.insert_temp(
                                            hit_choice_id,
                                            (
                                    self.local_conversation_search_query.clone(),
                                    selected_source_sequence,
                                ),
                                        )
                                    });
                                    native_copy_candidates[result_index].1 = selected_message_index;
                                    let selected_preview = if hit_ordinal == 0 {
                                        row.preview.clone()
                                    } else {
                                        selected_message_index.and_then(|position| {
                                            local_archive_search::local_snippet(
                                                &messages[position].text,
                                                &self.local_conversation_search_query.trim().to_lowercase(),
                                            )
                                        })
                                    };
                                    if let Some(excerpt) = selected_preview {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(excerpt)
                                                    .italics()
                                                    .size(10.0)
                                                    .color(egui::Color32::from_rgb(170, 174, 184)),
                                            )
                                            .truncate(),
                                        );
                                    }
                                    if let Some(message_index) = selected_message_index {
                                        if ui
                                            .small_button("Copy matched message · Alt+Enter")
                                            .on_hover_text(
                                                "Copy the exact visible source message with its local conversation ID and event sequence. Does not send or admit context.",
                                            )
                                            .clicked()
                                        {
                                            if let Some((text, sequence)) =
                                                native_conversation_search::copyable_hit_markdown(
                                                    &native_search_index,
                                                    entry.id,
                                                    title,
                                                    message_index,
                                                )
                                            {
                                                ctx.copy_text(text);
                                                self.status = format!(
                                                    "copied visible message event #{sequence} from local conversation"
                                                );
                                            }
                                        }

                                        if ui
                                            .add_enabled(
                                                self.local_memory_draft.is_empty()
                                                    && !self.local_memory_command_pending,
                                                egui::Button::new("Stage as memory · Alt+M"),
                                            )
                                            .on_hover_text(
                                                "Stage the exact matched message in the memory editor. Record and Admit / Use once are separate explicit actions. Requires an empty memory draft.",
                                            )
                                            .clicked()
                                        {
                                            stage_native_memory_requested =
                                                Some((entry.id, message_index));
                                        }
                                    }
                                });
                            ui.add_space(2.0);
                        }
                            });
                        if !self.local_conversation_search_query.trim().is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} visible native conversation match{}",
                                    visible_native_search_matches,
                                    if visible_native_search_matches == 1 { "" } else { "es" },
                                ))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            if visible_native_search_matches == 0 {
                                ui.label(
                                    "No visible native chats match. Enable Show archived to include archived chats.",
                                );
                            }
                        }

                        if !historical_mode {
                            let (pending_count, queued_count) = self.coordination_workspace.inbox_counts(
                                &self.local_conversation_id.to_string(),
                            );
                            egui::CollapsingHeader::new(format!(
                                "Inbox · {} delivered · {} queued",
                                pending_count, queued_count
                            ))
                            .id_salt(("chatarium-selected-inbox", self.local_conversation_id))
                            .default_open(false)
                            .show(ui, |ui| {
                                let before = self.coordination_workspace.clone();
                                let changed = coordination_workspace::render_chat_inbox(
                                    ui,
                                    &mut self.coordination_workspace,
                                    &mut self.coordination_ui,
                                    &self.local_conversation_id.to_string(),
                                );
                                if changed {
                                    if let Err(error) = self.coordination_workspace.save_atomic(&self.coordination_store_path()) {
                                        self.coordination_workspace = before;
                                        self.status = format!("Failed to persist inbox deletion: {error}");
                                    }
                                }
                            });
                            ui.add_space(8.0);
                            egui::CollapsingHeader::new("Manage selected chat")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(format!("Selected: {local_conversation_title}"));
                                    ui.label("Rename this chat:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.local_conversation_rename)
                                            .hint_text("Enter a title…")
                                            .desired_width(ui.available_width()),
                                    );
                                    if ui.button("Save title").clicked() {
                                        rename_local_requested = true;
                                    }
                                    ui.label("Project folder for this chat (absolute path):");
                                    ui.add(
                                        egui::TextEdit::singleline(
                                            &mut self.local_conversation_project_directory,
                                        )
                                        .hint_text("/home/user/Code/project")
                                        .desired_width(ui.available_width()),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("Save folder").clicked() {
                                            save_project_directory_requested = true;
                                        }
                                        if ui.button("Clear folder").clicked() {
                                            self.local_conversation_project_directory.clear();
                                            save_project_directory_requested = true;
                                        }
                                    });
                                    ui.label(
                                        "Used as the agent's project context and Fish default cwd. An explicit tool cwd overrides it.",
                                    );
                                    egui::CollapsingHeader::new("OpenViking project memory (opt-in)")
                                        .default_open(false)
                                        .show(ui, |ui| {
                                            project_memory_permission_request = openviking_project_ui::render(
                                                ui,
                                                &self.local_conversation_catalog,
                                                &self.project_memory_grants,
                                                self.local_conversation_id,
                                            );
                                        });
                                    if ui.button("Archive selected chat").clicked() {
                                        archive_local_requested = true;
                                    }
                                    ui.separator();
                                    ui.label("Export visible messages");
                                    ui.horizontal(|ui| {
                                        if ui.button("Copy Markdown").clicked() {
                                            ctx.copy_text(native_transcript_export::markdown(
                                                self.local_conversation_id,
                                                &local_conversation_title,
                                                &local_display_messages,
                                            ));
                                            self.status = format!(
                                                "copied {} visible native messages as Markdown",
                                                local_display_messages.len(),
                                            );
                                        }
                                        if ui.button("Copy JSON").clicked() {
                                            match serde_json::to_string_pretty(
                                                &native_transcript_export::json_value(
                                                    self.local_conversation_id,
                                                    &local_conversation_title,
                                                    &local_display_messages,
                                                ),
                                            ) {
                                                Ok(export) => {
                                                    ctx.copy_text(export);
                                                    self.status = format!(
                                                        "copied {} visible native messages as JSON",
                                                        local_display_messages.len(),
                                                    );
                                                }
                                                Err(error) => {
                                                    self.status = format!(
                                                        "cannot format visible transcript export: {error}"
                                                    );
                                                }
                                            }
                                        }
                                    });
                                    ui.label(
                                        egui::RichText::new("Clipboard export is not an archive backup.")
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(139, 143, 153)),
                                    );
                                });
                        }

                        if ACCOUNT_MIRRORING_ENABLED && !self.remote_conversation_catalog.is_empty() {
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

                        if ACCOUNT_MIRRORING_ENABLED && !self.live_mirror_catalog.is_empty() {
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

                        if ACCOUNT_MIRRORING_ENABLED && !self.historical_catalog.is_empty() {
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
                        if ACCOUNT_MIRRORING_ENABLED {
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
                        }
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
                        ui.horizontal(|ui| {
                            ui.label("Slow warning after");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut self.slow_warning_seconds)
                                        .range(
                                            local_inference_settings::MIN_SLOW_WARNING_SECONDS
                                                ..=local_inference_settings::MAX_SLOW_WARNING_SECONDS,
                                        )
                                        .suffix(" s"),
                                )
                                .on_hover_text(
                                    "Warn if no assistant output arrives in this many seconds. Stored per local conversation.",
                                )
                                .changed()
                            {
                                self.persist_current_inference_settings();
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Read-only model attempts");
                            let mut attempts = self.inference_settings.read_retry_attempts();
                            if ui
                                .add(
                                    egui::DragValue::new(&mut attempts).range(
                                        local_inference_settings::MIN_READ_RETRY_ATTEMPTS
                                            ..=local_inference_settings::MAX_READ_RETRY_ATTEMPTS,
                                    ),
                                )
                                .on_hover_text(
                                    "1 disables automatic retries. Applies to subsequent read-only model discovery, never to inference POSTs.",
                                )
                                .changed()
                            {
                                match self.inference_settings.set_read_retry_attempts(attempts) {
                                    Ok(()) => self.persist_current_inference_settings(),
                                    Err(error) => self.status = error,
                                }
                            }
                        });

                        ui.add_space(8.0);
                        if ACCOUNT_MIRRORING_ENABLED && self.account_bridge_provider.is_some() {
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
                            if self.remote_models.is_empty() {
                                if self.model_list_pending {
                                    ui.label("Loading ChatGPT models…");
                                } else if model_retry_available(
                                    self.remote_connected(),
                                    self.model_list_pending,
                                    self.remote_models.is_empty(),
                                ) && ui
                                    .add_sized(
                                        [sidebar_control_width, 30.0],
                                        egui::Button::new("Retry model discovery"),
                                    )
                                    .clicked()
                                {
                                    self.request_chatgpt_models();
                                }
                            }
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
        if focus_native_search {
            ctx.memory_mut(|memory| memory.request_focus(native_search_id));
        }
        if focus_native_composer {
            ctx.memory_mut(|memory| {
                memory.request_focus(egui::Id::new("chatarium-message-composer"))
            });
        }
        // An explicitly highlighted owner may disappear after archive/filter
        // changes. Keep its ID: clearing to None would make Alt+Enter or
        // Enter silently fall back to a different conversation's first row.
        // Arrow navigation or an edited query explicitly selects anew.
        if native_search_has_focus {
            let mut clear_focus = false;
            let mut copy_hit_requested = false;
            let mut stage_hit_requested = false;
            let mut move_message_hit = 0_isize;
            ctx.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.local_conversation_search_selection =
                        native_conversation_search::move_identity_selection(
                            &native_search_matches,
                            self.local_conversation_search_selection,
                            1,
                        );
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.local_conversation_search_selection =
                        native_conversation_search::move_identity_selection(
                            &native_search_matches,
                            self.local_conversation_search_selection,
                            -1,
                        );
                }
                if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown) {
                    move_message_hit = 1;
                }
                if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp) {
                    move_message_hit = -1;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    select_local_requested =
                        native_conversation_search::activate_identity_selection(
                            &native_search_matches,
                            self.local_conversation_search_selection,
                        );
                    native_reader_target_message =
                        native_conversation_search::activate_identity_copy_selection(
                            &native_copy_candidates,
                            self.local_conversation_search_selection,
                        );
                }
                if input.consume_key(egui::Modifiers::ALT, egui::Key::Enter) {
                    copy_hit_requested = true;
                }
                if input.consume_key(egui::Modifiers::ALT, egui::Key::M) {
                    stage_hit_requested = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.local_conversation_search_query.clear();
                    self.local_conversation_search_selection = None;
                    clear_focus = true;
                }
            });
            if move_message_hit != 0 {
                let native_entries = self.local_conversation_catalog.entries();
                let rows = native_conversation_search::cached_rows(
                    ctx,
                    &native_search_index,
                    &native_entries,
                    self.show_archived_local_conversations,
                    &self.local_conversation_search_query,
                );
                let row_index = native_conversation_search::selected_identity_row(
                    &native_search_matches,
                    self.local_conversation_search_selection,
                );
                if let Some(row) = row_index.and_then(|index| rows.get(index)) {
                    let conversation_id = native_entries[row.catalog_index].id;
                    let choice_id = egui::Id::new(("chatarium-native-hit-choice", conversation_id));
                    let prior =
                        ctx.data_mut(|data| data.get_temp::<(String, Option<u64>)>(choice_id));
                    let ordinal = native_conversation_search::selected_hit_ordinal(
                        row,
                        native_search_index.messages(conversation_id),
                        prior
                            .filter(|(query, _)| query == &self.local_conversation_search_query)
                            .and_then(|(_, source_sequence)| source_sequence),
                    );
                    if let Some((next_ordinal, message_index)) =
                        native_conversation_search::cycle_matching_message(
                            row,
                            ordinal,
                            move_message_hit,
                        )
                    {
                        let selected_source_sequence =
                            native_conversation_search::selected_hit_sequence(
                                row,
                                native_search_index.messages(conversation_id),
                                next_ordinal,
                            );
                        ctx.data_mut(|data| {
                            data.insert_temp(
                                choice_id,
                                (
                                    self.local_conversation_search_query.clone(),
                                    selected_source_sequence,
                                ),
                            )
                        });
                        if let Some(candidate) =
                            row_index.and_then(|index| native_copy_candidates.get_mut(index))
                        {
                            candidate.1 = Some(message_index);
                        }
                    }
                }
            }
            if copy_hit_requested {
                if let Some((conversation_id, message_index)) =
                    native_conversation_search::activate_identity_copy_selection(
                        &native_copy_candidates,
                        self.local_conversation_search_selection,
                    )
                {
                    let messages = native_search_index.messages(conversation_id);
                    let title = self
                        .local_conversation_catalog
                        .entry(conversation_id)
                        .and_then(|entry| entry.title.clone())
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| derived_conversation_title(messages));
                    if let Some((text, sequence)) =
                        native_conversation_search::copyable_hit_markdown(
                            &native_search_index,
                            conversation_id,
                            &title,
                            message_index,
                        )
                    {
                        ctx.copy_text(text);
                        self.status = format!(
                            "copied visible message event #{sequence} from local conversation"
                        );
                    }
                }
            }
            if stage_hit_requested {
                stage_native_memory_requested =
                    native_conversation_search::activate_identity_copy_selection(
                        &native_copy_candidates,
                        self.local_conversation_search_selection,
                    );
            }
            if clear_focus {
                ctx.memory_mut(|memory| memory.surrender_focus(native_search_id));
            }
        }
        if let Some((source_conversation_id, message_index)) = stage_native_memory_requested {
            if self.local_memory_draft.is_empty() && !self.local_memory_command_pending {
                if let Some(staged) = native_conversation_search::stageable_hit(
                    &native_search_index,
                    source_conversation_id,
                    message_index,
                ) {
                    self.local_memory_draft = staged.text;
                    self.staged_local_memory_origin =
                        Some((staged.source_conversation_id, staged.source_event_sequence));
                    self.status = format!(
                        "staged message event #{} as a local memory draft; open Context & inference controls → Local memory to review and Record. No context admitted.",
                        staged.source_event_sequence,
                    );
                }
            }
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
            let before = self.local_conversation_id;
            self.create_local_conversation();
            if self.local_conversation_id != before {
                // The transcript snapshot was taken before this selection.
                // Force a fresh frame rather than waiting for another event.
                ctx.request_repaint();
                // Newly created conversations are empty: show them even if a
                // previous search filter would have hidden their sidebar row.
                self.local_conversation_search_query.clear();
                self.local_conversation_search_selection = None;
                ctx.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new("chatarium-message-composer"))
                });
            }
        } else if let Some(local_conversation_id) = select_local_requested {
            let can_switch = !self.local_conversation_busy();
            let before = self.local_conversation_id;
            self.activate_local_conversation(local_conversation_id);
            if self.local_conversation_id != before {
                // Sidebar selection also occurs after the initial projection.
                ctx.request_repaint();
            }
            if can_switch
                && self.local_conversation_id == local_conversation_id
                && !self.local_conversation_search_query.trim().is_empty()
            {
                let kind = if native_message_hit_conversations.contains(&local_conversation_id) {
                    native_conversation_search::NativeSearchMatch::Message
                } else {
                    native_conversation_search::NativeSearchMatch::Title
                };
                // The ordinary reader search fallback selects hit zero. If
                // this row's remembered source event vanished, even that
                // fallback would silently retarget the user; open the chat
                // without an automatic search selection instead.
                self.reader_search_query = if native_reader_source_stale {
                    String::new()
                } else {
                    native_conversation_search::reader_query_for_result(
                        &self.local_conversation_search_query,
                        kind,
                    )
                    .unwrap_or_default()
                };
                self.reader_search_hit = native_reader_target_message
                    .filter(|(owner, _)| *owner == local_conversation_id)
                    .and_then(|(_, position)| {
                        native_conversation_search::reader_hit_ordinal(
                            native_search_index.messages(local_conversation_id),
                            &self.reader_search_query,
                            position,
                        )
                    })
                    .or_else(|| (!self.reader_search_query.is_empty()).then_some(0));
                if self.reader_search_hit.is_some() {
                    self.reader_restore_pending = false;
                }
            }
        } else if let Some(local_conversation_id) = select_historical_requested {
            self.select_historical_conversation(local_conversation_id);
        } else if let Some(remote_conversation_id) = select_remote_requested {
            self.selected_historical_conversation = None;
            self.selected_remote_catalog_id = Some(remote_conversation_id);
            self.historical_messages.clear();
            self.historical_load_pending = None;
            self.status = "remote conversation selected; local mirror unavailable".to_owned();
        }
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

        let filtered_messages = if !historical_mode
            && !display_preferences.show_transport_trace
            && local_display_messages
                .iter()
                .any(|m| m.role == DisplayRole::Activity)
        {
            Some(
                local_display_messages
                    .iter()
                    .filter(|m| m.role != DisplayRole::Activity)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        let display_messages: &[DisplayMessage] = if historical_mode {
            &self.historical_messages
        } else {
            filtered_messages
                .as_deref()
                .unwrap_or(local_display_messages)
        };

        let reader_key = self.reader_conversation_key();
        let restore_reader_offset = if self.reader_restore_pending {
            self.reader_restore_pending = false;
            Some(self.reader_positions.position(&reader_key))
        } else {
            None
        };
        let reader_hit_targets = if historical_mode || filtered_messages.is_some() {
            std::sync::Arc::new(
                display_messages
                    .iter()
                    .enumerate()
                    .flat_map(|(message_index, message)| {
                        offline_reader::search_hits(&message.text, &self.reader_search_query)
                            .into_iter()
                            .map(move |(start, end)| (message_index, start, end))
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            native_conversation_search::cached_reader_hits(
                ctx,
                &native_search_index,
                self.local_conversation_id,
                &self.reader_search_query,
            )
        };
        if self
            .reader_search_hit
            .is_some_and(|hit| hit >= reader_hit_targets.len())
        {
            self.reader_search_hit = reader_hit_targets.is_empty().then_some(0);
        }
        let mut next_reader_hit = false;
        let mut previous_reader_hit = false;
        let mut reader_home_requested = false;
        let mut reader_end_requested = false;
        let mut reader_page_delta = 0.0_f32;
        const READER_BOTTOM_SCROLL_OFFSET: f32 = 100_000_000.0;
        let transcript_scroll_id = egui::Id::new("transcript-reader-scroll");
        // ScrollArea adds its parent Ui ID to this salt. Resolve its actual
        // persistent state key only after the parent Ui has been created.
        let mut reader_scroll_state_id = transcript_scroll_id;
        if conversation_keybindings::may_consume_reader_navigation(ctx.wants_keyboard_input()) {
            ctx.input_mut(|input| {
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::N) {
                    next_reader_hit = true;
                }
                if input.consume_key(egui::Modifiers::CTRL, egui::Key::P) {
                    previous_reader_hit = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                    reader_page_delta = 480.0;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                    reader_page_delta = -480.0;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                    reader_home_requested = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                    reader_end_requested = true;
                }
            });
        }
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
        let mut virtual_anchor_delta = None;

        let virtual_id = egui::Id::new(("chatarium-native-virtual-layout", &reader_key));
        // Egui clones temporary values on lookup. Keep the large height and
        // signature arrays behind a shared handle rather than copying them
        // every redraw, including when paging makes virtualization inactive.
        let virtual_layout_state = ctx
            .data_mut(|data| {
                data.get_temp::<Arc<Mutex<virtual_reader::VirtualReaderLayout>>>(virtual_id)
            })
            .unwrap_or_else(|| Arc::new(Mutex::new(Default::default())));
        let mut virtual_layout = virtual_layout_state
            .lock()
            .expect("virtual reader cache lock poisoned");

        // Paging is a local UI projection, not a change to journal contents,
        // inference context or authorization. Keep restored full-reader
        // positions intact when the user already has a saved scroll position.
        let paging_id = egui::Id::new(("chatarium-native-reader-page", &reader_key));
        let mut reader_window = ctx
            .data_mut(|data| data.get_temp::<offline_reader::ReaderWindow>(paging_id))
            .unwrap_or_else(|| {
                let enable = !historical_mode
                    && display_messages.len() >= offline_reader::READER_AUTO_PAGE_THRESHOLD
                    && restore_reader_offset.unwrap_or(0.0) <= 0.0;
                offline_reader::ReaderWindow::new(display_messages.len(), enable)
            });
        reader_window.sync(
            display_messages.len(),
            &self.reader_search_query,
            active_reader_message,
        );
        if reader_window.enabled {
            if reader_home_requested {
                reader_window.first();
            }
            if reader_end_requested {
                reader_window.latest(display_messages.len());
            }
        }

        // Cache the durable MCP projection by journal length, not by frame.
        // This status remains visible after reopening a conversation, while
        // never using a stale in-memory agent flow as execution authority.
        let inline_mcp = if historical_mode {
            None
        } else {
            let key = egui::Id::new(("inline-mcp-status", self.local_conversation_id));
            let cached =
                ctx.data_mut(|data| data.get_temp::<(usize, Option<InlineMcpStatus>)>(key));
            if let Some((count, status)) = cached {
                if count == self.events.len() {
                    status
                } else {
                    let status =
                        project_inline_mcp_status(&self.events, self.local_conversation_id);
                    ctx.data_mut(|data| data.insert_temp(key, (self.events.len(), status.clone())));
                    status
                }
            } else {
                let status = project_inline_mcp_status(&self.events, self.local_conversation_id);
                ctx.data_mut(|data| data.insert_temp(key, (self.events.len(), status.clone())));
                status
            }
        };

        // Collect MCP commands while drawing. Mutating Chatarium from inside
        // the reader closure conflicts with its borrowed transcript messages.
        // Dispatch only after the entire central panel has finished rendering.
        let mut inline_mcp_decision: Option<(RouteId, RouteUserDecision)> = None;
        let mut inline_mcp_run: Option<(ToolCallId, RouteId)> = None;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 27, 35))
                    .stroke(egui::Stroke::new(
                        1.5,
                        egui::Color32::from_rgb(75, 91, 118),
                    ))
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                reader_scroll_state_id = ui.make_persistent_id(egui::Id::new(transcript_scroll_id));
                if reader_page_delta != 0.0 {
                    adjust_reader_scroll(ctx, reader_scroll_state_id, reader_page_delta);
                }
                if reader_home_requested {
                    set_reader_scroll(ctx, reader_scroll_state_id, 0.0);
                }
                if reader_end_requested {
                    // A maximum finite float can overflow egui's scroll
                    // geometry and unexpectedly move to the beginning.
                    // A bounded, finite offset is enough to clamp to the
                    // actual bottom of realistic native conversations.
                    set_reader_scroll(ctx, reader_scroll_state_id, READER_BOTTOM_SCROLL_OFFSET);
                }
                // A rejected model-authored MCP envelope has no route, no
                // purple card and no approval prompt. Explain that fact in the
                // conversation instead of hiding it in advanced status logs.
                if !historical_mode {
                    if let Some(problem) = self
                        .status
                        .strip_prefix("Malformed agent MCP proposal rejected: ")
                    {
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(56, 35, 39))
                            .stroke(egui::Stroke::new(
                                1.0,
                                egui::Color32::from_rgb(181, 103, 107),
                            ))
                            .corner_radius(egui::CornerRadius::same(9))
                            .inner_margin(egui::Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "MCP request rejected before execution",
                                    )
                                    .strong(),
                                );
                                ui.label(problem);
                                ui.label(
                                    "Nothing was dispatched. No approval prompt exists for this response.",
                                );
                            });
                        ui.add_space(12.0);
                    }
                }
                // Live in-chat MCP controls. Show the actual durable stage
                // above the transcript instead of hiding approval in Advanced.
                if let Some(call) = inline_mcp.as_ref() {
                    let active = self.agent_mcp_flow.as_ref().is_some_and(|flow| {
                        flow.conversation_id == self.local_conversation_id
                            && flow.pending_call == Some(call.call_id)
                    });
                    let tint = match call.stage {
                        mcp_call_review::CallReviewStage::AwaitingDecision
                        | mcp_call_review::CallReviewStage::ApprovedForSeparateReview => {
                            egui::Color32::from_rgb(54, 45, 30)
                        }
                        mcp_call_review::CallReviewStage::DispatchedAwaitingOutcome
                        | mcp_call_review::CallReviewStage::Unbound => {
                            egui::Color32::from_rgb(38, 44, 59)
                        }
                        mcp_call_review::CallReviewStage::Denied => {
                            egui::Color32::from_rgb(55, 37, 41)
                        }
                        mcp_call_review::CallReviewStage::OutcomeRecorded => {
                            if call.successful == Some(true) {
                                egui::Color32::from_rgb(30, 49, 39)
                            } else {
                                egui::Color32::from_rgb(55, 37, 41)
                            }
                        }
                    };
                    egui::Frame::default()
                        .fill(tint)
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(129, 122, 105),
                        ))
                        .corner_radius(egui::CornerRadius::same(9))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(format!(
                                    "MCP · {} :: {} · call #{}",
                                    call.provider,
                                    call.operation,
                                    call.call_id.get()
                                ))
                                .strong()
                                .size(12.0),
                            );
                            if matches!(
                                call.stage,
                                mcp_call_review::CallReviewStage::AwaitingDecision
                                    | mcp_call_review::CallReviewStage::ApprovedForSeparateReview
                            ) {
                                ui.collapsing("Review exact tool arguments before approval", |ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(140.0)
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(&call.arguments)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                });
                                if call.provider == local_http_mcp::MATHEMATICA {
                                    ui.label("Mathematica executes Wolfram code with your Linux user privileges.");
                                } else if call.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME
                                    && call.operation == host_shell_mcp::OPERATION
                                {
                                    ui.label("Host shell commands execute as your Linux user, without a sandbox.");
                                }
                            }
                            match call.stage {
                                mcp_call_review::CallReviewStage::AwaitingDecision
                                    if call.automatically_authorized =>
                                {
                                    ui.label(
                                        "Authorized by your saved tool policy; dispatch is pending. No result recorded yet.",
                                    );
                                }
                                mcp_call_review::CallReviewStage::AwaitingDecision => {
                                    ui.label(
                                        "Tool requested, NOT executed. Your approval is required.",
                                    );
                                    ui.horizontal(|ui| {
                                        let enabled = !self.tool_command_pending
                                            && self.persist_tx.is_some();
                                        if ui
                                            .add_enabled(
                                                enabled,
                                                egui::Button::new("Allow this call"),
                                            )
                                            .clicked()
                                        {
                                            inline_mcp_decision =
                                                Some((call.route_id, RouteUserDecision::Allow));
                                        }
                                        if ui
                                            .add_enabled(enabled, egui::Button::new("Deny"))
                                            .clicked()
                                        {
                                            inline_mcp_decision =
                                                Some((call.route_id, RouteUserDecision::Deny));
                                        }
                                    });
                                }
                                mcp_call_review::CallReviewStage::ApprovedForSeparateReview => {
                                    ui.label(
                                        "Approved, but NOT executed yet. Run once to continue.",
                                    );
                                    if ui
                                        .add_enabled(
                                            !self.tool_command_pending && self.persist_tx.is_some(),
                                            egui::Button::new("Run approved tool once"),
                                        )
                                        .clicked()
                                    {
                                        inline_mcp_run = Some((call.call_id, call.route_id));
                                    }
                                }
                                mcp_call_review::CallReviewStage::DispatchedAwaitingOutcome => {
                                    ui.label(if active {
                                        "Dispatched; waiting for the tool result. Do not send a duplicate call."
                                    } else {
                                        "Dispatched, but no result recorded. Outcome is uncertain; do not retry this call."
                                    });
                                }
                                mcp_call_review::CallReviewStage::OutcomeRecorded => {
                                    ui.label(if call.successful == Some(true) {
                                        if active {
                                            "Tool completed. Returning the recorded result to the agent."
                                        } else {
                                            "Tool completed; result recorded. No active agent continuation is confirmed."
                                        }
                                    } else {
                                        "Tool failed; error recorded in the purple MCP result. No automatic retry."
                                    });
                                }
                                mcp_call_review::CallReviewStage::Denied => {
                                    ui.label("Tool denied. No execution was authorized.");
                                }
                                mcp_call_review::CallReviewStage::Unbound => {
                                    ui.label("Tool request recorded, but its route is not bound. No execution.");
                                }
                            }
                            if self.tool_command_pending {
                                ui.spinner();
                            }
                        });
                    ui.add_space(12.0);
                }
                if !historical_mode && display_messages.len() > offline_reader::READER_PAGE_SIZE {
                    ui.horizontal_wrapped(|ui| {
                        let was_virtualized = reader_window.virtualized;
                        ui.checkbox(&mut reader_window.virtualized, "Continuous reader");
                        if reader_window.virtualized != was_virtualized {
                            if reader_window.virtualized {
                                reader_window.enabled = false;
                            }
                            set_reader_scroll(ctx, reader_scroll_state_id, READER_BOTTOM_SCROLL_OFFSET);
                            ctx.request_repaint();
                        }
                        let was_enabled = reader_window.enabled;
                        ui.checkbox(&mut reader_window.enabled, "Paged reader");
                        if reader_window.enabled {
                            reader_window.virtualized = false;
                        }
                        if reader_window.enabled != was_enabled {
                            if reader_window.enabled {
                                reader_window.latest(display_messages.len());
                                if let Some(target) = active_reader_message {
                                    reader_window.reveal(display_messages.len(), target);
                                }
                            }
                            set_reader_scroll(ctx, reader_scroll_state_id, READER_BOTTOM_SCROLL_OFFSET);
                        }
                        if reader_window.virtualized {
                            ui.label("Virtualized · scroll for older messages");
                        }
                        if reader_window.enabled {
                            let range = reader_window.range(display_messages.len());
                            if ui
                                .add_enabled(range.start > 0, egui::Button::new("← Earlier"))
                                .clicked()
                            {
                                reader_window.earlier();
                                set_reader_scroll(ctx, reader_scroll_state_id, 0.0);
                            }
                            if ui
                                .add_enabled(
                                    range.end < display_messages.len(),
                                    egui::Button::new("Later →"),
                                )
                                .clicked()
                            {
                                reader_window.later(display_messages.len());
                                set_reader_scroll(ctx, reader_scroll_state_id, 0.0);
                            }
                            let range = reader_window.range(display_messages.len());
                            ui.label(format!(
                                "{}–{} / {} messages",
                                range.start + 1,
                                range.end,
                                display_messages.len(),
                            ));
                        }
                    });
                }
                let mut reader_scroll = egui::ScrollArea::vertical()
                    .id_salt(transcript_scroll_id)
                    .stick_to_bottom(!reader_home_requested)
                    .auto_shrink([false, false]);
                // An explicit reader navigation command must take priority
                // over the ScrollArea's previous sticky-bottom state.
                if reader_home_requested {
                    reader_scroll = reader_scroll.vertical_scroll_offset(0.0);
                } else if reader_end_requested {
                    reader_scroll = reader_scroll.vertical_scroll_offset(READER_BOTTOM_SCROLL_OFFSET);
                }
                if let Some(offset) = restore_reader_offset {
                    // Full-reader pixel offsets refer to an entirely different
                    // content extent, so do not apply them to a virtual layout.
                    let resolved_offset = if reader_window.virtualized {
                        virtual_layout.saved_view_offset().unwrap_or(READER_BOTTOM_SCROLL_OFFSET)
                    } else if reader_window.enabled {
                        READER_BOTTOM_SCROLL_OFFSET
                    } else {
                        offset
                    };
                    reader_scroll = reader_scroll.vertical_scroll_offset(resolved_offset);
                }
                let reader_output = reader_scroll.show_viewport(ui, |ui, viewport| {
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
                            if reader_window.virtualized && !historical_mode && filtered_messages.is_none() {
                                let source = native_search_index
                                    .message_projection(self.local_conversation_id);
                                virtual_layout.sync(source, ui.available_width());
                                let list_top = ui.cursor().top();
                                let content_top = ui.max_rect().top();
                                let header_height = list_top - content_top;
                                let anchor_row =
                                    virtual_layout.row_at(viewport.min.y - header_height);
                                let anchor_before = anchor_row.map(|row| virtual_layout.offset(row));
                                let near_tail = viewport.max.y - header_height
                                    >= virtual_layout.total_height() - 24.0;
                                let mut search_scrolled = false;
                                if let Some(offset) = virtual_layout.new_search_offset(
                                    &self.reader_search_query,
                                    active_reader_message,
                                ) {
                                    search_scrolled = true;
                                    // ScrollArea::end owns its state; mutating
                                    // that state inside show_viewport would be
                                    // overwritten when the callback returns.
                                    // This request survives and can target a
                                    // bubble that has not been rendered yet.
                                    let target = egui::Rect::from_min_size(
                                        egui::pos2(ui.max_rect().left(), list_top + offset),
                                        egui::vec2(2.0, 32.0),
                                    );
                                    ui.scroll_to_rect(target, Some(egui::Align::Center));
                                    ctx.request_repaint();
                                }
                                let range = virtual_layout.visible_range(
                                    viewport.min.y - header_height,
                                    viewport.max.y - header_height,
                                );
                                let total_height = virtual_layout.total_height();
                                ui.set_min_height(header_height + total_height);
                                if !range.is_empty() {
                                    let top = list_top + virtual_layout.offset(range.start);
                                    let bottom = list_top + virtual_layout.offset(range.end);
                                    let rect = egui::Rect::from_x_y_ranges(
                                        ui.max_rect().x_range(),
                                        top..=bottom,
                                    );
                                    ui.scope_builder(
                                        egui::UiBuilder::new().max_rect(rect),
                                        |viewport_ui| {
                                            for message_index in range {
                                                viewport_ui.push_id(
                                                    (
                                                        "native-virtual-bubble",
                                                        message_index,
                                                        display_messages[message_index].sequence,
                                                    ),
                                                    |row_ui| {
                                                        let before = row_ui.cursor().top();
                                                        draw_reader_bubble(
                                                            row_ui,
                                                            &display_messages[message_index],
                                                            &self.reader_search_query,
                                                            false,
                                                            ctx,
                                                        );
                                                        row_ui.add_space(12.0);
                                                        let actual_height =
                                                            row_ui.cursor().top() - before;
                                                        virtual_layout.measure(
                                                            message_index,
                                                            actual_height,
                                                        );
                                                    },
                                                );
                                            }
                                        },
                                    );
                                    if virtual_layout.finish_measurements() {
                                        // The overscan neighborhood may include
                                        // rows above the viewport. Their measured
                                        // growth would otherwise push the reader's
                                        // current text downward. Compensate only
                                        // while browsing, not during a search jump
                                        // or when following the transcript tail.
                                        if !near_tail && !search_scrolled {
                                            if let (Some(row), Some(before)) =
                                                (anchor_row, anchor_before)
                                            {
                                                let delta = virtual_layout.offset(row) - before;
                                                if delta.abs() > 0.5 {
                                                    virtual_anchor_delta = Some(delta);
                                                }
                                            }
                                        }
                                        ctx.request_repaint();
                                    }
                                }
                            } else {
                                let window = if historical_mode {
                                    0..display_messages.len()
                                } else {
                                    reader_window.range(display_messages.len())
                                };
                                for (message_index, message) in display_messages
                                    .iter()
                                    .enumerate()
                                    .skip(window.start)
                                    .take(window.end - window.start)
                                {
                                    let message_is_active_hit =
                                        active_reader_message == Some(message_index);
                                    let response = draw_reader_bubble(
                                        ui,
                                        message,
                                        &self.reader_search_query,
                                        message_is_active_hit,
                                        ctx,
                                    );
                                    if message_is_active_hit {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    ui.add_space(12.0);
                                }
                            }
                        }
                        if !historical_mode {
                            let consumed = self.coordination_workspace.consumed_for(
                                &self.local_conversation_id.to_string(),
                            );
                            if !consumed.is_empty() {
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new("COORDINATION · SUBMITTED TO MODEL CONTEXT")
                                    .size(11.0)
                                    .strong()
                                    .color(egui::Color32::from_rgb(186, 199, 220)));
                                ui.add_space(5.0);
                                for message in &consumed {
                                    draw_consumed_coordination_bubble(ui, message, &display_preferences, ctx);
                                    ui.add_space(12.0);
                                }
                            }
                        }
                        if !historical_mode {
                            let conversation_id = self.local_conversation_id.to_string();
                            if let Some(failed) = self.delivery.for_conversation(&conversation_id).last() {
                                if matches!(
                                    failed.phase,
                                    delivery_telemetry::Phase::Failed
                                        | delivery_telemetry::Phase::Unconfirmed
                                ) {
                                    egui::CollapsingHeader::new("Last request failed · inspect error")
                                        .id_salt(("chatarium-last-request-error", self.local_conversation_id))
                                        .default_open(true)
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("Request ID: {}", failed.request_id))
                                                    .small()
                                                    .weak(),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    failed.error.as_deref().unwrap_or(
                                                        "The request did not complete; no transport error detail was supplied."
                                                    ),
                                                )
                                                .wrap(),
                                            );
                                        });
                                }
                            }
                        }
                        if !historical_mode && display_preferences.show_delivery_history {
                            let conversation_id = self.local_conversation_id.to_string();
                            draw_delivery_timeline(
                                ui,
                                &self.delivery,
                                &conversation_id,
                                self.slow_warning_seconds,
                            );
                        }
                    });
                reader_output_offset = Some(reader_output.state.offset.y);
            });

        // The UI callback no longer owns any borrowed conversation messages.
        // Invoke only the action clicked in this frame; all durable gate checks
        // and one-shot dispatch reservations still occur in their normal path.
        if let Some((route_id, decision)) = inline_mcp_decision {
            self.decide_tool_call_route(route_id, decision);
        } else if let Some((call_id, route_id)) = inline_mcp_run {
            if let Some(call) = inline_mcp
                .as_ref()
                .filter(|call| call.call_id == call_id && call.route_id == route_id)
            {
                if call.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME {
                    if call.operation == host_shell_mcp::OPERATION {
                        self.dispatch_host_shell(route_id);
                    } else {
                        self.dispatch_builtin_hello(route_id);
                    }
                } else {
                    self.execute_approved_tool(call_id);
                }
            }
        }

        if reader_window.virtualized {
            if let Some(offset) = reader_output_offset {
                let resolved = if let Some(delta) = virtual_anchor_delta {
                    // ScrollArea has finished writing its state. Apply the
                    // correction afterwards, never from inside its callback.
                    let corrected = (offset + delta).max(0.0);
                    set_reader_scroll(ctx, reader_scroll_state_id, corrected);
                    ctx.request_repaint();
                    corrected
                } else {
                    offset
                };
                virtual_layout.remember_view_offset(resolved);
            }
        }
        drop(virtual_layout);
        ctx.data_mut(|data| {
            data.insert_temp(paging_id, reader_window.clone());
            data.insert_temp(virtual_id, virtual_layout_state);
        });
        let selection_changed = self.reader_search_hit != reader_hit_from_button;
        self.reader_search_hit = reader_hit_from_button;
        if selection_changed {
            ctx.request_repaint();
        }
        if let Some(offset) =
            reader_output_offset.filter(|_| !reader_window.enabled && !reader_window.virtualized)
        {
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
