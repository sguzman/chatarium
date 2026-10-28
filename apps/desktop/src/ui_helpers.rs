//! Shared egui drawing, layout and reader scrolling utilities.
use super::*;

/// Count persisted Chatarium data files, without following directory symlinks.
/// Cached for a few seconds so UI frames do not traverse storage repeatedly.
pub(crate) fn local_storage_bytes_cached(
    ctx: &egui::Context,
    journal_path: &std::path::Path,
) -> u64 {
    let id = egui::Id::new("chatarium-local-data-usage");
    if let Some((last, bytes)) = ctx.data_mut(|data| data.get_temp::<(Instant, u64)>(id)) {
        if last.elapsed() < Duration::from_secs(8) {
            return bytes;
        }
    }
    let mut total = 0_u64;
    let mut scanned = 0_usize;
    if let Some(root) = journal_path.parent() {
        let mut folders = vec![(root.to_path_buf(), 0_usize)];
        while let Some((folder, depth)) = folders.pop() {
            if depth > 8 || scanned >= 20_000 {
                break;
            }
            let Ok(entries) = std::fs::read_dir(folder) else {
                continue;
            };
            for entry in entries.flatten() {
                scanned += 1;
                if scanned >= 20_000 {
                    break;
                }
                let Ok(metadata) = entry.path().symlink_metadata() else {
                    continue;
                };
                if metadata.is_file() {
                    total = total.saturating_add(metadata.len());
                } else if metadata.is_dir() {
                    folders.push((entry.path(), depth + 1));
                }
            }
        }
    }
    ctx.data_mut(|data| data.insert_temp(id, (Instant::now(), total)));
    total
}

pub(crate) fn readable_byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub(crate) fn delivery_status_color(
    attempt: &delivery_telemetry::Attempt,
    slow_warning_seconds: u32,
) -> egui::Color32 {
    use delivery_telemetry::Phase;
    if attempt.slow(Duration::from_secs(u64::from(slow_warning_seconds)))
        || attempt.phase == Phase::RetryScheduled
    {
        egui::Color32::from_rgb(242, 167, 79)
    } else {
        match attempt.phase {
            Phase::Complete => egui::Color32::from_rgb(126, 210, 156),
            Phase::Failed | Phase::Unconfirmed => egui::Color32::from_rgb(235, 105, 107),
            _ => egui::Color32::from_rgb(238, 207, 103),
        }
    }
}

pub(crate) fn draw_delivery_timeline(
    ui: &mut egui::Ui,
    timeline: &delivery_telemetry::Timeline,
    conversation_id: &str,
    slow_warning_seconds: u32,
) {
    let attempts = timeline
        .for_conversation(conversation_id)
        .collect::<Vec<_>>();
    if attempts.is_empty() {
        return;
    }
    ui.add_space(8.0);
    ui.separator();

    // Never expand the native chat transcript just because another response
    // finished. The per-conversation fold state is controlled only by the user.
    let summary = if let Some(mean) = timeline.mean_response_seconds(conversation_id) {
        format!(
            "Delivery history · {} requests · avg {mean:.1}s",
            attempts.len()
        )
    } else {
        format!("Delivery history · {} requests", attempts.len())
    };
    egui::CollapsingHeader::new(summary)
        .id_salt(("delivery-history", conversation_id))
        .default_open(false)
        .show(ui, |ui| {
            // Explicit expansion still cannot grow without bound. Preserve the
            // eight most recent exact per-response records inside this viewport.
            egui::ScrollArea::vertical()
                .id_salt(("delivery-history-scroll", conversation_id))
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    draw_delivery_timeline_details(
                        ui,
                        timeline,
                        conversation_id,
                        slow_warning_seconds,
                        &attempts,
                    );
                });
        });
}

