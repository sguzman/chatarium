//! Read-only UI for dispatch evidence, with conversation-scoped transient caching.
use super::*;

const REVERSE_PROVENANCE_PAGE_SIZE: usize = 12;

fn render_reverse_tool_provenance(
    ui: &mut egui::Ui,
    rows: &[RecordedDispatchManifest],
    conversation_id: LocalConversationId,
) {
    ui.collapsing("Reverse MCP provenance · find requests using a result", |ui| {
        ui.label("Look up one immutable MCP call ID across this conversation's manifested requests. 'Included' describes the composed input, not proof of remote receipt or model attention.");
        let input_id = reverse_query_id(conversation_id);
        let mut query = ui
            .ctx()
            .data_mut(|data| data.get_temp::<String>(input_id).unwrap_or_default());
        ui.horizontal(|ui| {
            ui.label("Call ID");
            ui.add(
                egui::TextEdit::singleline(&mut query)
                    .desired_width(125.0)
                    .hint_text("e.g. 42"),
            );
            if ui.button("Clear").clicked() {
                query.clear();
            }
        });

        // Shortcuts are derived only from the already verified historical
        // manifest rows. Manual entry still reaches any older call ID.
        let mut suggested = BTreeSet::new();
        ui.horizontal_wrapped(|ui| {
            ui.label("Recent recorded calls:");
            for dispatch in rows.iter() {
                for evidence in &dispatch.listed {
                    if suggested.insert(evidence.call_id) {
                        if ui
                            .selectable_label(
                                query == evidence.call_id.to_string(),
                                format!("#{}", evidence.call_id),
                            )
                            .clicked()
                        {
                            query = evidence.call_id.to_string();
                        }
                        if suggested.len() >= 8 {
                            break;
                        }
                    }
                }
                if suggested.len() >= 8 {
                    break;
                }
            }
            if suggested.is_empty() {
                ui.label("none in recorded manifest details");
            }
        });
        ui.ctx().data_mut(|data| data.insert_temp(input_id, query.clone()));
        if query.trim().is_empty() {
            ui.label("Select a recorded call or enter its numeric ID to inspect historical use.");
            return;
        }
        let call_id = match query.trim().parse::<u64>() {
            Ok(value) if value > 0 => value,
            _ => {
                ui.label("Enter a nonzero decimal tool call ID.");
                return;
            }
        };
        let report = match reverse_tool_provenance(rows, call_id) {
            Ok(report) => report,
            Err(error) => {
                ui.label(format!("Reverse provenance blocked: {error}"));
                return;
            }
        };
        ui.label(format!(
            "Manifested attempts · {} INCLUDED · {} admitted but OMITTED · {} UNKNOWN (truncated) · {} NOT ELIGIBLE (complete manifests)",
            report.included,
            report.admitted_but_omitted,
            report.uncertain_truncated,
            report.not_eligible_in_complete_manifests,
        ));
        if let Some(identity) = report.identity {
            ui.label(format!(
                "Terminal outcome #{} · provider {} · session {} · route {} · {}",
                identity.outcome_sequence,
                identity.provider_id,
                identity.source_session_id,
                identity.route_id,
                identity.outcome_kind,
            ));
        } else {
            ui.label("Call identity not present in retained manifest details. This is not proof that the result was never used.");
        }
        ui.label("Older requests predating context manifests cannot be reconstructed. A truncated manifest hides the disposition of unlisted calls.");
        if ui.button("Copy this lookup as JSON").clicked() {
            let export = reverse_tool_provenance_json(&report);
            if let Ok(json) = serde_json::to_string_pretty(&export) {
                ui.ctx().copy_text(json);
            }
        }
        if report.entries.is_empty() {
            ui.label("No positive or truncated-unknown matches in manifested history.");
            return;
        }
        let page_id = ui
            .id()
            .with(("reverse-mcp-call-page", conversation_id.to_string(), call_id));
        let mut page = ui
            .ctx()
            .data_mut(|data| data.get_temp::<usize>(page_id).unwrap_or(0));
        let total_pages = report.entries.len().div_ceil(REVERSE_PROVENANCE_PAGE_SIZE);
        page = page.min(total_pages - 1);
        ui.horizontal(|ui| {
            if ui.add_enabled(page > 0, egui::Button::new("Newer")).clicked() {
                page -= 1;
            }
            ui.label(format!(
                "Page {} / {} · {} direct or uncertain attempts",
                page + 1,
                total_pages,
                report.entries.len(),
            ));
            if ui
                .add_enabled(page + 1 < total_pages, egui::Button::new("Older"))
                .clicked()
            {
                page += 1;
            }
        });
        ui.ctx().data_mut(|data| data.insert_temp(page_id, page));
        for entry in report
            .entries
            .iter()
            .skip(page * REVERSE_PROVENANCE_PAGE_SIZE)
            .take(REVERSE_PROVENANCE_PAGE_SIZE)
        {
            ui.collapsing(
                format!(
                    "Dispatch #{} · {} · {}",
                    entry.dispatch.sequence,
                    entry.dispatch.request_class,
                    entry.disposition.label(),
                ),
                |ui| {
                    ui.label(format!("Local turn {}", entry.dispatch.turn_id));
                    ui.label(format!(
                        "Transport status: {}",
                        entry.dispatch.transport.status()
                    ));
                    if let Some(reason) = entry.dispatch.listed.iter()
                        .find(|evidence| evidence.call_id == report.call_id)
                        .and_then(|evidence| evidence.composition_reason)
                    {
                        ui.label(format!(
                            "Composition reason: {}",
                            reason.stable_name(),
                        ));
                    }
                    if entry.disposition == ToolProvenanceDisposition::UnknownBecauseManifestTruncated {
                        ui.label("No per-call determination is possible from this truncated manifest. Check other evidence before drawing conclusions.");
                    }
                    if ui.button("Copy request audit as JSON").clicked() {
                        let export = portable_audit_json(entry.dispatch);
                        if let Ok(json) = serde_json::to_string_pretty(&export) {
                            ui.ctx().copy_text(json);
                        }
                    }
                },
            );
        }
    });
}

