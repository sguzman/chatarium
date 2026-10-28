//! Human-owned coordination inbox, topology and message-center controls.
use super::*;
fn conversation_color(conversations: &[(String, String, Color32)], id: &str) -> Color32 {
    conversations
        .iter()
        .find(|(key, _, _)| key == id)
        .map(|(_, _, color)| *color)
        .unwrap_or(Color32::from_rgb(125, 160, 190))
}

/// Inspect the active chat's outstanding inbox without model execution or hidden reads.
/// The human controls deletion; deletion cannot erase an already-consumed transcript item.
pub fn render_chat_inbox(
    ui: &mut egui::Ui,
    store: &mut Workspace,
    drafts: &mut UiDrafts,
    conversation: &str,
) -> bool {
    let Some((cid, worker)) = store.member_for(conversation) else {
        ui.label("This chat is not attached to a coordination worker.");
        return false;
    };
    let Some(c) = store.containers.iter().find(|c| c.id == cid) else {
        return false;
    };
    let pending = c
        .messages
        .iter()
        .filter(|m| {
            m.to == worker && matches!(m.status.as_str(), "delivered" | "queued" | "approval")
        })
        .rev()
        .map(|message| {
            let sender = c
                .members
                .iter()
                .find(|member| member.id == message.from)
                .map(|member| member.name.clone())
                .unwrap_or_else(|| format!("Worker #{}", message.from));
            (message.clone(), sender)
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        ui.label("No pending messages for this chat. To see sent, consumed, or acknowledged messages across all chats, open Coordination / messages.");
        return false;
    }
    let mut dirty = false;
    egui::ScrollArea::vertical()
        .id_salt(("coordination-chat-inbox-scroll", conversation))
        .max_height(310.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (message, sender_name) in pending {
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(format!(
                            "#{} · {} · {}",
                            message.id, message.kind, message.status
                        ));
                        ui.label(format!("From {sender_name} (#{})", message.from));
                    });
                    ui.add(egui::Label::new(&message.body).wrap());
                    ui.label(egui::RichText::new(&message.note).weak().size(10.0));
                    if drafts.confirm_delete == Some(message.id) {
                        ui.horizontal(|ui| {
                            ui.label("Erase message contents and remove it from the inbox?");
                            if ui.button("Confirm delete").clicked() {
                                match store.delete_message(cid, message.id) {
                                    Ok(()) => {
                                        drafts.notice =
                                            format!("Inbox message #{} deleted", message.id);
                                        drafts.confirm_delete = None;
                                        dirty = true;
                                    }
                                    Err(error) => drafts.notice = error,
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                drafts.confirm_delete = None;
                            }
                        });
                    } else if ui.small_button("Delete…").clicked() {
                        drafts.confirm_delete = Some(message.id);
                    }
                });
            }
        });
    dirty
}