pub(crate) fn draw_delivery_timeline_details(
    ui: &mut egui::Ui,
    timeline: &delivery_telemetry::Timeline,
    conversation_id: &str,
    slow_warning_seconds: u32,
    attempts: &[&delivery_telemetry::Attempt],
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("DELIVERY HISTORY · ChatGPT Responses")
                .size(11.0)
                .strong(),
        );
        if let Some(mean) = timeline.mean_response_seconds(conversation_id) {
            ui.label(
                egui::RichText::new(format!("session average {:.1}s", mean))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(151, 154, 163)),
            );
        }
        if let Some(mean) = timeline.mean_first_output_seconds(conversation_id) {
            ui.label(
                egui::RichText::new(format!("average first output {:.1}s", mean))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(151, 154, 163)),
            );
        }
    });
    ui.label(egui::RichText::new(
        "Timings: local queue → HTTP ack → waiting for first output → streaming. HTTP ack is transport evidence, not proof of model activity. Timing history resets on restart; turns remain durable.",
    ).size(9.0).color(egui::Color32::from_rgb(125, 129, 139)));
    // Completed rows are individual requests, not repeated copies of the
    // current green "ready for input" status shown in the header.
    for (index, attempt) in attempts.iter().rev().take(8).rev().enumerate() {
        let response_number = attempts.len().saturating_sub(8) + index + 1;
        let color = delivery_status_color(attempt, slow_warning_seconds);
        let label = if attempt.slow(Duration::from_secs(u64::from(slow_warning_seconds))) {
            "SLOW · waiting for first response".to_owned()
        } else if attempt.phase == delivery_telemetry::Phase::Complete {
            format!("Response #{response_number} · complete")
        } else {
            attempt.label().to_owned()
        };
        egui::Frame::default()
            .fill(egui::Color32::from_rgb(29, 31, 36))
            .corner_radius(egui::CornerRadius::same(7))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!("● {label}"))
                            .strong()
                            .size(10.0)
                            .color(color),
                    );
                    ui.label(
                        egui::RichText::new(format!("elapsed {:.1}s", attempt.seconds()))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(195, 198, 205)),
                    );
                    // Show disjoint phases; cumulative ack/first-output times
                    // obscured whether delay was local, network, or remote.
                    for (name, duration) in [
                        ("queue", attempt.queued_seconds()),
                        ("HTTP ack", attempt.network_ack_seconds()),
                        ("reply startup", attempt.post_ack_wait_seconds()),
                        ("stream", attempt.streaming_seconds()),
                    ] {
                        if let Some(seconds) = duration {
                            ui.label(
                                egui::RichText::new(format!("{name} {seconds:.1}s"))
                                    .size(10.0)
                                    .color(egui::Color32::from_rgb(151, 154, 163)),
                            );
                        }
                    }
                    if attempt.retries > 0 {
                        ui.label(
                            egui::RichText::new(format!("retry {}", attempt.retries))
                                .size(10.0)
                                .color(egui::Color32::from_rgb(242, 167, 79)),
                        );
                    }
                    if let Some(at) = attempt.retry_at {
                        ui.label(
                            egui::RichText::new(format!(
                                "next attempt in {:.1}s",
                                at.saturating_duration_since(Instant::now()).as_secs_f32()
                            ))
                            .size(10.0)
                            .color(color),
                        );
                    }
                });
                if let Some(error) = &attempt.error {
                    ui.label(
                        egui::RichText::new(error)
                            .size(9.0)
                            .color(egui::Color32::from_rgb(170, 173, 183)),
                    );
                }
            });
        ui.add_space(4.0);
    }
}

/// Non-authored, durable coordination evidence rendered as its own distinct
/// bubble. It never masquerades as an authored user message or enters the transcript context.
pub(crate) fn draw_consumed_coordination_bubble(
    ui: &mut egui::Ui,
    message: &coordination_workspace::ConsumedDisplay,
    preferences: &display_preferences::DisplayPreferences,
    ctx: &egui::Context,
) {
    let rgb = preferences.coordination_color(&message.kind);
    let fill = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    // Reserve room for the frame's margins and stroke, even in a narrow reader.
    // A horizontal parent layout with an inner width equal to the whole bubble
    // previously pushed the body off the right edge of the conversation.
    let available = ui.available_width().max(1.0);
    let frame_width = (available * 0.88).min(900.0);
    let inset: i8 = if frame_width < 300.0 { 10 } else { 16 };
    let text_width = (frame_width - f32::from(inset) * 2.0 - 4.0).max(1.0);
    ui.push_id(("consumed-coordination", message.id), |ui| {
        egui::Frame::default()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, fill.gamma_multiply(1.7)))
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(inset, 12))
            .show(ui, |ui| {
                ui.set_width(text_width);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "Coordination {} · #{}",
                            message.kind, message.id
                        ))
                        .strong()
                        .color(egui::Color32::WHITE),
                    );
                    if ui.small_button("Copy").clicked() {
                        ctx.copy_text(message.body.clone());
                    }
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&message.body)
                            .color(egui::Color32::from_rgb(244, 246, 248)),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "From {} · handed to local bridge {} · model processing or reply not implied",
                            message.from,
                            timestamp_display::utc_minute(message.consumed_ms as f64 / 1000.0)
                                .unwrap_or_else(|| "time unknown".to_owned()),
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 222, 226)),
                    )
                    .wrap(),
                );
            });
    });
}

