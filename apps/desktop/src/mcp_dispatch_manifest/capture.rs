//! Capture admitted-context evidence and attach it to an outbound durable intent.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolEvidence {
    call_id: u64,
    route_id: u64,
    provider_id: u64,
    source_session_id: u64,
    outcome_sequence: u64,
    admitted_sequence: u64,
    outcome_kind: String,
}

fn tool_evidence(source: &ContextSource) -> Option<ToolEvidence> {
    if let ContextSource::ToolResult {
        call_id,
        route_id,
        provider_id,
        source_session_id,
        outcome_sequence,
        admitted_sequence,
        outcome_kind,
    } = source
    {
        Some(ToolEvidence {
            call_id: *call_id,
            route_id: *route_id,
            provider_id: *provider_id,
            source_session_id: *source_session_id,
            outcome_sequence: *outcome_sequence,
            admitted_sequence: *admitted_sequence,
            outcome_kind: outcome_kind.clone(),
        })
    } else {
        None
    }
}

/// Snapshot the frozen eligibility input and the *actual composed* message
/// sources, not a later live admission projection. In special dispatch paths,
/// eligibility is replayed at the execution/coordination start prefix.
pub fn capture(
    conversation_id: LocalConversationId,
    request_class: &'static str,
    frozen_eligible_sources: &[ContextSource],
    plan: &ContextPlan,
) -> Result<Value, String> {
    if !matches!(
        request_class,
        "authored" | "controller_coordination" | "worker_continuation" | "coordination_auto_wake"
    ) {
        return Err("unknown outgoing request class".to_owned());
    }
    let mut eligible = BTreeMap::new();
    for source in frozen_eligible_sources {
        let evidence = tool_evidence(source)
            .ok_or_else(|| "non-tool source supplied as eligible MCP evidence".to_owned())?;
        if evidence.call_id == 0
            || evidence.route_id == 0
            || evidence.provider_id == 0
            || evidence.outcome_sequence == 0
            || evidence.admitted_sequence <= evidence.outcome_sequence
        {
            return Err("tool evidence identity or chronology is invalid".to_owned());
        }
        let call_id = evidence.call_id;
        if eligible.insert(call_id, evidence).is_some() {
            return Err(format!("duplicate frozen admission for call {call_id}"));
        }
    }

    let mut included = BTreeSet::new();
    for message in &plan.messages {
        if let Some(evidence) = tool_evidence(&message.source) {
            if eligible.get(&evidence.call_id) != Some(&evidence) {
                return Err(format!(
                    "composed tool call {} was not in the frozen admitted set",
                    evidence.call_id
                ));
            }
            if !included.insert(evidence.call_id) {
                return Err(format!(
                    "composed tool call {} appears more than once",
                    evidence.call_id
                ));
            }
        }
    }
    // Verify source-level correspondence, not just an aggregate included
    // count. Every tool result named by the composer must match the frozen
    // admission's exact identity, including excluded-by-policy entries.
    let mut inventory_decisions = BTreeMap::new();
    for item in &plan.inventory {
        if let Some(evidence) = tool_evidence(&item.source) {
            if eligible.get(&evidence.call_id) != Some(&evidence) {
                return Err(format!(
                    "composed tool inventory references non-eligible call {}",
                    evidence.call_id
                ));
            }
            if inventory_decisions
                .insert(evidence.call_id, item.decision)
                .is_some()
            {
                return Err(format!(
                    "composed tool inventory repeats call {}",
                    evidence.call_id
                ));
            }
        }
    }
    let inventory_included = inventory_decisions
        .iter()
        .filter(|(_, decision)| **decision == InclusionDecision::Included)
        .map(|(call_id, _)| *call_id)
        .collect::<BTreeSet<_>>();
    if inventory_included != included {
        return Err("composed tool-result inventory differs from outgoing messages".to_owned());
    }

    let eligible_total = eligible.len();
    let included_total = included.len();
    let mut ordered = eligible.into_values().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| {
        right
            .admitted_sequence
            .cmp(&left.admitted_sequence)
            .then_with(|| right.call_id.cmp(&left.call_id))
    });
    let listed = ordered
        .iter()
        .take(MAX_MANIFEST_TOOL_ROWS)
        .map(|item| {
            json!({
                "call_id": item.call_id,
                "route_id": item.route_id,
                "provider_id": item.provider_id,
                "source_session_id": item.source_session_id,
                "outcome_sequence": item.outcome_sequence,
                "admitted_sequence": item.admitted_sequence,
                "outcome_kind": item.outcome_kind,
                "disposition": if included.contains(&item.call_id) { "included" } else { "omitted" },
                "composition_reason": match inventory_decisions.get(&item.call_id) {
                    Some(InclusionDecision::Included) => "included",
                    Some(InclusionDecision::ExcludedByPolicy) => "excluded_by_policy",
                    Some(InclusionDecision::ExcludedByBudget) => "excluded_by_budget",
                    Some(InclusionDecision::OmittedEmpty) => "omitted_empty",
                    None => "not_selected_for_dispatch",
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema": SCHEMA,
        "version": VERSION,
        "conversation_id": conversation_id.to_string(),
        "request_class": request_class,
        "eligible_tool_results": eligible_total,
        "included_tool_results": included_total,
        "omitted_tool_results": eligible_total - included_total,
        "listed_tool_results": listed,
        "list_truncated": eligible_total > MAX_MANIFEST_TOOL_ROWS,
        "context_included_items": plan.size.included_items,
        "context_omitted_items": plan.size.omitted_items,
        "context_included_utf8_bytes": plan.size.utf8_bytes,
    }))
}

/// Embed the manifest in the *same* durable DispatchAttempted observation as
/// the request intent. The original schema, kind, scope and turn correlation
/// remain unchanged for older recovery and transport consumers.
pub fn attach_to_dispatch_payload(payload: String, manifest: Value) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(&payload)
        .map_err(|error| format!("dispatch payload is not JSON: {error}"))?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-responses-turn-observation")
        || value.get("version").and_then(Value::as_u64) != Some(1)
        || value.pointer("/details/context_evidence").is_some()
    {
        return Err("unexpected dispatch observation envelope".to_owned());
    }
    value["details"]["context_evidence"] = manifest;
    serde_json::to_string(&value).map_err(|error| error.to_string())
}