pub fn topology_ui(ui: &mut egui::Ui, c: &Container, conversations: &[(String, String, Color32)]) {
    if c.members.is_empty() {
        ui.label("No workers attached yet. Choose a conversation above.");
        return;
    }
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(220.0), 230.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let radius = rect.width().min(rect.height()).mul_add(0.35, 0.0);
    let middle = rect.center();
    let n = c.members.len();
    let locations = c
        .members
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let angle =
                std::f32::consts::TAU * (i as f32) / n.max(1) as f32 - std::f32::consts::FRAC_PI_2;
            (
                w.id,
                Pos2::new(
                    middle.x + radius * angle.cos(),
                    middle.y + radius * angle.sin(),
                ),
            )
        })
        .collect::<Vec<_>>();
    for l in &c.links {
        let source = locations
            .iter()
            .find(|(id, _)| *id == l.from)
            .map(|(_, p)| *p);
        let target = locations
            .iter()
            .find(|(id, _)| *id == l.to)
            .map(|(_, p)| *p);
        if let (Some(a), Some(b)) = (source, target) {
            let color = match l.mode.as_str() {
                "block" => Color32::from_rgb(185, 82, 82),
                "queue" | "approve" => Color32::from_rgb(206, 172, 94),
                "rate_limit" => Color32::from_rgb(167, 137, 218),
                _ => Color32::from_rgb(69, 174, 164),
            };
            painter.line_segment([a, b], Stroke::new(2.0, color));
            let d = b - a;
            if d.length_sq() > 0.1 {
                let end = b - d.normalized() * 23.0;
                let wing = Vec2::new(-d.y, d.x).normalized() * 5.0;
                painter.line_segment(
                    [end - d.normalized() * 9.0 + wing, end],
                    Stroke::new(2.0, color),
                );
                painter.line_segment(
                    [end - d.normalized() * 9.0 - wing, end],
                    Stroke::new(2.0, color),
                );
            }
        }
    }
    for w in &c.members {
        if let Some((_, p)) = locations.iter().find(|(id, _)| *id == w.id) {
            let color = conversation_color(conversations, &w.conversation);
            let brightness = (u32::from(color.r()) * 299
                + u32::from(color.g()) * 587
                + u32::from(color.b()) * 114)
                / 1000;
            let text_color = if brightness > 150 {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            painter.circle_filled(*p, 21.0, color);
            painter.circle_stroke(*p, 21.0, Stroke::new(2.0, Color32::WHITE));
            painter.text(
                *p,
                Align2::CENTER_CENTER,
                w.id.to_string(),
                FontId::proportional(13.0),
                text_color,
            );
            let chat_title = conversations
                .iter()
                .find(|(id, _, _)| *id == w.conversation)
                .map(|(_, title, _)| title.as_str())
                .unwrap_or("missing chat");
            painter.text(
                Pos2::new(p.x, p.y + 31.0),
                Align2::CENTER_CENTER,
                format!(
                    "{} · {}",
                    w.name,
                    chat_title.chars().take(28).collect::<String>()
                ),
                FontId::proportional(11.0),
                color,
            );
        }
    }
}
/// The single cross-chat ledger: an inbox only shows pending receipts, but this
/// viewer shows every durable message in every container, including sent and
/// consumed records. It never performs implicit reads or model activation.
pub(super) fn message_visible(
    message: &Message,
    container_name: &str,
    sender: &str,
    recipient: &str,
    query: &str,
    filter: &str,
) -> bool {
    let status_matches = match filter {
        "pending" => matches!(message.status.as_str(), "delivered" | "queued" | "approval"),
        "consumed" => message.status == "consumed",
        "other" => !matches!(
            message.status.as_str(),
            "delivered" | "queued" | "approval" | "consumed"
        ),
        _ => true,
    };
    if !status_matches {
        return false;
    }
    let needle = query.trim().to_lowercase();
    needle.is_empty()
        || [
            container_name.to_lowercase(),
            sender.to_lowercase(),
            recipient.to_lowercase(),
            message.id.to_string(),
            format!("#{}", message.id),
            message.status.to_lowercase(),
            message.kind.to_lowercase(),
            message.body.to_lowercase(),
        ]
        .iter()
        .any(|haystack| haystack.contains(&needle))
}

