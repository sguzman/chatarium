//! worker lifecycle panel extracted from advanced controls.
//! Preserves existing manual authority, eligibility and approval checks.
use super::*;

impl ChatariumApp {
    pub(super) fn render_worker_lifecycle(&mut self, ui: &mut egui::Ui) {
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
    }
}