pub(crate) fn draw_reader_bubble(
    ui: &mut egui::Ui,
    message: &DisplayMessage,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) -> egui::Response {
    let preferences = ctx
        .data_mut(|data| {
            data.get_temp::<display_preferences::DisplayPreferences>(egui::Id::new(
                "chatarium-display-preferences",
            ))
        })
        .unwrap_or_default();
    let palette = &preferences.message_colors;
    let bubble_fill = |index: usize| {
        let [r, g, b] = palette[index];
        egui::Color32::from_rgb(r, g, b)
    };
    match message.role {
        DisplayRole::User => {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(0),
                    "You",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Assistant => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(1),
                    "Agent",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Mcp => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(2),
                    "MCP tool",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
        DisplayRole::Activity => {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                transcript_bubble(
                    ui,
                    message,
                    bubble_fill(3),
                    "Transport trace",
                    search_query,
                    active_hit,
                    ctx,
                )
            })
            .inner
        }
    }
}

pub(crate) fn transcript_bubble(
    ui: &mut egui::Ui,
    message: &DisplayMessage,
    fill: egui::Color32,
    label: &str,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) -> egui::Response {
    // The bubble is placed from an outer horizontal layout for left/right message
    // alignment, but its own contents must be vertical. If the inner UI inherits
    // that horizontal layout, the header consumes the row and the body is forced
    // into a tiny residual strip (often one character wide after a resize).
    //
    // Recompute the width from the current parent allocation every frame so a
    // maximized/restored window immediately reflows the transcript.
    // A single width rule shared by all roles; preserve contrasting side
    // alignment, but avoid different card widths based on short message text.
    // Keep a real gutter inside the outline even for narrow windows.
    // Account for the entire frame inset before fixing the inner width.
    let available_width = ui.available_width();
    let content_width = (available_width * 0.80)
        .clamp(200.0, 950.0)
        .min((available_width - 47.0).max(1.0));

    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(
            1.3_f32,
            if label == "You" {
                egui::Color32::from_rgb(66, 139, 228)
            } else if label == "Agent" {
                egui::Color32::from_rgb(65, 166, 108)
            } else if label == "Transport trace" {
                egui::Color32::from_rgb(213, 154, 73)
            } else {
                egui::Color32::from_rgb(155, 109, 215)
            },
        ))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(22, 13))
        .show(ui, |ui| {
            ui.set_width(content_width);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(10.0).strong().color(
                        if label == "You" {
                            egui::Color32::from_rgb(147, 191, 238)
                        } else if label == "Agent" {
                            egui::Color32::from_rgb(163, 221, 178)
                        } else if label == "Transport trace" {
                            egui::Color32::from_rgb(239, 199, 133)
                        } else {
                            egui::Color32::from_rgb(203, 166, 247)
                        },
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Copy").clicked() {
                            ctx.copy_text(offline_reader::copy_payload(&message.text));
                        }
                    });
                });
                ui.add_space(4.0);
                if matches!(message.role, DisplayRole::Mcp | DisplayRole::Activity) {
                    // Keep tool bytes and activity metadata non-executable.
                    // Neither surface may inject author-role markdown.
                    let activity = message.role == DisplayRole::Activity;
                    if activity {
                        // Show useful timing, not a wall of individually journaled deltas.
                        let summary = message.text.lines().nth(1).unwrap_or("No transport events");
                        ui.label(
                            egui::RichText::new(summary.trim())
                                .size(11.0)
                                .color(egui::Color32::from_rgb(231, 202, 159)),
                        );
                        ui.label(
                            egui::RichText::new("Bridge timing only · not model thinking")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(178, 161, 139)),
                        );
                    }
                    egui::CollapsingHeader::new(if activity {
                        "Raw transport events · expand to inspect"
                    } else {
                        "Exact tool activity · expand to inspect"
                    })
                    .id_salt(("non-authored-activity", message.sequence))
                    .default_open(!activity && message.text.len() < 720)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new(&message.text).monospace().size(11.0));
                            });
                    });
                } else {
                    let show_markdown = ctx
                        .data_mut(|data| {
                            data.get_temp::<display_preferences::DisplayPreferences>(egui::Id::new(
                                "chatarium-display-preferences",
                            ))
                        })
                        .unwrap_or_default()
                        .render_markdown;
                    if show_markdown {
                        for block in offline_reader::parse_markdown(&message.text) {
                            render_markdown_block(ui, &block, search_query, active_hit, ctx);
                            ui.add_space(6.0);
                        }
                    } else {
                        // Literal mode must keep Markdown markers, including
                        // inline backticks, exactly as they were authored.
                        ui.label(
                            egui::RichText::new(&message.text)
                                .color(egui::Color32::from_rgb(232, 234, 239)),
                        );
                    }
                }
                ui.add_space(5.0);
                let origin = message
                    .provenance_label
                    .as_deref()
                    .unwrap_or("source not specified");
                let metadata = message
                    .timestamp
                    .and_then(timestamp_display::utc_minute)
                    .map(|timestamp| format!("{label} · {timestamp} · {origin}"))
                    .unwrap_or_else(|| format!("{label} · {origin} · event #{}", message.sequence));
                ui.label(
                    egui::RichText::new(metadata)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(116, 121, 133)),
                );
            });
        })
        .response
}

