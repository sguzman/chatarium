//! routing controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_local_routing(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Local routing directory", |ui| {
            match replay_local_routing_directory(&self.events) {
                Err(error) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "routing directory projection blocked: {error}"
                        ))
                        .size(9.0)
                        .color(egui::Color32::from_rgb(186, 108, 108)),
                    );
                }
                Ok(entries) => {
                    let current_entry = entries
                        .iter()
                        .find(|entry| {
                            entry.conversation_id == self.local_conversation_id
                        })
                        .copied();
                    ui.label(
                        egui::RichText::new(format!(
                            "{} addressable local conversation{} · route intent/policy is durable; dispatch remains disabled",
                            entries.len(),
                            if entries.len() == 1 { "" } else { "s" },
                        ))
                        .size(9.0)
                        .color(egui::Color32::from_rgb(139, 143, 153)),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("local-routing-directory")
                        .max_height(140.0)
                        .show(ui, |ui| {
                            for entry in &entries {
                                let title = local_conversation_display_title(
                                    &self.local_conversation_catalog,
                                    entry.conversation_id,
                                    &self.events,
                                );
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(if entry.conversation_id
                                            == self.local_conversation_id
                                        {
                                            "CURRENT"
                                        } else {
                                            "LOCAL"
                                        })
                                        .monospace()
                                        .size(9.0),
                                    );
                                    ui.label(
                                        egui::RichText::new(title).size(10.0),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "endpoint {} · session {} · {}",
                                            entry.endpoint_id.get(),
                                            entry.current_session_id.get(),
                                            session_lifecycle_phase_label(
                                                entry.current_session_phase,
                                            ),
                                        ))
                                        .monospace()
                                        .size(9.0)
                                        .color(egui::Color32::from_rgb(
                                            139, 143, 153,
                                        )),
                                    );

                                    if entry.conversation_id
                                        != self.local_conversation_id
                                    {
                                        let route_fresh = current_entry.is_some_and(
                                            |source| {
                                                source
                                                    .current_session_phase
                                                    .accepts_ordinary_turns()
                                                    && entry
                                                        .current_session_phase
                                                        .accepts_ordinary_turns()
                                            },
                                        );
                                        if ui
                                            .add_enabled(
                                                route_fresh
                                                    && !self
                                                        .route_policy_command_pending
                                                    && self.persist_tx.is_some(),
                                                egui::Button::new(
                                                    "Propose route",
                                                ),
                                            )
                                            .clicked()
                                        {
                                            self.propose_local_session_route(
                                                entry.conversation_id,
                                            );
                                        }
                                    }
                                });
                            }
                        });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Manual local route policy")
                            .strong()
                            .size(10.0),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Session-message routes carry an immutable payload. Allow requires a payload. Dispatch consumes one-shot authority and records local delivery provenance.",
                        )
                        .size(9.0)
                        .color(egui::Color32::from_rgb(139, 143, 153)),
                    );

                    match replay_routing_audit(&self.events) {
                        Err(error) => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "route policy projection blocked: {error}"
                                ))
                                .size(9.0)
                                .color(egui::Color32::from_rgb(186, 108, 108)),
                            );
                        }
                        Ok(routes) => {
                            let routes = routes
                                .into_iter()
                                .filter(|route| {
                                    route.request.class
                                        == RouteClass::SessionMessage
                                })
                                .collect::<Vec<_>>();
                            let payloads =
                                match replay_local_route_payload_audit(&self.events) {
                                    Ok(payloads) => Some(payloads),
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "route payload projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(
                                                186, 108, 108,
                                            )),
                                        );
                                        None
                                    }
                                };
                            let deliveries =
                                match replay_local_route_delivery_audit(&self.events)
                                {
                                    Ok(deliveries) => Some(deliveries),
                                    Err(error) => {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "route delivery projection blocked: {error}"
                                            ))
                                            .size(9.0)
                                            .color(egui::Color32::from_rgb(
                                                186, 108, 108,
                                            )),
                                        );
                                        None
                                    }
                                };
                            if routes.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "no local session-message routes proposed",
                                    )
                                    .size(9.0)
                                    .color(egui::Color32::from_rgb(
                                        139, 143, 153,
                                    )),
                                );
                            } else {
                                egui::ScrollArea::vertical()
                                    .id_salt("local-route-policy")
                                    .max_height(180.0)
                                    .show(ui, |ui| {
                                        for route in routes {
                                            let source = entries.iter().find(
                                                |entry| {
                                                    entry.endpoint_id
                                                        == route.request.source
                                                },
                                            );
                                            let destination = entries.iter().find(
                                                |entry| {
                                                    entry.endpoint_id
                                                        == route
                                                            .request
                                                            .destination
                                                },
                                            );
                                            let fresh = source.is_some_and(
                                                |entry| {
                                                    entry
                                                        .current_session_phase
                                                        .accepts_ordinary_turns()
                                                },
                                            ) && destination.is_some_and(
                                                |entry| {
                                                    entry
                                                        .current_session_phase
                                                        .accepts_ordinary_turns()
                                                },
                                            );
                                            let source_label = source
                                                .map(|entry| {
                                                    local_conversation_display_title(
                                                        &self
                                                            .local_conversation_catalog,
                                                        entry.conversation_id,
                                                        &self.events,
                                                    )
                                                })
                                                .unwrap_or_else(|| {
                                                    format!(
                                                        "endpoint {}",
                                                        route
                                                            .request
                                                            .source
                                                            .get()
                                                    )
                                                });
                                            let destination_label = destination
                                                .map(|entry| {
                                                    local_conversation_display_title(
                                                        &self
                                                            .local_conversation_catalog,
                                                        entry.conversation_id,
                                                        &self.events,
                                                    )
                                                })
                                                .unwrap_or_else(|| {
                                                    format!(
                                                        "endpoint {}",
                                                        route
                                                            .request
                                                            .destination
                                                            .get()
                                                    )
                                                });
                                            let payload = payloads
                                                .as_ref()
                                                .and_then(|payloads| {
                                                    payloads.iter().find(
                                                        |payload| {
                                                            payload.route_id
                                                                == route.request.id
                                                        },
                                                    )
                                                });
                                            let delivery = deliveries
                                                .as_ref()
                                                .and_then(|deliveries| {
                                                    deliveries.iter().find(
                                                        |delivery| {
                                                            delivery.route_id
                                                                == route.request.id
                                                        },
                                                    )
                                                });

                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "route {} · {}",
                                                        route.request.id.get(),
                                                        route_gate_state_label(
                                                            route.gate_state,
                                                        ),
                                                    ))
                                                    .monospace()
                                                    .size(9.0),
                                                );
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{source_label} → {destination_label}"
                                                    ))
                                                    .size(10.0),
                                                );
                                                if !fresh {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "STALE",
                                                        )
                                                        .monospace()
                                                        .size(9.0)
                                                        .color(
                                                            egui::Color32::from_rgb(
                                                                186, 108, 108,
                                                            ),
                                                        ),
                                                    );
                                                }

                                                let can_decide = fresh
                                                    && !route
                                                        .gate_state
                                                        .is_dispatched()
                                                    && !self
                                                        .route_policy_command_pending
                                                    && !self
                                                        .route_payload_command_pending
                                                    && self.persist_tx.is_some();
                                                if ui
                                                    .add_enabled(
                                                        can_decide
                                                            && payload.is_some()
                                                            && route
                                                                .latest_user_decision
                                                                != Some(
                                                                    RouteUserDecision::Allow,
                                                                ),
                                                        egui::Button::new(
                                                            "Allow",
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    self.decide_local_session_route(
                                                        route.request.id,
                                                        RouteUserDecision::Allow,
                                                    );
                                                }
                                                if ui
                                                    .add_enabled(
                                                        can_decide
                                                            && route
                                                                .latest_user_decision
                                                                != Some(
                                                                    RouteUserDecision::Deny,
                                                                ),
                                                        egui::Button::new(
                                                            "Deny",
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    self.decide_local_session_route(
                                                        route.request.id,
                                                        RouteUserDecision::Deny,
                                                    );
                                                }

                                                let can_dispatch = fresh
                                                    && payload.is_some()
                                                    && delivery.is_none()
                                                    && matches!(
                                                        route.gate_state,
                                                        RouteGateState::Allowed { .. }
                                                            | RouteGateState::Dispatched { .. }
                                                    )
                                                    && !self
                                                        .route_dispatch_command_pending
                                                    && !self
                                                        .route_policy_command_pending
                                                    && !self
                                                        .route_payload_command_pending
                                                    && self.persist_tx.is_some();
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
                                                    self.dispatch_local_session_route(
                                                        route.request.id,
                                                    );
                                                }
                                                if let Some(delivery) = delivery {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            format!(
                                                                "DELIVERED · event #{}",
                                                                delivery
                                                                    .delivered_sequence,
                                                            ),
                                                        )
                                                        .monospace()
                                                        .size(9.0),
                                                    );
                                                }
                                            });

                                            if let Some(payload) = payload {
                                                ui.collapsing(
                                                    format!(
                                                        "Payload {} · {} bytes · immutable",
                                                        payload.payload_id.get(),
                                                        payload.text.len(),
                                                    ),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                payload.text.as_str(),
                                                            )
                                                            .size(10.0),
                                                        );
                                                    },
                                                );
                                            } else {
                                                let mut attach_clicked = false;
                                                ui.horizontal_wrapped(|ui| {
                                                    let draft = self
                                                        .route_payload_drafts
                                                        .entry(route.request.id)
                                                        .or_default();
                                                    ui.add(
                                                        egui::TextEdit::singleline(
                                                            draft,
                                                        )
                                                        .desired_width(260.0)
                                                        .hint_text(
                                                            "Exact routed message text",
                                                        ),
                                                    );
                                                    let can_attach = fresh
                                                        && !draft.trim().is_empty()
                                                        && !route
                                                            .gate_state
                                                            .is_dispatched()
                                                        && !self
                                                            .route_payload_command_pending
                                                        && !self
                                                            .route_policy_command_pending
                                                        && self.persist_tx.is_some();
                                                    if ui
                                                        .add_enabled(
                                                            can_attach,
                                                            egui::Button::new(
                                                                "Attach payload",
                                                            ),
                                                        )
                                                        .clicked()
                                                    {
                                                        attach_clicked = true;
                                                    }
                                                });
                                                if attach_clicked {
                                                    self.attach_local_session_route_payload(
                                                        route.request.id,
                                                    );
                                                }
                                            }
                                        }
                                    });
                            }
                            if self.route_policy_command_pending
                                || self.route_payload_command_pending
                                || self.route_dispatch_command_pending
                            {
                                ui.spinner();
                            }
                        }
                    }
                }
            }
        });
    }
}