fn render_message_center(
    ui: &mut egui::Ui,
    store: &mut Workspace,
    drafts: &mut UiDrafts,
    conversations: &[(String, String, Color32)],
) -> bool {
    let total = store
        .containers
        .iter()
        .map(|c| c.messages.len())
        .sum::<usize>();
    let pending = store
        .containers
        .iter()
        .flat_map(|c| c.messages.iter())
        .filter(|m| matches!(m.status.as_str(), "delivered" | "queued" | "approval"))
        .count();
    let consumed = store
        .containers
        .iter()
        .flat_map(|c| c.messages.iter())
        .filter(|m| m.status == "consumed")
        .count();
    let mut dirty = false;
    egui::CollapsingHeader::new(format!(
        "All-message explorer · {total} total · {pending} pending · {consumed} consumed"
    ))
    .id_salt("chatarium-all-coordination-messages")
    .default_open(true)
    .show(ui, |ui| {
        ui.label("Every coordination message, across ALL chats and containers. An empty chat inbox means no PENDING receipts, not missing history.");
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut drafts.message_query)
                    .hint_text("Search IDs, workers, message kinds, or content")
                    .desired_width(280.0),
            );
            egui::ComboBox::from_id_salt("coordination-message-status-filter")
                .selected_text(match drafts.message_filter.as_str() {
                    "pending" => "Pending inboxes",
                    "consumed" => "Consumed",
                    "other" => "Blocked / deleted / other",
                    _ => "All statuses",
                })
                .show_ui(ui, |ui| {
                    for (value, title) in [
                        ("all", "All statuses"),
                        ("pending", "Pending inboxes"),
                        ("consumed", "Consumed"),
                        ("other", "Blocked / deleted / other"),
                    ] {
                        ui.selectable_value(&mut drafts.message_filter, value.to_owned(), title);
                    }
                });
            if ui.small_button("Clear filters").clicked() {
                drafts.message_filter = "all".to_owned();
                drafts.message_query.clear();
            }
        });

        // Capture read-only search strings rather than a mutable draft in the
        // nested iterator. The row action buttons need mutable draft access.
        let query = drafts.message_query.as_str();
        let filter = drafts.message_filter.as_str();
        // Clone only lightweight records so queue controls can mutate the
        // workspace without borrowing the selected container.
        let mut entries = store
            .containers
            .iter()
            .flat_map(|container| {
                container.messages.iter().filter_map(move |message| {
                    let source = container.members.iter().find(|m| m.id == message.from);
                    let target = container.members.iter().find(|m| m.id == message.to);
                    let source_name = source
                        .map(|m| m.name.as_str())
                        .unwrap_or("Former worker");
                    let target_name = target
                        .map(|m| m.name.as_str())
                        .unwrap_or("Former worker");
                    message_visible(
                        message,
                        &container.name,
                        source_name,
                        target_name,
                        query,
                        filter,
                    )
                    .then(|| {
                        (
                            container.id,
                            container.name.clone(),
                            source.cloned(),
                            target.cloned(),
                            message.clone(),
                        )
                    })
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(container, _, _, _, message)| {
            (std::cmp::Reverse(message.created_ms), std::cmp::Reverse(*container), std::cmp::Reverse(message.id))
        });
        let matching = entries.len();
        if matching == 0 {
            ui.label("No messages match those filters. Try All statuses or clear the search.");
            return;
        }
        ui.label(format!("Showing newest {} of {matching} matching messages", matching.min(100)));
        egui::ScrollArea::vertical()
            .id_salt("coordination-all-message-scroll")
            .max_height(385.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (container_id, container_name, source, target, message) in
                    entries.into_iter().take(100)
                {
                    ui.push_id((container_id, message.id), |ui| {
                        let source_name = source.as_ref().map_or("Former worker", |m| m.name.as_str());
                        let target_name = target.as_ref().map_or("Former worker", |m| m.name.as_str());
                        let status = match message.status.as_str() {
                            "delivered" => "DELIVERED TO INBOX",
                            "consumed" => "CONSUMED BY MODEL REQUEST",
                            "queued" => "QUEUED",
                            "approval" => "AWAITING APPROVAL",
                            "blocked" => "BLOCKED",
                            "deleted" => "DELETED",
                            "rejected" => "REJECTED",
                            other => other,
                        };
                        ui.group(|ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(format!("#{} · {} · {status}", message.id, message.kind));
                                ui.weak(format!("in {container_name}"));
                            });
                            ui.horizontal_wrapped(|ui| {
                                let source_color = source.as_ref()
                                    .map(|m| conversation_color(conversations, &m.conversation))
                                    .unwrap_or(Color32::GRAY);
                                let target_color = target.as_ref()
                                    .map(|m| conversation_color(conversations, &m.conversation))
                                    .unwrap_or(Color32::GRAY);
                                ui.colored_label(source_color, "●");
                                ui.strong(format!("{source_name} (#{} )", message.from));
                                ui.label("→");
                                ui.colored_label(target_color, "●");
                                ui.strong(format!("{target_name} (#{} )", message.to));
                            });
                            if message.status != "deleted" {
                                let summary = message.body.chars().take(180).collect::<String>();
                                ui.add(egui::Label::new(summary).wrap());
                            }
                            egui::CollapsingHeader::new("Details / navigate / manage")
                                .id_salt("message-details")
                                .default_open(false)
                                .show(ui, |ui| {
                                    if message.status != "deleted" {
                                        ui.add(egui::Label::new(&message.body).wrap());
                                        if ui.small_button("Copy message").clicked() {
                                            ui.ctx().copy_text(message.body.clone());
                                        }
                                    }
                                    ui.add(egui::Label::new(format!(
                                        "Message #{} · created {} · status {} · {}",
                                        message.id, message.created_ms, message.status, message.note,
                                    )).wrap());
                                    ui.horizontal_wrapped(|ui| {
                                        if let Some(source) = &source {
                                            if ui.small_button(format!("Open sender: {}", source.name)).clicked() {
                                                drafts.navigate_to_conversation = Some(source.conversation.clone());
                                            }
                                        }
                                        if let Some(target) = &target {
                                            if ui.small_button(format!("Open recipient: {}", target.name)).clicked() {
                                                drafts.navigate_to_conversation = Some(target.conversation.clone());
                                            }
                                        }
                                        if matches!(message.status.as_str(), "queued" | "approval") {
                                            if ui.small_button("Release").clicked() {
                                                match store.decide(container_id, message.id, true, now_ms()) {
                                                    Ok(()) => {
                                                        dirty = true;
                                                        drafts.notice = format!("Message #{} released", message.id);
                                                    }
                                                    Err(error) => drafts.notice = error,
                                                }
                                            }
                                            if ui.small_button("Reject").clicked() {
                                                match store.decide(container_id, message.id, false, now_ms()) {
                                                    Ok(()) => {
                                                        dirty = true;
                                                        drafts.notice = format!("Message #{} rejected", message.id);
                                                    }
                                                    Err(error) => drafts.notice = error,
                                                }
                                            }
                                        }
                                    });
                                });
                        });
                    });
                }
            });
    });
    dirty
}