pub(crate) fn render_markdown_block(
    ui: &mut egui::Ui,
    block: &offline_reader::MarkdownBlock,
    search_query: &str,
    active_hit: bool,
    ctx: &egui::Context,
) {
    match block {
        offline_reader::MarkdownBlock::Paragraph(text) => {
            render_markdown_lines(ui, text, search_query, active_hit, None);
        }
        offline_reader::MarkdownBlock::Heading { level, text } => {
            render_markdown_lines(ui, text, search_query, active_hit, Some(*level));
        }
        offline_reader::MarkdownBlock::UnorderedList(items) => {
            for item in items {
                render_markdown_list_item(ui, "•", item, search_query, active_hit);
            }
        }
        offline_reader::MarkdownBlock::OrderedList(items) => {
            for (index, item) in items.iter().enumerate() {
                render_markdown_list_item(
                    ui,
                    &format!("{}.", index + 1),
                    item,
                    search_query,
                    active_hit,
                );
            }
        }
        offline_reader::MarkdownBlock::BlockQuote(lines) => {
            egui::Frame::default()
                .fill(egui::Color32::from_rgb(36, 39, 47))
                .stroke(egui::Stroke::new(
                    2.0_f32,
                    egui::Color32::from_rgb(118, 151, 190),
                ))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    for line in lines {
                        render_markdown_lines(ui, line, search_query, active_hit, None);
                    }
                });
        }
        offline_reader::MarkdownBlock::CodeBlock { language, code } => {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(language.as_deref().unwrap_or("code"))
                        .monospace()
                        .size(10.0)
                        .color(egui::Color32::from_rgb(169, 180, 201)),
                );
                if ui.small_button("Copy code").clicked() {
                    ctx.copy_text(offline_reader::copy_payload(code));
                }
            });
            egui::Frame::default()
                .fill(egui::Color32::from_rgb(15, 17, 21))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        render_reader_lines(ui, code, search_query, true, active_hit);
                    });
                });
        }
    }
}

/// Keep list markers beside a vertical text lane. An outer
/// horizontal_wrapped() shifts continuation rows to the bubble's outer left
/// edge because egui places wrapped label rows at the horizontal UI origin.
/// A nonwrapping row with its own vertical text lane preserves the inset and
/// the hanging indent, including long source URLs from web-search results.
pub(crate) fn render_markdown_list_item(
    ui: &mut egui::Ui,
    marker: &str,
    text: &str,
    search_query: &str,
    active_hit: bool,
) -> egui::Rect {
    ui.horizontal_top(|ui| {
        ui.label(egui::RichText::new(marker).strong());
        ui.vertical(|ui| {
            render_markdown_lines(ui, text, search_query, active_hit, None);
        })
        .response
        .rect
    })
    .inner
}

