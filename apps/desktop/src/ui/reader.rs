//! Native transcript reader: projection, search navigation, virtualization and scroll state.
//! The existing render path is preserved; expensive work must remain cached/off-thread.
use super::*;

impl ChatariumApp {
    pub(super) fn render_conversation_reader(
        &mut self,
        ctx: &egui::Context,
        historical_mode: bool,
        selected_live_mirror: bool,
        remote_catalog_selected: bool,
        display_preferences: &display_preferences::DisplayPreferences,
        local_display_messages: &[DisplayMessage],
        native_search_index: &Arc<native_conversation_search::NativeConversationSearchIndex>,
    ) {
        // The activity-filtered view may contain thousands of large message
        // bodies. Prepare it on a worker, not on the render/UI thread.
        let filtered_messages = if !historical_mode && !display_preferences.show_transport_trace {
            match native_conversation_search::background_without_activity(
                ctx,
                native_search_index,
                self.local_conversation_id,
            ) {
                Ok(Some(prepared)) => {
                    // No activity messages: the worker returned the original
                    // Arc and the virtualized native reader stays available.
                    if native_search_index
                        .message_projection(self.local_conversation_id)
                        .is_some_and(|original| Arc::ptr_eq(&prepared, &original))
                    {
                        None
                    } else {
                        Some(prepared)
                    }
                }
                Ok(None) => {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.spinner();
                        ui.label("Preparing local transcript…");
                    });
                    return;
                }
                Err(error) => {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.label(format!("Local transcript unavailable: {error}"));
                    });
                    return;
                }
            }
        } else {
            None
        };
        let display_messages: &[DisplayMessage] = if historical_mode {
            &self.historical_messages
        } else {
            filtered_messages
                .as_ref()
                .map(|projection| projection.as_slice())
                .unwrap_or(local_display_messages)
        };

        let reader_key = self.reader_conversation_key();
        let restore_reader_offset = if self.reader_restore_pending {
            self.reader_restore_pending = false;
            Some(self.reader_positions.position(&reader_key))
        } else {
            None
        };

        // Historical import browsing remains isolated from native search.
        // Native reader search never scans large projections on a UI frame.
        let native_projection = if historical_mode {
            None
        } else {
            filtered_messages
                .clone()
                .or_else(|| native_search_index.message_projection(self.local_conversation_id))
        };
        let reader_hit_outcome = if historical_mode {
            Ok(Some(Arc::new(if self.reader_search_query.trim().is_empty() {
                Vec::new()
            } else {
                display_messages
                    .iter()
                    .enumerate()
                    .flat_map(|(message_index, message)| {
                        offline_reader::search_hits(&message.text, &self.reader_search_query)
                            .into_iter()
                            .map(move |(start, end)| (message_index, start, end))
                    })
                    .collect::<Vec<_>>()
            })))
        } else if let Some(source) = &native_projection {
            native_conversation_search::background_reader_hits(
                ctx,
                source,
                &self.reader_search_query,
            )
        } else {
            Ok(Some(Arc::new(Vec::new())))
        };
        let reader_hits_pending = matches!(&reader_hit_outcome, Ok(None));
        let reader_hits_error = reader_hit_outcome.as_ref().err().cloned();
        let reader_hit_targets = reader_hit_outcome
            .ok()
            .flatten()
            .unwrap_or_else(|| Arc::new(Vec::new()));
        if reader_hits_pending || reader_hits_error.is_some() {
            // Never navigate an earlier query's hit index during async replay.
            self.reader_search_hit = None;
        }
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
                        if reader_hits_pending {
                            ui.label("Searching transcript…");
                            ui.add_space(8.0);
                        } else if let Some(error) = &reader_hits_error {
                            ui.label(format!("Transcript search unavailable: {error}"));
                            ui.add_space(8.0);
                        }
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
    }
}