/// The journal is append-only while Chatarium is running. A transient UI
/// cache may reuse this validated projection only when its owner and
/// journal's observed high-water are unchanged; it never survives restart.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JournalHighWater {
    event_count: usize,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
    first_scope: Option<String>,
    last_scope: Option<String>,
    last_kind: Option<EventKind>,
    last_payload_bytes: Option<usize>,
}

impl JournalHighWater {
    fn capture(events: &[EventEnvelope]) -> Self {
        Self {
            event_count: events.len(),
            first_sequence: events.first().map(|event| event.sequence),
            last_sequence: events.last().map(|event| event.sequence),
            first_scope: events.first().and_then(|event| event.scope.clone()),
            last_scope: events.last().and_then(|event| event.scope.clone()),
            last_kind: events.last().map(|event| event.kind),
            last_payload_bytes: events.last().map(|event| event.payload.len()),
        }
    }
}

#[derive(Clone)]
pub(super) struct DispatchHistoryCache {
    conversation_id: String,
    high_water: JournalHighWater,
    pub(super) rows: Result<Vec<RecordedDispatchManifest>, String>,
}

impl DispatchHistoryCache {
    pub(super) fn build(events: &[EventEnvelope], conversation_id: LocalConversationId) -> Self {
        Self {
            conversation_id: conversation_id.to_string(),
            high_water: JournalHighWater::capture(events),
            rows: dispatch_history(events, conversation_id),
        }
    }

    pub(super) fn matches(
        &self,
        events: &[EventEnvelope],
        conversation_id: LocalConversationId,
    ) -> bool {
        self.conversation_id == conversation_id.to_string()
            && self.high_water == JournalHighWater::capture(events)
    }
}