/// Build one styled paragraph instead of one widget for every Markdown fragment.
/// egui's horizontal wrapping can wrap *between* widgets but cannot keep a
/// long URL/inline-code widget inside the enclosing message bubble. A single
/// LayoutJob preserves emphasis AND lets the text shaper break long words at
/// the maximum available width. Keep the durable source unchanged.
pub(crate) fn markdown_layout_job(
    text: &str,
    search_query: &str,
    active_hit: bool,
    heading_level: Option<u8>,
    body_font: egui::FontId,
    has_bold_face: bool,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for span in markdown_inline::parse(text) {
        let hits = offline_reader::search_hits(&span.text, search_query);
        let mut pieces = Vec::new();
        if hits.is_empty() {
            pieces.push((span.text.as_str(), false));
        } else {
            let mut at = 0;
            for (start, end) in hits {
                if start > at {
                    pieces.push((&span.text[at..start], false));
                }
                pieces.push((&span.text[start..end], true));
                at = end;
            }
            if at < span.text.len() {
                pieces.push((&span.text[at..], false));
            }
        }
        for (fragment, highlighted) in pieces {
            let mut font_id = body_font.clone();
            let mut color = egui::Color32::from_rgb(232, 234, 239);
            if span.code {
                font_id.family = egui::FontFamily::Monospace;
            }
            if let Some(level) = heading_level {
                font_id.size = match level {
                    1 => 22.0,
                    2 => 19.0,
                    3 => 17.0,
                    _ => 15.0,
                };
            }
            if span.bold || heading_level.is_some() {
                color = egui::Color32::WHITE;
                if !span.code && has_bold_face {
                    font_id.family =
                        egui::FontFamily::Name(system_fonts::MARKDOWN_BOLD_FAMILY.into());
                } else if !span.code && heading_level.is_none() {
                    font_id.size = 15.0;
                }
            }
            let mut format = egui::TextFormat {
                font_id,
                color,
                italics: span.italic,
                ..Default::default()
            };
            if span.strike {
                format.strikethrough = egui::Stroke::new(1.0, color);
            }
            if span.code {
                format.background = egui::Color32::from_rgb(20, 23, 30);
            }
            if highlighted {
                format.background = if active_hit {
                    egui::Color32::from_rgb(161, 120, 43)
                } else {
                    egui::Color32::from_rgb(91, 78, 38)
                };
            }
            job.append(fragment, 0.0, format);
        }
    }
    job
}

/// Render authored inline Markdown as a single width-constrained paragraph.
/// Code fences remain independently horizontally scrollable.
pub(crate) fn render_markdown_lines(
    ui: &mut egui::Ui,
    text: &str,
    search_query: &str,
    active_hit: bool,
    heading_level: Option<u8>,
) {
    let has_bold_face = system_fonts::markdown_bold_available(ui.ctx());
    let body_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(14.0));
    for line in text.split('\n') {
        let job = markdown_layout_job(
            line,
            search_query,
            active_hit,
            heading_level,
            body_font.clone(),
            has_bold_face,
        );
        // One galley uses the actual remaining bubble width. In particular,
        // a URL with no spaces is broken inside the card, never painted beyond
        // the message stroke or over adjacent UI.
        ui.add(egui::Label::new(job).wrap());
    }
}

#[cfg(test)]
mod markdown_bubble_wrap_regressions {
    use super::*;

    #[test]
    fn smithsonian_web_search_long_urls_are_one_styled_paragraph() {
        let input = "Read **Ask Smithsonian** at https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai — for the history.";
        let job = markdown_layout_job(
            input,
            "",
            false,
            None,
            egui::FontId::proportional(14.0),
            true,
        );
        assert_eq!(
            job.text,
            "Read Ask Smithsonian at https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai — for the history."
        );
        assert!(job.sections.iter().any(|section| {
            section.format.font_id.family
                == egui::FontFamily::Name(system_fonts::MARKDOWN_BOLD_FAMILY.into())
        }));
    }

