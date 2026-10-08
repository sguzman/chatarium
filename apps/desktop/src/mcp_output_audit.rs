//! Read-only MCP result comparison against exact earlier tools/list observations.
//! The UI never treats a refreshed catalog as authority for an earlier call.

use chatarium_store::EventEnvelope;
use chatarium_core::LocalConversationId;
use chatarium_protocol::mcp_output_inspection::{
    McpOutputInspection, inspect_structured_tool_output,
};
use chatarium_protocol::mcp_wire::{McpResponse, decode_stdio_response, parse_tools_list_page};
use chatarium_store::tool_call_audit::ToolCallAuditRecord;
use chatarium_store::tool_outcome_audit::{ToolCallOutcomeKind, ToolCallOutcomeRecord};
use chatarium_store::tool_result_context_audit::tool_outcome_owning_conversation;
use chatarium_store::tool_stdio_preflight::MCP_LIST_TOOLS_OPERATION;
use eframe::egui;

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
    if target_outcome.call_id != target_call.call_id
        || target_outcome.provider_id != target_call.provider_id
        || target_outcome.source_session_id != target_call.source_session_id
        || target_outcome.call_recorded_sequence != target_call.recorded_sequence
        || catalog_outcome.call_id != catalog_call.call_id
        || catalog_outcome.provider_id != catalog_call.provider_id
        || catalog_outcome.source_session_id != catalog_call.source_session_id
        || catalog_outcome.call_recorded_sequence != catalog_call.recorded_sequence
        || catalog_call.recorded_sequence >= catalog_outcome.observed_sequence
        || target_call.recorded_sequence >= target_outcome.observed_sequence
    {
        return Err("catalog and tool result have inconsistent durable identities".to_owned());
    }
    let catalog_response = decode_stdio_response(
        catalog_outcome.text.as_str(),
        catalog_call.call_id.get(),
    )
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
    let response = decode_stdio_response(
        target_outcome.text.as_str(),
        target_call.call_id.get(),
    )
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
            ui.label("Only earlier tools/list observations from the same provider, source session and local conversation qualify. This does not verify a full JSON Schema or change context admission.");
            if tool_outcome_owning_conversation(events, target_outcome)
                .ok()
                .flatten()
                != Some(conversation_id)
            {
                ui.label("This tool result is not owned by the current local conversation.");
                return;
            }
            let mut matching = 0_usize;
            for catalog_call in calls.iter().rev() {
                if catalog_call.operation.as_str() != MCP_LIST_TOOLS_OPERATION
                    || catalog_call.provider_id != target_call.provider_id
                    || catalog_call.source_session_id != target_call.source_session_id
                {
                    continue;
                }
                let Some(catalog_outcome) = outcomes.iter().find(|record| {
                    record.call_id == catalog_call.call_id
                        && record.kind == ToolCallOutcomeKind::Result
                        && record.observed_sequence < target_call.recorded_sequence
                }) else {
                    continue;
                };
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
                        matching += 1;
                        ui.collapsing(
                            format!(
                                "Catalog observed #{} · call {} · {:?}",
                                catalog_outcome.observed_sequence,
                                catalog_call.call_id.get(),
                                inspection.verdict,
                            ),
                            |ui| {
                                ui.label(inspection.explanation);
                                ui.label(format!(
                                    "Provider {} · session {} · catalog #{} before call #{} · result #{}",
                                    target_call.provider_id.get(),
                                    target_call.source_session_id.get(),
                                    catalog_outcome.observed_sequence,
                                    target_call.recorded_sequence,
                                    target_outcome.observed_sequence,
                                ));
                                ui.label("This provisional comparison is not execution approval, current provider testimony, full JSON Schema validation, or context admission.");
                            },
                        );
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        matching += 1;
                        ui.label(format!(
                            "Catalog observation #{} cannot be compared: {reason}",
                            catalog_outcome.observed_sequence,
                        ));
                    }
                }
            }
            if matching == 0 {
                ui.label("No eligible earlier catalog advertises this operation. No output-schema validity is claimed.");
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::routing::RouteId;
    use chatarium_core::session::SessionId;
    use chatarium_core::tool::{ToolCallId, ToolOperationName, ToolProviderId};
    use chatarium_protocol::mcp_output_inspection::McpOutputVerdict;
    use serde_json::{Value, json};

    fn call(id: u64, operation: &str, provider: u64, session: u64, sequence: u64) -> ToolCallAuditRecord {
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
            text: json!({"jsonrpc":"2.0","id":call.call_id.get(),"result":result})
                .to_string(),
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
        let result = outcome(&call, 35, json!({
            "resultType":"complete",
            "content":[{"type":"text","text":"untrusted"}],
            "structuredContent":value
        }));
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
            &target_call, &target_result, &catalog_call, &catalog_result
        ).unwrap().unwrap();
        assert_eq!(check.verdict, McpOutputVerdict::PassedSupportedChecks);

        let (target_call, target_result) = target(json!({"temperature":"wrong"}));
        let check = inspect_prior_catalog_snapshot(
            &target_call, &target_result, &catalog_call, &catalog_result
        ).unwrap().unwrap();
        assert_eq!(check.verdict, McpOutputVerdict::Mismatch);
    }

    #[test]
    fn foreign_and_future_snapshots_are_ineligible() {
        let (catalog_call, catalog_result) = catalog(Some(json!({"type":"object"})));
        let (target_call, target_result) = target(json!({}));
        let mut different_provider = catalog_call.clone();
        different_provider.provider_id = ToolProviderId::new(99);
        assert!(inspect_prior_catalog_snapshot(
            &target_call, &target_result, &different_provider, &catalog_result
        ).unwrap().is_none());
        let mut different_session = catalog_call.clone();
        different_session.source_session_id = SessionId::new(99);
        assert!(inspect_prior_catalog_snapshot(
            &target_call, &target_result, &different_session, &catalog_result
        ).unwrap().is_none());
        let mut future = catalog_result.clone();
        future.observed_sequence = target_call.recorded_sequence + 1;
        assert!(inspect_prior_catalog_snapshot(
            &target_call, &target_result, &catalog_call, &future
        ).unwrap().is_none());
    }

    #[test]
    fn no_schema_and_unlisted_operation_are_not_success() {
        let (catalog_call, catalog_result) = catalog(None);
        let (target_call, target_result) = target(json!({}));
        let verdict = inspect_prior_catalog_snapshot(
            &target_call, &target_result, &catalog_call, &catalog_result
        ).unwrap().unwrap();
        assert_eq!(verdict.verdict, McpOutputVerdict::NoAdvertisedSchema);
        let mut unlisted_call = target_call.clone();
        unlisted_call.operation = ToolOperationName::new("other.read").unwrap();
        assert!(inspect_prior_catalog_snapshot(
            &unlisted_call, &target_result, &catalog_call, &catalog_result
        ).unwrap().is_none());
    }

    #[test]
    fn invalid_frames_and_identity_corruption_fail_closed() {
        let (catalog_call, mut catalog_result) = catalog(Some(json!({"type":"object"})));
        let (target_call, target_result) = target(json!({}));
        catalog_result.text = "not json".to_owned();
        assert!(inspect_prior_catalog_snapshot(
            &target_call, &target_result, &catalog_call, &catalog_result
        ).is_err());
        let (catalog_call, catalog_result) = catalog(Some(json!({"type":"object"})));
        let mut bad_result = target_result.clone();
        bad_result.call_id = ToolCallId::new(88);
        assert!(inspect_prior_catalog_snapshot(
            &target_call, &bad_result, &catalog_call, &catalog_result
        ).is_err());
    }
}