fn cached_history(
    ui: &egui::Ui,
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Arc<DispatchHistoryCache> {
    let cache_id = ui.id().with("mcp-dispatch-history-transient-cache");
    if let Some(existing) = ui
        .ctx()
        .data_mut(|data| data.get_temp::<Arc<DispatchHistoryCache>>(cache_id))
    {
        if existing.matches(events, conversation_id) {
            return existing;
        }
    }
    let fresh = Arc::new(DispatchHistoryCache::build(events, conversation_id));
    ui.ctx()
        .data_mut(|data| data.insert_temp(cache_id, Arc::clone(&fresh)));
    fresh
}

/// Visible history of what Chatarium assembled before attempting transport.
/// This cannot assert that a server received, retained or used a request.
pub fn render_dispatches(
    ui: &mut egui::Ui,
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) {
    let cache = cached_history(ui, events, conversation_id);
    match cache.rows.as_ref() {
        Ok(rows) if rows.is_empty() => {
            ui.label("No manifested dispatch attempts for this local conversation. Older attempts did not record this metadata.");
        }
        Ok(rows) => {
            ui.label("Journaled request-preparation evidence, not proof of remote receipt or model attention. Transport outcomes are correlated by exact turn, request ID and durable sequence.");
            render_reverse_tool_provenance(ui, rows, conversation_id);
            let page_id = ui
                .id()
                .with(("mcp-dispatch-history-page", conversation_id.to_string()));
            let mut page = ui
                .ctx()
                .data_mut(|data| data.get_temp::<usize>(page_id).unwrap_or(0));
            let pages = rows.len().div_ceil(MAX_VISIBLE_DISPATCHES);
            page = page.min(pages.saturating_sub(1));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(page > 0, egui::Button::new("Newer"))
                    .clicked()
                {
                    page -= 1;
                }
                ui.label(format!(
                    "Page {} / {} · {} recorded attempts",
                    page + 1,
                    pages,
                    rows.len()
                ));
                if ui
                    .add_enabled(page + 1 < pages, egui::Button::new("Older"))
                    .clicked()
                {
                    page += 1;
                }
            });
            ui.ctx().data_mut(|data| data.insert_temp(page_id, page));
            for item in rows
                .iter()
                .skip(page * MAX_VISIBLE_DISPATCHES)
                .take(MAX_VISIBLE_DISPATCHES)
            {
                ui.collapsing(
                    format!(
                        "{} · turn {} · event #{} · {} included / {} admitted",
                        item.request_class,
                        item.turn_id,
                        item.sequence,
                        item.included_total,
                        item.eligible_total,
                    ),
                    |ui| {
                        ui.label(
                            egui::RichText::new(item.transport.status()).strong(),
                        );
                        let timeline = item.transport.timeline();
                        if timeline.is_empty() {
                            ui.label("No turn-correlated transport observation yet.");
                        } else {
                            for (sequence, kind) in timeline {
                                ui.label(
                                    egui::RichText::new(format!("Event #{sequence} · {kind}"))
                                        .monospace()
                                        .small(),
                                );
                            }
                        }
                        if ui.button("Copy this audit as JSON").clicked() {
                            let report = portable_audit_json(item);
                            if let Ok(text) = serde_json::to_string_pretty(&report) {
                                ui.ctx().copy_text(text);
                            }
                        }
                        ui.label(format!(
                            "MCP evidence · {} eligible · {} included in composed payload · {} omitted by this dispatch path",
                            item.eligible_total,
                            item.included_total,
                            item.omitted_total,
                        ));
                        ui.label(format!(
                            "Entire composed context · {} included sources · {} omitted sources · {} included UTF-8 bytes (not tokens)",
                            item.included_items,
                            item.omitted_items,
                            item.included_bytes,
                        ));
                        for evidence in &item.listed {
                            let disposition = if evidence.included { "included" } else { "omitted" };
                            let reason = evidence.composition_reason
                                .map(ToolCompositionReason::stable_name)
                                .unwrap_or("not recorded in legacy manifest");
                            ui.label(
                                egui::RichText::new(format!(
                                    "call {} · provider {} · session {} · route {} · {} · {} ({}) · outcome #{} · admitted #{}",
                                    evidence.call_id,
                                    evidence.provider_id,
                                    evidence.source_session_id,
                                    evidence.route_id,
                                    evidence.outcome_kind,
                                    disposition,
                                    reason,
                                    evidence.outcome_sequence,
                                    evidence.admitted_sequence,
                                ))
                                .monospace()
                                .small(),
                            );
                        }
                        if item.truncated {
                            ui.label("DETAIL LIST TRUNCATED at 32 newest admissions. Totals include all admitted results; unlisted items are not silently marked included.");
                        }
                    },
                );
            }
        }
        Err(error) => {
            ui.label(format!(
                "Dispatch-context history blocked by invalid journal evidence: {error}"
            ));
        }
    }
}