    #[test]
    fn web_search_list_continuations_keep_the_inner_gutter() {
        // Regresses the real agent response: the old nested
        // horizontal_wrapped() placed URL continuation rows flush against
        // the bubble outline rather than inside the content lane.
        let context = egui::Context::default();
        let mut measured = None;
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::Frame::default()
                    .inner_margin(egui::Margin::symmetric(22, 13))
                    .show(ui, |ui| {
                        ui.set_width(310.0);
                        let content_left = ui.cursor().left();
                        let content_right = content_left + ui.available_width();
                        let body = render_markdown_list_item(
                            ui,
                            "•",
                            "**Ask Smithsonian: Are Cats Domesticated?** — https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai",
                            "",
                            false,
                        );
                        measured = Some((content_left, content_right, body));
                    });
            });
        });
        let (left, right, body) = measured.expect("native layout ran");
        assert!(
            body.left() >= left + 8.0,
            "list continuation lost its gutter: body={} parent={left}",
            body.left()
        );
        assert!(
            body.right() <= right + 1.0,
            "list text exceeds its content lane: {} > {right}",
            body.right()
        );
    }

    #[test]
    fn narrow_web_search_paragraph_wraps_within_its_width() {
        let context = egui::Context::default();
        let mut observed = None;
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut job = markdown_layout_job(
                    "https://www.smithsonianmag.com/smithsonian-institution/ask-smithsonian-are-cats-domesticated-180955111/?utm_source=openai",
                    "",
                    false,
                    None,
                    egui::FontId::proportional(14.0),
                    false,
                );
                job.wrap.max_width = 230.0;
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                observed = Some((galley.rows.len(), galley.size().x));
            });
        });
        let (rows, width) = observed.expect("layout ran");
        assert!(rows > 1, "unbroken URL must wrap over multiple rows");
        assert!(width <= 231.0, "URL escaped the bubble: {width}");
    }
}

pub(crate) fn render_reader_lines(
    ui: &mut egui::Ui,
    text: &str,
    search_query: &str,
    monospace: bool,
    active_hit: bool,
) {
    // Count once, not once per rendered line: the latter is quadratic
    // for large multiline pasted messages and code blocks.
    let line_count = text.lines().count();
    for (line_index, line) in text.split('\n').enumerate() {
        ui.horizontal_wrapped(|ui| {
            for (segment, inline_code) in offline_reader::inline_segments(line) {
                let segments = offline_reader::search_hits(&segment, search_query);
                if segments.is_empty() {
                    let mut rich = egui::RichText::new(segment);
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                    continue;
                }
                let mut cursor = 0;
                for (start, end) in segments {
                    if start > cursor {
                        let mut rich = egui::RichText::new(segment[cursor..start].to_owned());
                        if monospace || inline_code {
                            rich = rich.monospace();
                        }
                        ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                    }
                    let mut rich = egui::RichText::new(segment[start..end].to_owned())
                        .background_color(if active_hit {
                            egui::Color32::from_rgb(161, 120, 43)
                        } else {
                            egui::Color32::from_rgb(91, 78, 38)
                        });
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(255, 244, 190)));
                    cursor = end;
                }
                if cursor < segment.len() {
                    let mut rich = egui::RichText::new(segment[cursor..].to_owned());
                    if monospace || inline_code {
                        rich = rich.monospace();
                    }
                    ui.label(rich.color(egui::Color32::from_rgb(232, 234, 239)));
                }
            }
        });
        if line_index + 1 < line_count {
            ui.add_space(2.0);
        }
    }
}

pub(crate) fn adjust_reader_scroll(ctx: &egui::Context, id: egui::Id, delta: f32) {
    let mut state = egui::scroll_area::State::load(ctx, id).unwrap_or_default();
    state.offset.y = (state.offset.y + delta).max(0.0);
    state.store(ctx, id);
}

pub(crate) fn set_reader_scroll(ctx: &egui::Context, id: egui::Id, offset: f32) {
    let mut state = egui::scroll_area::State::load(ctx, id).unwrap_or_default();
    state.offset.y = offset.max(0.0);
    state.store(ctx, id);
}
