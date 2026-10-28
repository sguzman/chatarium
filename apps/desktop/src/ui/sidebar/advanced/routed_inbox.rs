//! routed inbox controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_routed_inbox(&mut self, ui: &mut egui::Ui) {
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
    }
}
