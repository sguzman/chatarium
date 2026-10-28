//! Native composer controls, preserved from the frame update implementation.
use super::*;

impl ChatariumApp {
    pub(super) fn render_composer_panel(
        &mut self,
        ctx: &egui::Context,
        historical_mode: bool,
        remote_catalog_selected: bool,
        selected_live_mirror: bool,
        selected_historical_id: Option<LocalConversationId>,
        display_preferences: &display_preferences::DisplayPreferences,
        mirror_remote_requested: &mut Option<String>,
        sync_live_requested: &mut Option<LocalConversationId>,
    ) {
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
                                egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                    "Remote conversation is not mirrored locally"
                                } else if selected_live_mirror {
                                    "Live mirror is read-only for now"
                                } else {
                                    "Historical snapshot is read-only"
                                })
                                .strong()
                                .color(egui::Color32::from_rgb(221, 223, 229)),
                            );
                            ui.label(
                                egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                    "Selecting a catalog row is local-only. Use the explicit mirror action to capture this conversation through ChatGPT."
                                } else if selected_live_mirror {
                                    "Chatarium can refresh this real ChatGPT thread through your browser session. Same-thread write-back remains evidence-gated, so the composer stays disabled."
                                } else {
                                    "Sync can upgrade this imported lineage to a validated live mirror without creating a parallel conversation. Same-thread write-back remains a separate interoperability problem."
                                })
                                .size(11.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            );
                            ui.add_space(8.0);
                            if ACCOUNT_MIRRORING_ENABLED {
                            ui.horizontal(|ui| {
                                let pending = self.live_mirror_pending == selected_historical_id;
                                let can_sync = (selected_historical_id.is_some()
                                    || remote_catalog_selected)
                                    && self.persist_tx.is_some()
                                    && self.account_bridge_provider.is_some()
                                    && self.live_mirror_pending.is_none();
                                let label = if pending {
                                    "Syncing…"
                                } else if remote_catalog_selected && !selected_live_mirror {
                                    "Mirror from ChatGPT"
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
                                    if remote_catalog_selected && !selected_live_mirror {
                                        *mirror_remote_requested =
                                            self.selected_remote_catalog_id.clone();
                                    } else {
                                        *sync_live_requested = selected_historical_id;
                                    }
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
                            } else {
                                ui.label("ChatGPT history mirroring is paused; saved snapshots remain read-only.");
                            }
                        });
                    return;
                }

                let composer_fill = if self.persist_tx.is_some() {
                    egui::Color32::from_rgb(31, 33, 39)
                } else {
                    egui::Color32::from_rgb(45, 29, 31)
                };

                ui.add_space(8.0);
                if let Some((filename, byte_len)) = self
                    .staged_attachment
                    .as_ref()
                    .map(|attached| (attached.filename.clone(), attached.byte_len))
                {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Attachment: {} · {} bytes (pending durable commit)",
                                filename, byte_len,
                            ))
                            .size(10.0),
                        );
                        if ui
                            .add_enabled(
                                self.commit_in_flight.is_none(),
                                egui::Button::new("Remove attachment"),
                            )
                            .clicked()
                        {
                            self.staged_attachment = None;
                        }
                    });
                }
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
                            .id(egui::Id::new("chatarium-message-composer"))
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
                // Keep the editable Markdown source intact and provide a
                // live rendered preview, using the same block renderer as
                // both user and agent transcript messages after Send.
                let draft_has_markdown = self.draft.contains("**")
                    || self.draft.contains('`')
                    || self.draft.lines().any(|line| {
                        line.starts_with("# ")
                            || line.starts_with("## ")
                            || line.starts_with("- ")
                            || line.starts_with("> ")
                    });
                if display_preferences.render_markdown && draft_has_markdown {
                    egui::CollapsingHeader::new("Rendered Markdown preview")
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(160.0)
                                .show(ui, |ui| {
                                    for block in offline_reader::parse_markdown(&self.draft) {
                                        render_markdown_block(ui, &block, "", false, ctx);
                                        ui.add_space(4.0);
                                    }
                                });
                        });
                }

                let remote_turn_idle =
                    self.pending_remote_turn.is_none() && self.active_remote_turn.is_none();
                let behavior_ready_for_send =
                    !self.remote_connected() || self.current_behavior_request_patch().is_ok();
                let remote_ready_for_send = !self.remote_connected()
                    || (self.selected_model.is_some()
                        && remote_turn_idle
                        && !self.capability_probe.running()
                        && behavior_ready_for_send);
                let can_commit = self.persist_tx.is_some()
                    && (self.staged_attachment.is_none() || self.remote_connected())
                    && self.commit_in_flight.is_none()
                    && remote_ready_for_send
                    && !self.draft.trim().is_empty();
                let composer_shortcut_ready =
                    conversation_keybindings::may_submit_composer_shortcut(
                        can_commit,
                        response.has_focus(),
                    );
                let commit_shortcut = composer_shortcut_ready
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
                        // right_to_left lays out Send first (rightmost), then
                        // the attach menu immediately to its left.
                        ui.menu_button(egui::RichText::new("+").size(20.0).strong(), |ui| {
                            ui.set_min_width(250.0);
                            ui.label(egui::RichText::new("Attach a local file").strong());
                            ui.label("Drop a file into Chatarium, or enter its path:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.attachment_path_draft)
                                    .hint_text("Absolute file path")
                                    .desired_width(260.0),
                            );
                            if ui
                                .add_enabled(
                                    self.commit_in_flight.is_none()
                                        && !self.attachment_path_draft.trim().is_empty(),
                                    egui::Button::new("Stage attachment"),
                                )
                                .clicked()
                            {
                                let path = std::path::PathBuf::from(
                                    self.attachment_path_draft.trim(),
                                );
                                self.stage_attachment(&path);
                                ui.close();
                            }
                            if self.staged_attachment.is_some()
                                && ui.button("Remove staged attachment").clicked()
                            {
                                self.staged_attachment = None;
                                ui.close();
                            }
                            ui.label("One file per turn, up to 128 KiB.");
                        });

                        if let Some(target_request_id) = self
                            .active_remote_turn
                            .as_ref()
                            .map(|active| active.request_id.clone())
                        {
                            if ui.button("Stop generation").clicked() {
                                match self.remote.send(
                                    siwc_bridge::BridgeCommand::CancelResponse {
                                        target_request_id,
                                    },
                                ) {
                                    Ok(()) => {
                                        self.agent_mcp_flow = None;
                                        self.status =
                                            "cancelling active ChatGPT response and MCP continuation…".to_owned();
                                    }
                                    Err(error) => {
                                        self.status =
                                            format!("failed to cancel active response: {error}");
                                    }
                                }
                            }
                        }

                        if self.agent_mcp_flow.is_some()
                            && ui.button("Stop MCP continuation")
                                .on_hover_text("Stop requesting new model/tool steps. A subprocess whose one-shot permit was already consumed cannot be undone; its eventual outcome remains audited.")
                                .clicked()
                        {
                            self.agent_mcp_flow = None;
                            self.status = "MCP continuation stopped. Dispatched tools remain subject to their durable one-shot outcome.".to_owned();
                        }
                        if self.commit_in_flight.is_some() {
                            ui.spinner();
                        }
                    });
                });
            });
    }
}
