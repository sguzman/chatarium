//! context controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_next_request_context(
        &mut self,
        ui: &mut egui::Ui,
        local_display_messages: &[DisplayMessage],
    ) {
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
    }
}
