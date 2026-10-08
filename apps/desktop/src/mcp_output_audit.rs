//! Read-only MCP result comparison against exact earlier tools/list observations.
//! The UI never treats a refreshed catalog as authority for an earlier call.

use chatarium_core::LocalConversationId;
use chatarium_protocol::mcp_output_inspection::{
    McpOutputInspection, inspect_structured_tool_output,
};
use chatarium_protocol::mcp_wire::{
    McpListedTool, McpResponse, decode_stdio_response, parse_tools_list_page,
};
use chatarium_store::EventEnvelope;
use chatarium_store::tool_call_audit::ToolCallAuditRecord;
use chatarium_store::tool_outcome_audit::{ToolCallOutcomeKind, ToolCallOutcomeRecord};
use chatarium_store::tool_result_context_audit::tool_outcome_owning_conversation;
use chatarium_store::tool_stdio_preflight::MCP_LIST_TOOLS_OPERATION;
use eframe::egui;
use std::collections::BTreeMap;

const RECENT_CATALOGS: usize = 4;
const MAX_CATALOGS: usize = 64;
const SCHEMA_PREVIEW_BYTES: usize = 8 * 1024;

fn schema_preview(schema: &serde_json::Value) -> String {
    let Ok(json) = serde_json::to_string_pretty(schema) else {
        return "Schema preview unavailable".to_owned();
    };
    if json.len() <= SCHEMA_PREVIEW_BYTES {
        return json;
    }
    let mut end = SCHEMA_PREVIEW_BYTES;
    while !json.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[PREVIEW TRUNCATED · exact schema retained in recorded catalog outcome]",
        &json[..end]
    )
}

/// Provider testimony: it does not grant calls or validate an output.
pub fn render_catalog_output_schema(ui: &mut egui::Ui, tool: &McpListedTool) {
    let Some(schema) = tool.output_schema.as_ref() else {
        ui.label("outputSchema: not advertised");
        return;
    };
    ui.collapsing("Review advertised outputSchema · untrusted", |ui| {
        ui.label("Bounded read-only preview; raw recorded catalog remains authoritative.");
        ui.label(egui::RichText::new(schema_preview(schema)).monospace());
    });
}

/// Exact journal identity and chronology, independent of protocol framing.
fn outcome_matches_call(call: &ToolCallAuditRecord, outcome: &ToolCallOutcomeRecord) -> bool {
    let (Some(route), Some(bound)) = (call.route_id, call.route_bound_sequence) else {
        return false;
    };
    outcome.call_id == call.call_id
        && outcome.route_id == route
        && outcome.provider_id == call.provider_id
        && outcome.source_session_id == call.source_session_id
        && outcome.call_recorded_sequence == call.recorded_sequence
        && outcome.route_bound_sequence == bound
        && call.recorded_sequence < bound
        && bound < outcome.dispatch_sequence
        && outcome.dispatch_sequence < outcome.observed_sequence
}

/// Compare only a prior, exact catalog from the same provider and source
/// session. The caller must independently check the conversation owner.
pub fn inspect_prior_catalog_snapshot(
    target_call: &ToolCallAuditRecord,
    target_outcome: &ToolCallOutcomeRecord,
    catalog_call: &ToolCallAuditRecord,
    catalog_outcome: &ToolCallOutcomeRecord,
) -> Result<Option<McpOutputInspection>, String> {
    if catalog_call.operation.as_str() != MCP_LIST_TOOLS_OPERATION
        || target_call.operation.as_str() == MCP_LIST_TOOLS_OPERATION
        || target_outcome.kind != ToolCallOutcomeKind::Result
        || catalog_outcome.kind != ToolCallOutcomeKind::Result
        || target_call.provider_id != catalog_call.provider_id
        || target_call.source_session_id != catalog_call.source_session_id
        || catalog_outcome.observed_sequence >= target_call.recorded_sequence
    {
        return Ok(None);
    }
    if !outcome_matches_call(target_call, target_outcome)
        || !outcome_matches_call(catalog_call, catalog_outcome)
    {
        return Err("catalog and tool result have inconsistent durable identities".to_owned());
    }
    let catalog_response =
        decode_stdio_response(catalog_outcome.text.as_str(), catalog_call.call_id.get())
            .map_err(|error| format!("recorded catalog MCP frame rejected: {error:?}"))?;
    let McpResponse::Complete(catalog_result) = catalog_response else {
        return Err("recorded catalog is not a complete MCP response".to_owned());
    };
    let page = parse_tools_list_page(&catalog_result)
        .map_err(|error| format!("recorded catalog page rejected: {error:?}"))?;
    let Some(advertised) = page
        .tools
        .iter()
        .find(|tool| tool.name == target_call.operation.as_str())
    else {
        return Ok(None);
    };
    let response = decode_stdio_response(target_outcome.text.as_str(), target_call.call_id.get())
        .map_err(|error| format!("recorded tool MCP frame rejected: {error:?}"))?;
    let McpResponse::Complete(result) = response else {
        return Err("recorded tool result is not a complete MCP response".to_owned());
    };
    inspect_structured_tool_output(advertised, &result)
        .map(Some)
        .map_err(|error| format!("recorded structured result rejected: {error:?}"))
}

