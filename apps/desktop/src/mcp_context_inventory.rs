//! Conversation-scoped, read-only projection of currently admitted MCP evidence.
//!
//! This inventory is independent of provider selection and the 16-result
//! review window. Its input is the same verified journal replay consumed by
//! normal conversation Context Composer. A user may explicitly *exclude*
//! a single item through the existing checked persistence path; rendering
//! never changes the journal, grants permissions, or runs a tool.

use chatarium_core::LocalConversationId;
use chatarium_core::tool::ToolCallId;
use chatarium_store::tool_call_audit::ToolCallAuditRecord;
use chatarium_store::tool_result_context_audit::{
    AdmittedToolResult, MAX_CONTEXT_TOOL_RESULT_BYTES, ToolResultContextDecision,
};
use eframe::egui;
use std::collections::BTreeSet;

/// Complete admission evidence with correlated immutable call metadata.
pub struct AdmittedEvidenceRow<'a> {
    pub admitted: &'a AdmittedToolResult,
    pub call: &'a ToolCallAuditRecord,
}

/// Validates the entire supplied set before exposing any UI controls.
/// The authoritative store already verifies outcome ownership and exact
/// terminal bytes. Here we additionally require that every call joins its
/// immutable metadata without duplicates, cross-conversation leakage,
/// stale decisions or impossible journal chronology.
pub fn verified_inventory<'a>(
    admitted: &'a [AdmittedToolResult],
    calls: &'a [ToolCallAuditRecord],
    conversation_id: LocalConversationId,
) -> Result<Vec<AdmittedEvidenceRow<'a>>, String> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::with_capacity(admitted.len());
    for item in admitted {
        let record = &item.record;
        if record.conversation_id != conversation_id {
            return Err(format!(
                "call {} belongs to another conversation",
                record.call_id.get()
            ));
        }
        if record.decision != ToolResultContextDecision::Admit
            || item.text.len() > MAX_CONTEXT_TOOL_RESULT_BYTES
            || record.first_decision_sequence <= record.outcome_sequence
            || record.last_decision_sequence < record.first_decision_sequence
        {
            return Err(format!(
                "call {} has invalid admission evidence",
                record.call_id.get()
            ));
        }
        if !seen.insert(record.call_id) {
            return Err(format!(
                "duplicate admitted tool call {}",
                record.call_id.get()
            ));
        }
        let mut matching_calls = calls.iter().filter(|call| call.call_id == record.call_id);
        let call = matching_calls.next().ok_or_else(|| {
            format!(
                "admitted tool call {} lacks immutable call metadata",
                record.call_id.get()
            )
        })?;
        if matching_calls.next().is_some() {
            return Err(format!("duplicate immutable call {}", record.call_id.get()));
        }
        if call.provider_id != record.provider_id
            || call.source_session_id != record.source_session_id
            || call.route_id != Some(record.route_id)
            || call.recorded_sequence >= record.outcome_sequence
            || call.route_bound_sequence.is_none_or(|sequence| {
                sequence <= call.recorded_sequence || sequence >= record.outcome_sequence
            })
        {
            return Err(format!(
                "call {} has inconsistent immutable correlation",
                record.call_id.get()
            ));
        }
        rows.push(AdmittedEvidenceRow {
            admitted: item,
            call,
        });
    }
    rows.sort_unstable_by(|left, right| {
        right
            .admitted
            .record
            .last_decision_sequence
            .cmp(&left.admitted.record.last_decision_sequence)
            .then_with(|| {
                right
                    .admitted
                    .record
                    .outcome_sequence
                    .cmp(&left.admitted.record.outcome_sequence)
            })
            .then_with(|| {
                right
                    .admitted
                    .record
                    .call_id
                    .get()
                    .cmp(&left.admitted.record.call_id.get())
            })
    });
    Ok(rows)
}