pub fn render(
    ui: &mut egui::Ui,
    store: &mut Workspace,
    drafts: &mut UiDrafts,
    conversations: &[(String, String, Color32)],
    current: &str,
) -> bool {
    let mut dirty = false;
    ui.heading("Coordination · messages");
    dirty |= render_message_center(ui, store, drafts, conversations);
    ui.separator();
    ui.heading("Containers and routing");
    ui.label("Create a container first, attach existing chats as workers, then set authority and delivery rules.");
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut drafts.new_container)
                .hint_text("Container name")
                .desired_width(165.0),
        );
        if ui.button("Create container").clicked() {
            match store.create_container(&drafts.new_container) {
                Ok(id) => {
                    drafts.selected_container = id;
                    drafts.new_container.clear();
                    dirty = true;
                    drafts.notice = "container created".to_owned();
                }
                Err(e) => drafts.notice = e,
            }
        }
    });
    egui::ComboBox::from_label("Active container")
        .selected_text(
            store
                .containers
                .iter()
                .find(|c| c.id == drafts.selected_container)
                .map(|c| c.name.as_str())
                .unwrap_or("Select container"),
        )
        .show_ui(ui, |ui| {
            for c in &store.containers {
                ui.selectable_value(
                    &mut drafts.selected_container,
                    c.id,
                    format!("{} (#{})", c.name, c.id),
                );
            }
        });
    let Some(container) = store
        .containers
        .iter()
        .find(|c| c.id == drafts.selected_container)
        .cloned()
    else {
        if !drafts.notice.is_empty() {
            ui.label(&drafts.notice);
        }
        return dirty;
    };
    if !drafts.notice.is_empty() {
        ui.label(&drafts.notice);
    }
    ui.separator();
    let member_label = |id: u64| {
        container
            .members
            .iter()
            .find(|w| w.id == id)
            .map(|w| {
                let chat = conversations
                    .iter()
                    .find(|(id, _, _)| *id == w.conversation)
                    .map(|(_, title, _)| title.chars().take(32).collect::<String>())
                    .unwrap_or_else(|| "missing chat".to_owned());
                format!("{} · {} (#{})", w.name, chat, w.id)
            })
            .unwrap_or("Select worker".to_owned())
    };
    let member_accent = |id: u64| {
        container
            .members
            .iter()
            .find(|w| w.id == id)
            .map(|w| conversation_color(conversations, &w.conversation))
            .unwrap_or(Color32::GRAY)
    };

    egui::CollapsingHeader::new(format!("Workers ({})", container.members.len()))
        .id_salt("coordination-workers")
        .default_open(false)
        .show(ui, |ui| {
            ui.strong("Add existing conversation as worker");
            egui::ComboBox::from_label("Conversation")
                .selected_text(
                    conversations
                        .iter()
                        .find(|(id, _, _)| *id == drafts.selected_conversation)
                        .map(|(_, name, color)| {
                            egui::RichText::new(format!("● {name}")).color(*color)
                        })
                        .unwrap_or_else(|| egui::RichText::new("Choose chat")),
                )
                .show_ui(ui, |ui| {
                    for (id, title, color) in conversations {
                        let bound = store.member_for(id).is_some();
                        ui.add_enabled_ui(!bound, |ui| {
                            ui.selectable_value(
                                &mut drafts.selected_conversation,
                                id.clone(),
                                egui::RichText::new(format!("● {title}")).color(*color),
                            );
                        });
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut drafts.worker_name).hint_text("Worker name"));
            ui.add(egui::TextEdit::singleline(&mut drafts.worker_role).hint_text("Role"));
            if ui.button("Attach as worker").clicked() {
                match store.attach(
                    container.id,
                    &drafts.selected_conversation,
                    &drafts.worker_name,
                    &drafts.worker_role,
                ) {
                    Ok(id) => {
                        drafts.notice = format!("worker #{id} attached");
                        drafts.worker_name.clear();
                        dirty = true;
                    }
                    Err(e) => drafts.notice = e,
                }
            }
            for m in &container.members {
                ui.horizontal(|ui| {
                    ui.colored_label(member_accent(m.id), "●");
                    ui.label(format!(
                        "{} — {}{}",
                        member_label(m.id),
                        m.role,
                        if m.conversation == current {
                            " (ACTIVE CHAT)"
                        } else {
                            ""
                        }
                    ));
                });
            }
        });
    ui.separator();
    let (inbox_count, queue_count) = store.inbox_counts(current);
    egui::CollapsingHeader::new(format!(
        "Active chat inbox ({} delivered, {} queued)",
        inbox_count, queue_count
    ))
    .id_salt("coordination-active-inbox")
    .default_open(false)
    .show(ui, |ui| {
        dirty |= render_chat_inbox(ui, store, drafts, current);
    });
    egui::CollapsingHeader::new("Topology viewer")
        .id_salt("coordination-topology-view")
        .default_open(false)
        .show(ui, |ui| {
            topology_ui(ui, &container, conversations);
        });
    egui::CollapsingHeader::new(format!(
        "Authority and delivery rules ({} edges)",
        container.links.len()
    ))
    .id_salt("coordination-authority")
    .default_open(false)
    .show(ui, |ui| {
        ui.label("Configure a directed edge. This does not change the active chat.");
        egui::ComboBox::from_label("Authority sender")
            .selected_text(member_label(drafts.source))
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.source,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        egui::ComboBox::from_label("Authority recipient")
            .selected_text(member_label(drafts.target))
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.target,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        ui.checkbox(&mut drafts.directives, "Can issue directives");
        ui.checkbox(&mut drafts.reports, "Can send reports / requests / results");
        egui::ComboBox::from_label("Delivery")
            .selected_text(&drafts.mode)
            .show_ui(ui, |ui| {
                for mode in ["allow", "block", "queue", "approve", "rate_limit"] {
                    ui.selectable_value(&mut drafts.mode, mode.to_owned(), mode);
                }
            });
        if drafts.mode == "rate_limit" {
            ui.add(egui::Slider::new(&mut drafts.per_minute, 0..=600).text("Messages / minute"));
        }
        if ui.button("Save directed edge").clicked() {
            match store.set_link(
                container.id,
                drafts.source,
                drafts.target,
                drafts.directives,
                drafts.reports,
                &drafts.mode,
                drafts.per_minute,
            ) {
                Ok(()) => {
                    dirty = true;
                    drafts.send_from = drafts.source;
                    drafts.send_to = drafts.target;
                    drafts.notice = format!(
                        "authority saved: {} → {}",
                        member_label(drafts.source),
                        member_label(drafts.target)
                    );
                }
                Err(e) => drafts.notice = e,
            }
        }
        for l in &container.links {
            ui.label(format!(
                "{} → {} · {} · directives={} reports={} · {}/min",
                member_label(l.from),
                member_label(l.to),
                l.mode,
                l.directives,
                l.reports,
                l.per_minute
            ));
            if l.directives
                && l.mode != "block"
                && !container
                    .links
                    .iter()
                    .any(|reverse| reverse.from == l.to && reverse.to == l.from)
            {
                ui.weak(format!(
                    "  Automatic reply: {} → {} · reports / requests / results / ack, no directives",
                    member_label(l.to),
                    member_label(l.from)
                ));
            }
        }
    });
    egui::CollapsingHeader::new("Send a test message")
        .id_salt("coordination-test-message")
        .default_open(true)
        .show(ui, |ui| {
    ui.label("Human-operated test: choose which worker speaks. Agent messages still use verified conversation identity.");
    if let Some(active) = container.members.iter().find(|m| m.conversation == current) {
        ui.colored_label(
            member_accent(active.id),
            format!("● Active conversation: {}", member_label(active.id)),
        );
    } else {
        ui.label("The currently open conversation is not attached to this container.");
    }
    ui.horizontal(|ui| {
        ui.label("From");
        egui::ComboBox::from_id_salt("coordination-test-sender")
            .selected_text(
                egui::RichText::new(format!("● {}", member_label(drafts.send_from)))
                    .color(member_accent(drafts.send_from)),
            )
            .show_ui(ui, |ui| {
                for m in &container.members {
                    ui.selectable_value(
                        &mut drafts.send_from,
                        m.id,
                        egui::RichText::new(format!("● {}", member_label(m.id)))
                            .color(member_accent(m.id)),
                    );
                }
            });
        ui.label("→ To");
        egui::ComboBox::from_id_salt("coordination-test-recipient")
            .selected_text(
                egui::RichText::new(format!("● {}", member_label(drafts.send_to)))
                    .color(member_accent(drafts.send_to)),
            )
            .show_ui(ui, |ui| {
                for m in &container.members {
                    if m.id != drafts.send_from {
                        ui.selectable_value(
                            &mut drafts.send_to,
                            m.id,
                            egui::RichText::new(format!("● {}", member_label(m.id)))
                                .color(member_accent(m.id)),
                        );
                    }
                }
            });
    });
    egui::ComboBox::from_label("Message kind")
        .selected_text(&drafts.kind)
        .show_ui(ui, |ui| {
            for kind in ["directive", "report", "request", "result", "ack"] {
                ui.selectable_value(&mut drafts.kind, kind.to_owned(), kind);
            }
        });
    let send_source = container.members.iter().find(|m| m.id == drafts.send_from);
    let send_target = container.members.iter().find(|m| m.id == drafts.send_to);
    let route = Workspace::route_for(&container, drafts.send_from, drafts.send_to);
    let valid_pair =
        send_source.is_some() && send_target.is_some() && drafts.send_from != drafts.send_to;
    if !valid_pair {
        ui.colored_label(
            Color32::from_rgb(240, 172, 108),
            "Select two different workers to send a message.",
        );
    } else if let Some((link, automatic_reply)) = route {
        if automatic_reply {
            ui.weak("Automatic report-only return route; reverse directives remain forbidden.");
        }
        let authorized = if drafts.kind == "directive" {
            link.directives
        } else {
            link.reports
        };
        if !authorized || link.mode == "block" {
            ui.colored_label(
                Color32::from_rgb(240, 172, 108),
                "This message will be recorded as BLOCKED by authority or delivery policy.",
            );
        } else if link.mode == "queue" || link.mode == "approve" {
            ui.label(format!(
                "Delivery will wait under the {} policy.",
                link.mode
            ));
        } else {
            ui.colored_label(
                Color32::from_rgb(116, 201, 155),
                "Authorized route configured.",
            );
        }
    } else {
        ui.colored_label(
            Color32::from_rgb(240, 172, 108),
            "No authority edge for this direction. Sending will record a BLOCKED message.",
        );
    }
    ui.add(
        egui::TextEdit::multiline(&mut drafts.body)
            .hint_text("Message for the other worker")
            .desired_rows(2)
            .desired_width(ui.available_width()),
    );
    if ui
        .add_enabled(
            valid_pair && !drafts.body.trim().is_empty(),
            egui::Button::new("Send test message"),
        )
        .clicked()
    {
        if let Some(source) = send_source {
            match store.send(
                &source.conversation,
                drafts.send_to,
                &drafts.kind,
                &drafts.body,
                now_ms(),
            ) {
                Ok((id, status)) => {
                    dirty = true;
                    drafts.notice = format!(
                        "Message #{id}: {status} · {} → {}",
                        member_label(drafts.send_from),
                        member_label(drafts.send_to)
                    );
                    drafts.body.clear();
                }
                Err(error) => drafts.notice = format!("Message not sent: {error}"),
            }
        }
    }
        });
    dirty
}
