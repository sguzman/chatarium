//! Advanced context, memory, tools, workers and inference controls.
//! Moving this scroll view does not change its egui IDs or action ordering.
use super::*;

mod admissions;
mod context;
mod memory;
mod routed_inbox;
mod routing;
mod tools;
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

                        self.render_worker_inbox(ui);

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
