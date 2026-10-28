//! workers controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_worker_inbox(&mut self, ui: &mut egui::Ui) {
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
    }
}