/// One visible row per *currently* admitted result, across all providers.
/// show_rows virtualizes the potentially long inventory; no 16-result cap.
/// Only a single individual revocation intent may leave the renderer.
pub fn render_inventory(
    ui: &mut egui::Ui,
    rows: &[AdmittedEvidenceRow<'_>],
    can_revoke: bool,
) -> Option<ToolCallId> {
    let bytes = rows.iter().fold(0_usize, |total, row| {
        total.saturating_add(row.admitted.text.len())
    });
    ui.label(format!(
        "{} currently admitted MCP result{} · {} raw UTF-8 bytes",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        bytes,
    ));
    ui.label(
        "Across all providers and historical sessions. These results are eligible for the next normal conversation request; specialized controller flows may compose different context. Exact request composition is shown below.",
    );
    if rows.is_empty() {
        ui.label("Nothing admitted. Completed tool calls are excluded by default.");
        return None;
    }
    ui.label("Revoke records an explicit Exclude decision for only that call; it does not erase the journal or change any tool permission.");
    let mut revoke = None;
    egui::ScrollArea::vertical()
        .id_salt("admitted-mcp-context-inventory")
        .max_height(320.0)
        .show_rows(ui, 28.0, rows.len(), |ui, range| {
            for row in &rows[range] {
                let record = &row.admitted.record;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "Call {} · {} · provider {} · {} · {} B · admitted #{}",
                                record.call_id.get(),
                                row.call.operation.as_str(),
                                record.provider_id.get(),
                                record.outcome_kind.stable_name(),
                                row.admitted.text.len(),
                                record.last_decision_sequence,
                            ))
                            .monospace()
                            .size(10.0),
                        )
                        .truncate(),
                    );
                    if ui
                        .add_enabled(
                            can_revoke && revoke.is_none(),
                            egui::Button::new("Revoke"),
                        )
                        .on_hover_text("Explicitly exclude this exact admitted result from future normal conversation context. Its audit history remains.")
                        .clicked()
                    {
                        revoke = Some(record.call_id);
                    }
                });
            }
        });
    ui.label("Only currently admitted results are listed; other completed and revoked calls remain in the Tools / MCP audit.");
    revoke
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::routing::RouteId;
    use chatarium_core::session::SessionId;
    use chatarium_core::tool::{ToolOperationName, ToolProviderId};
    use chatarium_store::tool_outcome_audit::ToolCallOutcomeKind;
    use chatarium_store::tool_result_context_audit::ToolResultContextRecord;

    fn sample(
        owner: LocalConversationId,
        id: u64,
        provider: u64,
        decision: ToolResultContextDecision,
    ) -> (ToolCallAuditRecord, AdmittedToolResult) {
        let call_id = ToolCallId::new(id);
        let route_id = RouteId::new(id + 100);
        let source_session_id = SessionId::new(id + 200);
        let call = ToolCallAuditRecord {
            call_id,
            source_session_id,
            provider_id: ToolProviderId::new(provider),
            operation: ToolOperationName::new("sample.read").unwrap(),
            arguments_text: "{}".to_owned(),
            recorded_sequence: id * 10,
            route_id: Some(route_id),
            route_bound_sequence: Some(id * 10 + 1),
        };
        let record = ToolResultContextRecord {
            call_id,
            route_id,
            provider_id: call.provider_id,
            source_session_id,
            conversation_id: owner,
            outcome_sequence: id * 10 + 3,
            outcome_kind: ToolCallOutcomeKind::Result,
            decision,
            first_decision_sequence: id * 10 + 4,
            last_decision_sequence: id * 10 + 4,
        };
        (
            call,
            AdmittedToolResult {
                record,
                text: format!("raw result for {id}"),
            },
        )
    }

    #[test]
    fn all_providers_and_older_admissions_remain_visible() {
        let owner = LocalConversationId::new();
        let samples = (1..=22)
            .map(|id| sample(owner, id, id % 3 + 1, ToolResultContextDecision::Admit))
            .collect::<Vec<_>>();
        let calls = samples
            .iter()
            .map(|(call, _)| call.clone())
            .collect::<Vec<_>>();
        let admitted = samples
            .into_iter()
            .map(|(_, item)| item)
            .collect::<Vec<_>>();
        let rows = verified_inventory(&admitted, &calls, owner).unwrap();
        assert_eq!(rows.len(), 22);
        assert_eq!(rows[0].admitted.record.call_id.get(), 22);
        assert_eq!(rows[21].admitted.record.call_id.get(), 1);
        assert_eq!(rows[0].admitted.record.provider_id.get(), 2);
        assert_eq!(rows[1].admitted.record.provider_id.get(), 1);
    }

    #[test]
    fn foreign_or_excluded_evidence_blocks_the_whole_inventory() {
        let owner = LocalConversationId::new();
        let other = LocalConversationId::new();
        let (call, result) = sample(other, 1, 7, ToolResultContextDecision::Admit);
        assert!(verified_inventory(&[result], &[call], owner).is_err());
        let (call, result) = sample(owner, 1, 7, ToolResultContextDecision::Exclude);
        assert!(verified_inventory(&[result], &[call], owner).is_err());
    }

    #[test]
    fn duplicates_and_missing_calls_fail_closed() {
        let owner = LocalConversationId::new();
        let (call, result) = sample(owner, 1, 7, ToolResultContextDecision::Admit);
        assert!(verified_inventory(&[result.clone()], &[], owner).is_err());
        assert!(
            verified_inventory(&[result.clone()], &[call.clone(), call.clone()], owner).is_err()
        );
        assert!(verified_inventory(&[result.clone(), result], &[call], owner).is_err());
    }

    #[test]
    fn route_provider_and_session_mismatches_block_revocation_controls() {
        let owner = LocalConversationId::new();
        let (call, result) = sample(owner, 1, 7, ToolResultContextDecision::Admit);
        let mut wrong = call.clone();
        wrong.provider_id = ToolProviderId::new(88);
        assert!(verified_inventory(&[result.clone()], &[wrong], owner).is_err());
        let mut wrong = call.clone();
        wrong.route_id = Some(RouteId::new(88));
        assert!(verified_inventory(&[result.clone()], &[wrong], owner).is_err());
        let mut wrong = call.clone();
        wrong.source_session_id = SessionId::new(88);
        assert!(verified_inventory(&[result.clone()], &[wrong], owner).is_err());
        let mut wrong = call;
        wrong.route_bound_sequence = Some(result.record.outcome_sequence);
        assert!(verified_inventory(&[result], &[wrong], owner).is_err());
    }

    #[test]
    fn impossible_admission_chronology_and_oversize_fail_closed() {
        let owner = LocalConversationId::new();
        let (call, mut result) = sample(owner, 1, 7, ToolResultContextDecision::Admit);
        result.record.first_decision_sequence = result.record.outcome_sequence;
        assert!(verified_inventory(&[result.clone()], &[call.clone()], owner).is_err());
        result.record.first_decision_sequence = result.record.outcome_sequence + 1;
        result.record.last_decision_sequence = result.record.outcome_sequence;
        assert!(verified_inventory(&[result.clone()], &[call.clone()], owner).is_err());
        result.record.last_decision_sequence = result.record.first_decision_sequence;
        result.text = "x".repeat(MAX_CONTEXT_TOOL_RESULT_BYTES + 1);
        assert!(verified_inventory(&[result], &[call], owner).is_err());
    }

    #[test]
    fn latest_decision_orders_re_admitted_evidence_first() {
        let owner = LocalConversationId::new();
        let (first_call, mut first) = sample(owner, 1, 7, ToolResultContextDecision::Admit);
        let (second_call, second) = sample(owner, 2, 8, ToolResultContextDecision::Admit);
        first.record.last_decision_sequence = 99;
        let admitted = [first, second];
        let calls = [first_call, second_call];
        let rows = verified_inventory(&admitted, &calls, owner).unwrap();
        assert_eq!(rows[0].admitted.record.call_id.get(), 1);
        assert_eq!(rows[1].admitted.record.call_id.get(), 2);
    }
}
