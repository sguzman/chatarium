//! Advanced context, memory, tools, workers and inference controls.
//! Moving this scroll view does not change its egui IDs or action ordering.
use super::*;

mod admissions;
mod context;
mod controller;
mod memory;
mod routed_inbox;
mod routing;
mod tools;
mod worker_lifecycle;
mod workers;

impl ChatariumApp {
    pub(super) fn render_advanced_controls(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        local_display_messages: &[DisplayMessage],
    ) {
        let mut inference_controls_changed = false;
        let mut behavior_profile_changed = false;
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

                        self.render_local_routing(ui);

                        self.render_routed_inbox(ui);

                        self.render_local_memory(ui);

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        self.render_mcp_tools(ui, ctx);

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        self.render_worker_lifecycle(ui);

                        self.render_worker_inbox(ui);

                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        self.render_controller_supervision(ui);

                        ui.add_space(8.0);
                        self.render_admitted_mcp_evidence(ui);
                        ui.add_space(8.0);
                        ui.collapsing("Recent outgoing context snapshots · journaled", |ui| {
                            mcp_dispatch_manifest::render_dispatches(
                                ui,
                                &self.events,
                                self.local_conversation_id,
                            );
                        });
                        ui.add_space(8.0);
                        self.render_next_request_context(ui, local_display_messages);
                    });
                    });
        if inference_controls_changed {
            self.persist_current_inference_settings();
        }
        if behavior_profile_changed {
            self.persist_current_behavior_profile();
        }
    }
}