/// A purely observational expander. No user input, process, route, journal
/// append or permission change is available from this panel.
pub fn render_snapshot_comparisons(
    ui: &mut egui::Ui,
    events: &[EventEnvelope],
    calls: &[ToolCallAuditRecord],
    outcomes: &[ToolCallOutcomeRecord],
    target_call: &ToolCallAuditRecord,
    target_outcome: &ToolCallOutcomeRecord,
    conversation_id: LocalConversationId,
) {
    if target_outcome.kind != ToolCallOutcomeKind::Result
        || target_call.operation.as_str() == MCP_LIST_TOOLS_OPERATION
    {
        return;
    }
    ui.collapsing(
        "Compare with earlier MCP catalog snapshot · read-only",
        |ui| {
            ui.label("Historical provider testimony only. Neither full schema validation nor execution permission.");
            if tool_outcome_owning_conversation(events, target_outcome)
                .ok()
                .flatten()
                != Some(conversation_id)
            {
                ui.label("Tool result is not owned by this conversation.");
                return;
            }
            let indexed: BTreeMap<_, _> = outcomes
                .iter()
                .map(|outcome| (outcome.call_id, outcome))
                .collect();
            let candidate_catalogs = calls
                .iter()
                .rev()
                .filter(|call| {
                    call.operation.as_str() == MCP_LIST_TOOLS_OPERATION
                        && call.provider_id == target_call.provider_id
                        && call.source_session_id == target_call.source_session_id
                })
                .filter_map(|call| {
                    let outcome = indexed.get(&call.call_id).copied()?;
                    if outcome.kind != ToolCallOutcomeKind::Result
                        || outcome.observed_sequence >= target_call.recorded_sequence
                    {
                        return None;
                    }
                    Some((call, outcome))
                })
                .take(MAX_CATALOGS + 1)
                .collect::<Vec<_>>();
            let shown = candidate_catalogs.len().min(MAX_CATALOGS);
            if shown == 0 {
                ui.label("No earlier catalog observations for this provider/session.");
                return;
            }
            let recent = shown.min(RECENT_CATALOGS);
            ui.label(format!("Recent catalog observations · {recent}"));
            render_catalog_rows(
                ui,
                events,
                &candidate_catalogs[..recent],
                target_call,
                target_outcome,
                conversation_id,
            );
            if shown > recent {
                ui.collapsing(
                    format!("Older catalog observations · {}", shown - recent),
                    |ui| {
                        render_catalog_rows(
                            ui,
                            events,
                            &candidate_catalogs[recent..shown],
                            target_call,
                            target_outcome,
                            conversation_id,
                        );
                    },
                );
            }
            if candidate_catalogs.len() > MAX_CATALOGS {
                ui.label("Display capped at 64 recent candidate catalogs. Earlier observations remain in the immutable audit and are not assigned verdicts.");
            }
        },
    );
}

