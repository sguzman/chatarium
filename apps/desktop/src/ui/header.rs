//! Conversation header UI; preserves existing widget identities and layout.
use super::*;

impl ChatariumApp {
    pub(super) fn render_conversation_header(
        &mut self,
        ctx: &egui::Context,
        historical_mode: bool,
        header_conversation_title: &String,
        remote_catalog_selected: bool,
        selected_live_mirror: bool,
        local_conversation_title: &String,
        local_display_messages: &[DisplayMessage],
    ) {
        egui::TopBottomPanel::top("conversation_header")
            .resizable(false)
            .exact_height(142.0)
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(22, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Reserve a dedicated right-side column for status and
                    // actions so long chat titles never overlap its controls.
                    let header_width = ui.available_width();
                    let right_column_width = (header_width * 0.40).clamp(170.0, 224.0);
                    let title_width =
                        (header_width - right_column_width - ui.spacing().item_spacing.x).max(80.0);
                    ui.vertical(|ui| {
                        ui.set_width(title_width);
                        ui.set_max_width(title_width);
                        ui.horizontal(|ui| {
                            if !historical_mode {
                                if let Some(entry) = self.local_conversation_catalog.entry(self.local_conversation_id) {
                                    let rgb = entry.accent_rgb();
                                    ui.colored_label(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]), "●");
                                }
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(header_conversation_title.as_str())
                                        .size(19.0)
                                        .strong()
                                        .color(egui::Color32::from_rgb(238, 239, 244)),
                                )
                                .truncate(),
                            );
                        });
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                "Remote catalog item · not mirrored locally"
                            } else if selected_live_mirror {
                                "Local durable mirror · read-only"
                            } else if historical_mode {
                                "Historical account-export snapshot · read-only"
                            } else if self.remote_connected() {
                                "Durable on this machine · authenticated with ChatGPT"
                            } else {
                                "Durable on this machine · connect ChatGPT to enable remote turns"
                            })
                                .size(11.0)
                                .color(egui::Color32::from_rgb(139, 143, 153)),
                            )
                            .truncate(),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(right_column_width);
                            ui.set_max_width(right_column_width);
                            let connected = self.remote_connected();
                            egui::Frame::default()
                                .fill(if selected_live_mirror {
                                    egui::Color32::from_rgb(24, 52, 37)
                                } else if historical_mode {
                                    egui::Color32::from_rgb(39, 42, 49)
                                } else if connected && self.network_health.is_offline() {
                                    egui::Color32::from_rgb(62, 31, 34)
                                } else if connected {
                                    egui::Color32::from_rgb(24, 52, 37)
                                } else {
                                    egui::Color32::from_rgb(48, 42, 26)
                                })
                                .corner_radius(egui::CornerRadius::same(12))
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_width(right_column_width - 24.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            egui::RichText::new(if remote_catalog_selected && !selected_live_mirror {
                                            "REMOTE · NOT MIRRORED"
                                        } else if selected_live_mirror {
                                            if self.live_mirror_truncated_before {
                                                "MIRRORED LOCALLY · PARTIAL"
                                            } else {
                                                "MIRRORED LOCALLY"
                                            }
                                        } else if historical_mode {
                                            "IMPORTED SNAPSHOT"
                                        } else if connected && self.network_health.is_offline() {
                                            "AUTHENTICATED · API UNREACHABLE"
                                        } else if connected {
                                            "CHATGPT AUTHENTICATED"
                                        } else {
                                            "LOCAL ONLY"
                                        })
                                            .size(11.5)
                                            .strong()
                                            .color(
                                            if remote_catalog_selected && !selected_live_mirror {
                                                egui::Color32::from_rgb(225, 194, 108)
                                            } else if selected_live_mirror {
                                                egui::Color32::from_rgb(126, 210, 156)
                                            } else if historical_mode {
                                                egui::Color32::from_rgb(179, 184, 196)
                                            } else if connected && self.network_health.is_offline() {
                                                egui::Color32::from_rgb(238, 119, 119)
                                            } else if connected {
                                                egui::Color32::from_rgb(126, 210, 156)
                                            } else {
                                                egui::Color32::from_rgb(225, 194, 108)
                                            },
                                        ),
                                    );
                                    });
                                });
                            ui.add_space(8.0);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                // One-click export of the entire selected native conversation,
                                // including journal-verified MCP calls, results and failures.
                                // Uses the full reader projection, not the paged/visible viewport.
                                if !historical_mode
                                    && ui
                                        .add_sized(
                                            [right_column_width, 32.0],
                                            egui::Button::new(
                                                egui::RichText::new("Copy entire chat").size(12.5),
                                            ),
                                        )
                                        .on_hover_text(
                                            "Copy all messages and verified MCP tool activity in this chat as Markdown. The clipboard may contain private data. Not an archive backup.",
                                        )
                                        .clicked()
                                {
                                    ctx.copy_text(native_transcript_export::markdown(
                                        self.local_conversation_id,
                                        &local_conversation_title,
                                        local_display_messages,
                                    ));
                                    self.status = format!(
                                        "copied entire chat ({} messages, including MCP activity)",
                                        local_display_messages.len(),
                                    );
                                }
                            });
                        });
                    });
                });
                if !historical_mode {
                    let conversation_id = self.local_conversation_id.to_string();
                    let last = self.delivery.for_conversation(&conversation_id).last();
                    let color = last.map(|attempt| delivery_status_color(attempt, self.slow_warning_seconds)).unwrap_or_else(|| {
                        if self.remote_connected() {
                            egui::Color32::from_rgb(126, 210, 156)
                        } else {
                            egui::Color32::from_rgb(225, 194, 108)
                        }
                    });
                    let label = last.map(|a| {
                        if a.phase == delivery_telemetry::Phase::Complete {
                            format!("● Ready for your message · previous reply {:.1}s", a.seconds())
                        } else if a.slow(Duration::from_secs(u64::from(self.slow_warning_seconds))) {
                            format!("● SLOW · {} · {:.1}s", a.label(), a.seconds())
                        } else {
                            format!("● {} · {:.1}s", a.label(), a.seconds())
                        }
                    }).unwrap_or_else(|| {
                        if self.remote_connected() { "● Ready for your message".to_owned() }
                        else { "● Local-only · connect ChatGPT to send".to_owned() }
                    });
                    ui.label(egui::RichText::new(label).size(10.0).color(color));
                    let network = self.network_health.display();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(network.label).size(10.0).color(network.color))
                            .on_hover_text(network.tooltip);
                        if ui.small_button("Check now").on_hover_text("Run another HTTPS connectivity check without sending a model request").clicked() {
                            self.network_health.refresh();
                            ctx.request_repaint();
                        }
                    });
                }
            });
    }
}
