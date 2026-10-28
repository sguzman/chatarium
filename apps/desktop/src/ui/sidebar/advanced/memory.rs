//! memory controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_local_memory(&mut self, ui: &mut egui::Ui) {
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
    }
}