fn render_catalog_rows(
    ui: &mut egui::Ui,
    events: &[EventEnvelope],
    candidates: &[(&ToolCallAuditRecord, &ToolCallOutcomeRecord)],
    target_call: &ToolCallAuditRecord,
    target_outcome: &ToolCallOutcomeRecord,
    conversation_id: LocalConversationId,
) {
    let mut visible = 0_usize;
    for &(catalog_call, catalog_outcome) in candidates {
        if tool_outcome_owning_conversation(events, catalog_outcome)
            .ok()
            .flatten()
            != Some(conversation_id)
        {
            continue;
        }
        match inspect_prior_catalog_snapshot(
            target_call,
            target_outcome,
            catalog_call,
            catalog_outcome,
        ) {
            Ok(Some(inspection)) => {
                visible += 1;
                let label = match inspection.verdict {
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::PassedSupportedChecks => "SUPPORTED CHECKS PASS · NOT FULL VALIDATION",
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::Mismatch => "DEFINITE MISMATCH",
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::Inconclusive => "INCONCLUSIVE",
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::NoAdvertisedSchema => "NO OUTPUT SCHEMA",
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::NoStructuredContent => "NO STRUCTURED RESULT",
                    chatarium_protocol::mcp_output_inspection::McpOutputVerdict::ToolReportedError => "TOOL ERROR",
                };
                ui.collapsing(
                    format!("Catalog #{} · {label}", catalog_outcome.observed_sequence),
                    |ui| {
                        ui.label(inspection.explanation);
                        ui.label(format!(
                            "Catalog call {} · provider {} · session {}",
                            catalog_call.call_id.get(),
                            target_call.provider_id.get(),
                            target_call.source_session_id.get(),
                        ));
                        ui.label(format!(
                            "Catalog observed #{} before call recorded #{} · result #{}",
                            catalog_outcome.observed_sequence,
                            target_call.recorded_sequence,
                            target_outcome.observed_sequence,
                        ));
                        ui.label("This comparison grants no authority or context admission.");
                    },
                );
            }
            Ok(None) => {}
            Err(reason) => {
                visible += 1;
                ui.label(format!(
                    "Catalog #{} · comparison unavailable: {reason}",
                    catalog_outcome.observed_sequence,
                ));
            }
        }
    }
    if visible == 0 {
        ui.label("None of these catalog pages advertises this tool in this conversation. No validity is inferred.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::routing::RouteId;
    use chatarium_core::session::SessionId;
    use chatarium_core::tool::{ToolCallId, ToolOperationName, ToolProviderId};
    use chatarium_protocol::mcp_output_inspection::McpOutputVerdict;
    use serde_json::{Value, json};

    fn call(
        id: u64,
        operation: &str,
        provider: u64,
        session: u64,
        sequence: u64,
    ) -> ToolCallAuditRecord {
        ToolCallAuditRecord {
            call_id: ToolCallId::new(id),
            source_session_id: SessionId::new(session),
            provider_id: ToolProviderId::new(provider),
            operation: ToolOperationName::new(operation).unwrap(),
            arguments_text: "{}".to_owned(),
            recorded_sequence: sequence,
            route_id: Some(RouteId::new(id + 100)),
            route_bound_sequence: Some(sequence + 1),
        }
    }

    fn outcome(call: &ToolCallAuditRecord, observed: u64, result: Value) -> ToolCallOutcomeRecord {
        ToolCallOutcomeRecord {
            call_id: call.call_id,
            route_id: call.route_id.unwrap(),
            provider_id: call.provider_id,
            source_session_id: call.source_session_id,
            kind: ToolCallOutcomeKind::Result,
            text: json!({"jsonrpc":"2.0","id":call.call_id.get(),"result":result}).to_string(),
            call_recorded_sequence: call.recorded_sequence,
            route_bound_sequence: call.recorded_sequence + 1,
            dispatch_sequence: call.recorded_sequence + 2,
            observed_sequence: observed,
        }
    }

    fn catalog(schema: Option<Value>) -> (ToolCallAuditRecord, ToolCallOutcomeRecord) {
        let call = call(10, MCP_LIST_TOOLS_OPERATION, 3, 7, 20);
        let mut tool = json!({"name":"weather.read","inputSchema":{"type":"object"}});
        if let Some(schema) = schema {
            tool["outputSchema"] = schema;
        }
        let result = outcome(&call, 25, json!({"resultType":"complete","tools":[tool]}));
        (call, result)
    }

    fn target(value: Value) -> (ToolCallAuditRecord, ToolCallOutcomeRecord) {
        let call = call(12, "weather.read", 3, 7, 30);
        let result = outcome(
            &call,
            35,
            json!({
                "resultType":"complete",
                "content":[{"type":"text","text":"untrusted"}],
                "structuredContent":value
            }),
        );
        (call, result)
    }

    #[test]
    fn exact_earlier_catalog_can_pass_or_detect_mismatch() {
        let (catalog_call, catalog_result) = catalog(Some(json!({
            "type":"object","required":["temperature"],
            "properties":{"temperature":{"type":"number"}}
        })));
        let (target_call, target_result) = target(json!({"temperature":25}));
        let check = inspect_prior_catalog_snapshot(
            &target_call,
            &target_result,
            &catalog_call,
            &catalog_result,
        )
        .unwrap()
        .unwrap();
        assert_eq!(check.verdict, McpOutputVerdict::PassedSupportedChecks);

        let (target_call, target_result) = target(json!({"temperature":"wrong"}));
        let check = inspect_prior_catalog_snapshot(
            &target_call,
            &target_result,
            &catalog_call,
            &catalog_result,
        )
        .unwrap()
        .unwrap();
        assert_eq!(check.verdict, McpOutputVerdict::Mismatch);
    }

    #[test]
    fn foreign_and_future_snapshots_are_ineligible() {
        let (catalog_call, catalog_result) = catalog(Some(json!({"type":"object"})));
        let (target_call, target_result) = target(json!({}));
        let mut different_provider = catalog_call.clone();
        different_provider.provider_id = ToolProviderId::new(99);
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &target_result,
                &different_provider,
                &catalog_result
            )
            .unwrap()
            .is_none()
        );
        let mut different_session = catalog_call.clone();
        different_session.source_session_id = SessionId::new(99);
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &target_result,
                &different_session,
                &catalog_result
            )
            .unwrap()
            .is_none()
        );
        let mut future = catalog_result.clone();
        future.observed_sequence = target_call.recorded_sequence + 1;
        assert!(
            inspect_prior_catalog_snapshot(&target_call, &target_result, &catalog_call, &future)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn no_schema_and_unlisted_operation_are_not_success() {
        let (catalog_call, catalog_result) = catalog(None);
        let (target_call, target_result) = target(json!({}));
        let verdict = inspect_prior_catalog_snapshot(
            &target_call,
            &target_result,
            &catalog_call,
            &catalog_result,
        )
        .unwrap()
        .unwrap();
        assert_eq!(verdict.verdict, McpOutputVerdict::NoAdvertisedSchema);
        let mut unlisted_call = target_call.clone();
        unlisted_call.operation = ToolOperationName::new("other.read").unwrap();
        assert!(
            inspect_prior_catalog_snapshot(
                &unlisted_call,
                &target_result,
                &catalog_call,
                &catalog_result
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn route_and_dispatch_identity_are_required() {
        let (catalog_call, catalog_result) = catalog(Some(json!({"type":"object"})));
        let (target_call, target_result) = target(json!({}));
        let mut wrong_route = target_result.clone();
        wrong_route.route_id = RouteId::new(999);
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &wrong_route,
                &catalog_call,
                &catalog_result
            )
            .is_err()
        );
        let mut wrong_dispatch = target_result.clone();
        wrong_dispatch.dispatch_sequence = target_result.observed_sequence;
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &wrong_dispatch,
                &catalog_call,
                &catalog_result
            )
            .is_err()
        );
    }

    #[test]
    fn output_schema_preview_bounds_utf8_and_labels_truncation() {
        let small = json!({"type":"object"});
        assert_eq!(
            schema_preview(&small),
            serde_json::to_string_pretty(&small).unwrap()
        );
        let large = json!({"description":"é".repeat(SCHEMA_PREVIEW_BYTES)});
        let preview = schema_preview(&large);
        assert!(preview.contains("PREVIEW TRUNCATED"));
        assert!(preview.len() < SCHEMA_PREVIEW_BYTES + 110);
    }

    #[test]
    fn invalid_frames_and_identity_corruption_fail_closed() {
        let (catalog_call, mut catalog_result) = catalog(Some(json!({"type":"object"})));
        let (target_call, target_result) = target(json!({}));
        catalog_result.text = "not json".to_owned();
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &target_result,
                &catalog_call,
                &catalog_result
            )
            .is_err()
        );
        let (catalog_call, catalog_result) = catalog(Some(json!({"type":"object"})));
        let mut bad_result = target_result.clone();
        bad_result.call_id = ToolCallId::new(88);
        assert!(
            inspect_prior_catalog_snapshot(
                &target_call,
                &bad_result,
                &catalog_call,
                &catalog_result
            )
            .is_err()
        );
    }
}
