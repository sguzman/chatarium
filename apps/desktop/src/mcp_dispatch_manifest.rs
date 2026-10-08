//! Bounded, content-free evidence manifest for each prepared Responses dispatch.
//!
//! Recording admission is distinct from inclusion in a composed request.
//! DispatchAttempted is an intent boundary, not proof that the remote accepted
//! the request. This module never grants context or tool execution authority.

use crate::context_composer::{ContextPlan, ContextSource, InclusionDecision};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_store::authored::{DecodedUserMessageCommit, decode_user_message_commit};
use chatarium_store::continuation_execution_audit::replay_worker_continuation_execution_audit;
use chatarium_store::controller_coordination_audit::replay_controller_coordination_audit;
use chatarium_store::EventEnvelope;
use eframe::egui;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MANIFEST_TOOL_ROWS: usize = 32;
const MAX_VISIBLE_DISPATCHES: usize = 12;
const SCHEMA: &str = "chatarium-dispatch-context-evidence";
const VERSION: u64 = 1;

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
        "authored" | "controller_coordination" | "worker_continuation"
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
    let inventory_included = plan
        .inventory
        .iter()
        .filter(|item| {
            item.decision == InclusionDecision::Included
                && matches!(item.source, ContextSource::ToolResult { .. })
        })
        .count();
    if inventory_included != included.len() {
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchTransportTrace {
    pub accepted_sequence: Option<u64>,
    pub failure_sequence: Option<u64>,
    pub completed_sequence: Option<u64>,
    pub interrupted_sequence: Option<u64>,
    pub first_output_sequence: Option<u64>,
    pub last_observation_sequence: Option<u64>,
}

impl DispatchTransportTrace {
    fn observe(&mut self, kind: EventKind, sequence: u64) {
        match kind {
            EventKind::RemoteAcceptanceObserved => {
                self.accepted_sequence.get_or_insert(sequence);
            }
            EventKind::RemoteFailureObserved => {
                self.failure_sequence.get_or_insert(sequence);
            }
            EventKind::AssistantCompletionObserved => {
                self.completed_sequence.get_or_insert(sequence);
            }
            EventKind::TransportInterrupted => {
                self.interrupted_sequence.get_or_insert(sequence);
            }
            EventKind::AssistantStreamStarted
            | EventKind::AssistantDeltaObserved
            | EventKind::AssistantSnapshotObserved => {
                self.first_output_sequence.get_or_insert(sequence);
            }
            _ => return,
        }
        self.last_observation_sequence = Some(sequence);
    }

    pub fn status(&self) -> &'static str {
        match (
            self.completed_sequence.is_some(),
            self.failure_sequence.is_some(),
            self.interrupted_sequence.is_some(),
            self.accepted_sequence.is_some(),
            self.first_output_sequence.is_some(),
        ) {
            (true, true, _, _, _) => "CONFLICTING TERMINAL EVIDENCE · inspect turn audit",
            (true, false, true, _, _) => "COMPLETION OBSERVED AFTER INTERRUPTION",
            (true, false, false, _, _) => "COMPLETION OBSERVED",
            (false, true, _, _, _) => "REMOTE FAILURE OBSERVED",
            (false, false, true, true, _) => "INTERRUPTED AFTER REMOTE ACCEPTANCE",
            (false, false, true, false, _) => "TRANSPORT INTERRUPTED · OUTCOME UNKNOWN",
            (false, false, false, true, _) => "REMOTE ACCEPTED · COMPLETION NOT OBSERVED",
            (false, false, false, false, true) => "OUTPUT OBSERVED · ACCEPTANCE NOT RECORDED",
            _ => "DISPATCH ATTEMPT RECORDED · OUTCOME NOT OBSERVED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedDispatchManifest {
    pub sequence: u64,
    pub turn_id: String,
    pub request_class: String,
    pub conversation_id: String,
    pub eligible_total: usize,
    pub included_total: usize,
    pub omitted_total: usize,
    pub included_items: usize,
    pub omitted_items: usize,
    pub included_bytes: usize,
    pub listed: Vec<(u64, u64, String)>,
    pub truncated: bool,
    pub transport: DispatchTransportTrace,
}

/// Reject malformed *manifested* dispatches rather than guessing. Old
/// DispatchAttempted events without the new field remain valid legacy history
/// and are not misrepresented as having captured outgoing evidence.
fn parse_dispatch(event: &EventEnvelope) -> Result<Option<RecordedDispatchManifest>, String> {
    if event.kind != EventKind::DispatchAttempted {
        return Ok(None);
    }
    let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
        return Ok(None);
    };
    let Some(manifest) = payload.pointer("/details/context_evidence") else {
        return Ok(None);
    };
    let error = || {
        format!(
            "invalid dispatch context manifest at event #{}",
            event.sequence
        )
    };
    if payload.get("schema").and_then(Value::as_str) != Some("chatarium-responses-turn-observation")
        || payload.get("version").and_then(Value::as_u64) != Some(1)
        || manifest.get("schema").and_then(Value::as_str) != Some(SCHEMA)
        || manifest.get("version").and_then(Value::as_u64) != Some(VERSION)
    {
        return Err(error());
    }
    let turn_id = payload
        .pointer("/details/local_turn_id")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(error)?;
    let request_id = payload
        .pointer("/details/request_id")
        .and_then(Value::as_str)
        .ok_or_else(error)?;
    if request_id != turn_id
        || event.scope.as_deref() != Some(format!("local-turn:{turn_id}").as_str())
    {
        return Err(error());
    }
    let conversation_id = manifest
        .get("conversation_id")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(error)?;
    let request_class = manifest
        .get("request_class")
        .and_then(Value::as_str)
        .filter(|class| {
            matches!(
                *class,
                "authored" | "controller_coordination" | "worker_continuation"
            )
        })
        .ok_or_else(error)?;
    let count = |key: &str| -> Result<usize, String> {
        manifest
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|number| usize::try_from(number).ok())
            .ok_or_else(error)
    };
    let eligible_total = count("eligible_tool_results")?;
    let included_total = count("included_tool_results")?;
    let omitted_total = count("omitted_tool_results")?;
    if included_total.checked_add(omitted_total) != Some(eligible_total) {
        return Err(error());
    }
    let listed = manifest
        .get("listed_tool_results")
        .and_then(Value::as_array)
        .ok_or_else(error)?;
    let truncated = manifest
        .get("list_truncated")
        .and_then(Value::as_bool)
        .ok_or_else(error)?;
    if listed.len() != eligible_total.min(MAX_MANIFEST_TOOL_ROWS)
        || truncated != (eligible_total > MAX_MANIFEST_TOOL_ROWS)
    {
        return Err(error());
    }
    let mut seen = BTreeSet::new();
    let mut result_rows = Vec::with_capacity(listed.len());
    let mut listed_included = 0;
    for item in listed {
        let number = |key| {
            item.get(key)
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .ok_or_else(error)
        };
        let call_id = number("call_id")?;
        let provider_id = number("provider_id")?;
        let route_id = number("route_id")?;
        let _session = number("source_session_id")?;
        let outcome_seq = number("outcome_sequence")?;
        let admission_seq = number("admitted_sequence")?;
        let kind = item
            .get("outcome_kind")
            .and_then(Value::as_str)
            .ok_or_else(error)?;
        let disposition = item
            .get("disposition")
            .and_then(Value::as_str)
            .ok_or_else(error)?;
        if !seen.insert(call_id)
            || admission_seq <= outcome_seq
            || kind.is_empty()
            || !matches!(disposition, "included" | "omitted")
        {
            return Err(error());
        }
        if disposition == "included" {
            listed_included += 1;
        }
        result_rows.push((
            call_id,
            provider_id,
            format!("route {route_id} · {kind} · {disposition} · admission #{admission_seq}"),
        ));
    }
    if listed_included > included_total
        || (eligible_total == listed.len() && listed_included != included_total)
    {
        return Err(error());
    }
    Ok(Some(RecordedDispatchManifest {
        sequence: event.sequence,
        turn_id: turn_id.to_owned(),
        request_class: request_class.to_owned(),
        conversation_id: conversation_id.to_owned(),
        eligible_total,
        included_total,
        omitted_total,
        included_items: count("context_included_items")?,
        omitted_items: count("context_omitted_items")?,
        included_bytes: count("context_included_utf8_bytes")?,
        listed: result_rows,
        truncated,
        transport: DispatchTransportTrace::default(),
    }))
}


fn is_transport_evidence(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::RemoteAcceptanceObserved
            | EventKind::RemoteFailureObserved
            | EventKind::AssistantStreamStarted
            | EventKind::AssistantDeltaObserved
            | EventKind::AssistantSnapshotObserved
            | EventKind::AssistantCompletionObserved
            | EventKind::TransportInterrupted
    )
}

/// Only typed, turn-correlated observations can establish remote outcome
/// evidence. An unrelated event with the same scope cannot spoof acceptance.
fn validate_transport_observation(
    event: &EventEnvelope,
    recorded: &RecordedDispatchManifest,
) -> Result<(), String> {
    let invalid = || {
        format!(
            "transport observation at event #{} does not match manifested turn {}",
            event.sequence, recorded.turn_id
        )
    };
    let value: Value = serde_json::from_str(&event.payload).map_err(|_| invalid())?;
    if value.get("schema").and_then(Value::as_str)
        != Some("chatarium-responses-turn-observation")
        || value.get("version").and_then(Value::as_u64) != Some(1)
        || value.pointer("/details/local_turn_id").and_then(Value::as_str)
            != Some(recorded.turn_id.as_str())
        || value.pointer("/details/request_id").and_then(Value::as_str)
            != Some(recorded.turn_id.as_str())
    {
        return Err(invalid());
    }
    Ok(())
}

/// Independently link each manifest's asserted conversation to its durable
/// originating authored commit or typed non-authored orchestration start.
/// No manifest may borrow another conversation's identity.
fn verify_origins(
    events: &[EventEnvelope],
    rows: &[RecordedDispatchManifest],
) -> Result<(), String> {
    let mut authored = BTreeMap::new();
    for event in events {
        if let Some(DecodedUserMessageCommit::Typed(message)) =
            decode_user_message_commit(event)?
        {
            let id = message.turn_id.to_string();
            if event.scope.as_deref() != Some(format!("local-turn:{id}").as_str())
                || authored
                    .insert(id.clone(), (message.conversation_id.to_string(), event.sequence))
                    .is_some()
            {
                return Err(format!("conflicting typed authored origin for turn {id}"));
            }
        }
    }
    let require_coordination = rows
        .iter()
        .any(|row| row.request_class == "controller_coordination");
    let require_continuation = rows
        .iter()
        .any(|row| row.request_class == "worker_continuation");
    let coordination = if require_coordination {
        replay_controller_coordination_audit(events)?
            .into_iter()
            .map(|item| {
                (
                    item.coordination_turn_id.to_string(),
                    (
                        item.controller_conversation_id.to_string(),
                        item.started_sequence,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    let continuation = if require_continuation {
        replay_worker_continuation_execution_audit(events)?
            .into_iter()
            .map(|item| {
                (
                    item.execution_turn_id.to_string(),
                    (
                        item.worker_conversation_id.to_string(),
                        item.started_sequence,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        BTreeMap::new()
    };
    for row in rows {
        let origin = match row.request_class.as_str() {
            "authored" => authored.get(&row.turn_id),
            "controller_coordination" => coordination.get(&row.turn_id),
            "worker_continuation" => continuation.get(&row.turn_id),
            _ => None,
        };
        match origin {
            Some((owner, started)) if owner == &row.conversation_id && *started < row.sequence => {}
            _ => {
                return Err(format!(
                    "dispatch event #{} has no matching historically owned {} origin",
                    row.sequence, row.request_class
                ));
            }
        }
    }
    Ok(())
}

/// Linear chronological audit. Earlier implementation rescanned the entire
/// journal for every visible dispatch; this records each exact-turn transport
/// observation once, with stable immutable event sequence identifiers.
pub fn dispatch_history(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<RecordedDispatchManifest>, String> {
    let mut results = Vec::new();
    let mut turn_to_row = BTreeMap::<String, usize>::new();
    let mut last_sequence = None;
    for event in events {
        if last_sequence.is_some_and(|previous| event.sequence <= previous) {
            return Err(format!("journal sequence is not increasing at #{}", event.sequence));
        }
        last_sequence = Some(event.sequence);
        if event.kind == EventKind::DispatchAttempted {
            if let Some(manifest) = parse_dispatch(event)? {
                if turn_to_row
                    .insert(manifest.turn_id.clone(), results.len())
                    .is_some()
                {
                    return Err(format!(
                        "duplicate manifested dispatch for turn {}",
                        manifest.turn_id
                    ));
                }
                results.push(manifest);
            }
        } else if is_transport_evidence(event.kind) {
            if let Some(scope) = event.scope.as_deref() {
                if let Some(turn_id) = scope.strip_prefix("local-turn:") {
                    if let Some(index) = turn_to_row.get(turn_id).copied() {
                        let recorded = &mut results[index];
                        validate_transport_observation(event, recorded)?;
                        recorded.transport.observe(event.kind, event.sequence);
                    }
                }
            }
        }
    }
    verify_origins(events, &results)?;
    results.retain(|row| row.conversation_id == conversation_id.to_string());
    results.reverse();
    Ok(results)
}

/// Visible history of what Chatarium assembled before attempting transport.
/// This cannot assert that a server received, retained or used a request.
pub fn render_dispatches(
    ui: &mut egui::Ui,
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) {
    match dispatch_history(events, conversation_id) {
        Ok(rows) if rows.is_empty() => {
            ui.label("No manifested dispatch attempts for this local conversation. Older attempts did not record this metadata.");
        }
        Ok(rows) => {
            ui.label("Journaled request-preparation evidence, not proof of remote receipt or model attention. Transport outcomes are correlated by exact turn, request ID and durable sequence.");
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
                        ui.label(format!(
                            "Transport audit · accepted {:?} · completed {:?} · failed {:?} · interrupted {:?} · first output {:?}",
                            item.transport.accepted_sequence,
                            item.transport.completed_sequence,
                            item.transport.failure_sequence,
                            item.transport.interrupted_sequence,
                            item.transport.first_output_sequence,
                        ));
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
                        for (call_id, provider_id, summary) in &item.listed {
                            ui.label(
                                egui::RichText::new(format!(
                                    "call {call_id} · provider {provider_id} · {summary}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_composer::{ContextPolicy, TranscriptMessage};
    use chatarium_core::{EventKind, LocalConversationId};

    fn admitted_source(call_id: u64) -> ContextSource {
        ContextSource::ToolResult {
            call_id,
            route_id: call_id + 20,
            provider_id: 3,
            source_session_id: 4,
            outcome_sequence: call_id + 100,
            admitted_sequence: call_id + 200,
            outcome_kind: "result".to_owned(),
        }
    }

    fn plan_with_tool(source: &ContextSource) -> ContextPlan {
        let ContextSource::ToolResult {
            call_id,
            route_id,
            provider_id,
            source_session_id,
            outcome_sequence,
            admitted_sequence,
            outcome_kind,
        } = source
        else {
            panic!("test requires tool result");
        };
        ContextPlan::compose(
            ContextPolicy::dispatch(),
            "",
            "",
            [TranscriptMessage::tool_result(
                "secret adapter bytes",
                *call_id,
                *route_id,
                *provider_id,
                *source_session_id,
                *outcome_sequence,
                *admitted_sequence,
                outcome_kind.as_str(),
            )],
        )
    }

    #[test]
    fn captures_only_actual_composed_tool_sources_without_raw_content() {
        let owner = LocalConversationId::new();
        let source = admitted_source(11);
        let plan = plan_with_tool(&source);
        let manifest = capture(owner, "authored", &[source], &plan).unwrap();
        assert_eq!(manifest["eligible_tool_results"], 1);
        assert_eq!(manifest["included_tool_results"], 1);
        assert_eq!(
            manifest["listed_tool_results"][0]["disposition"],
            "included"
        );
        assert_eq!(manifest["context_included_items"], 3 - 2); // one tool; no instructions or developer text
        assert!(!manifest.to_string().contains("secret adapter bytes"));
    }

    #[test]
    fn specialized_dispatch_reports_admitted_but_not_included() {
        let owner = LocalConversationId::new();
        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", []);
        let manifest =
            capture(owner, "worker_continuation", &[admitted_source(12)], &plan).unwrap();
        assert_eq!(manifest["eligible_tool_results"], 1);
        assert_eq!(manifest["included_tool_results"], 0);
        assert_eq!(manifest["omitted_tool_results"], 1);
        assert_eq!(manifest["listed_tool_results"][0]["disposition"], "omitted");
    }

    #[test]
    fn unsupported_or_duplicated_evidence_is_rejected() {
        let owner = LocalConversationId::new();
        let source = admitted_source(7);
        let plan = plan_with_tool(&source);
        assert!(capture(owner, "authored", &[], &plan).is_err());
        assert!(capture(owner, "authored", &[source.clone(), source.clone()], &plan).is_err());
        assert!(capture(owner, "arbitrary", &[source], &plan).is_err());
    }

    #[test]
    fn bounded_manifest_discloses_incomplete_details_not_incomplete_totals() {
        let owner = LocalConversationId::new();
        let sources = (1..=40).map(admitted_source).collect::<Vec<_>>();
        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", []);
        let manifest = capture(owner, "controller_coordination", &sources, &plan).unwrap();
        assert_eq!(manifest["eligible_tool_results"], 40);
        assert_eq!(manifest["omitted_tool_results"], 40);
        assert_eq!(
            manifest["listed_tool_results"].as_array().unwrap().len(),
            32
        );
        assert_eq!(manifest["list_truncated"], true);
    }

    #[test]
    fn durable_envelope_replays_and_detects_scope_or_count_corruption() {
        let owner = LocalConversationId::new();
        let turn = "turn-test";
        let manifest = capture(
            owner,
            "authored",
            &[admitted_source(1)],
            &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
        )
        .unwrap();
        let payload = json!({
            "schema":"chatarium-responses-turn-observation",
            "version":1,
            "details":{"local_turn_id":turn,"request_id":turn}
        })
        .to_string();
        let payload = attach_to_dispatch_payload(payload, manifest).unwrap();
        let mut event = EventEnvelope {
            sequence: 9,
            at_unix_ms: 0,
            scope: Some("local-turn:turn-test".to_owned()),
            kind: EventKind::DispatchAttempted,
            payload,
        };
        let parsed = parse_dispatch(&event).unwrap().unwrap();
        assert_eq!(parsed.eligible_total, 1);
        assert_eq!(parsed.included_total, 0);
        assert_eq!(parsed.omitted_total, 1);
        event.scope = Some("local-turn:wrong".to_owned());
        assert!(parse_dispatch(&event).is_err());
        event.scope = Some("local-turn:turn-test".to_owned());
        let mut corrupt: Value = serde_json::from_str(&event.payload).unwrap();
        corrupt["details"]["context_evidence"]["included_tool_results"] = json!(2);
        event.payload = corrupt.to_string();
        assert!(parse_dispatch(&event).is_err());
    }

    fn append_authored_attempt(
        store: &mut chatarium_store::MemoryEventStore,
        owner: LocalConversationId,
    ) -> (String, u64) {
        use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
        use chatarium_store::authored::{commit_user_message, local_turn_scope};
        use chatarium_store::EventStore;

        let turn = LocalTurnId::new();
        let message = AuthoredUserMessage::new(
            owner,
            turn,
            LocalMessageId::new(),
            "historically typed user message",
        );
        commit_user_message(store, &message).unwrap();
        let manifest = capture(
            owner,
            "authored",
            &[],
            &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
        )
        .unwrap();
        let turn_id = turn.to_string();
        let payload = json!({
            "schema": "chatarium-responses-turn-observation",
            "version": 1,
            "details": {
                "local_turn_id": turn_id,
                "request_id": turn_id,
            },
        })
        .to_string();
        let payload = attach_to_dispatch_payload(payload, manifest).unwrap();
        let sequence = store
            .append_scoped(
                Some(local_turn_scope(turn)),
                EventKind::DispatchAttempted,
                payload,
            )
            .unwrap();
        (turn_id, sequence)
    }

    fn append_transport_observation(
        store: &mut chatarium_store::MemoryEventStore,
        turn: &str,
        request: &str,
        kind: EventKind,
    ) -> u64 {
        use chatarium_store::EventStore;
        store
            .append_scoped(
                Some(format!("local-turn:{turn}")),
                kind,
                json!({
                    "schema": "chatarium-responses-turn-observation",
                    "version": 1,
                    "details": {
                        "local_turn_id": turn,
                        "request_id": request,
                    },
                })
                .to_string(),
            )
            .unwrap()
    }

    #[test]
    fn exact_turn_outcomes_replay_without_cross_conversation_leakage() {
        use chatarium_store::{EventStore, MemoryEventStore};

        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let (first_turn, first_dispatch) = append_authored_attempt(&mut store, first);
        let (second_turn, second_dispatch) = append_authored_attempt(&mut store, second);
        let accepted = append_transport_observation(
            &mut store,
            &first_turn,
            &first_turn,
            EventKind::RemoteAcceptanceObserved,
        );
        let completed = append_transport_observation(
            &mut store,
            &first_turn,
            &first_turn,
            EventKind::AssistantCompletionObserved,
        );
        assert!(completed > accepted && accepted > first_dispatch);
        assert!(second_dispatch > first_dispatch);
        let first_rows = dispatch_history(store.events(), first).unwrap();
        assert_eq!(first_rows.len(), 1);
        assert_eq!(first_rows[0].turn_id, first_turn);
        assert_eq!(first_rows[0].transport.accepted_sequence, Some(accepted));
        assert_eq!(first_rows[0].transport.completed_sequence, Some(completed));
        assert_eq!(first_rows[0].transport.status(), "COMPLETION OBSERVED");
        let second_rows = dispatch_history(store.events(), second).unwrap();
        assert_eq!(second_rows.len(), 1);
        assert_eq!(second_rows[0].turn_id, second_turn);
        assert_eq!(second_rows[0].transport.accepted_sequence, None);
        assert_eq!(
            second_rows[0].transport.status(),
            "DISPATCH ATTEMPT RECORDED · OUTCOME NOT OBSERVED",
        );
    }

    #[test]
    fn foreign_request_id_in_same_turn_scope_blocks_projection() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let (turn, _) = append_authored_attempt(&mut store, owner);
        append_transport_observation(
            &mut store,
            &turn,
            "a-different-request",
            EventKind::RemoteAcceptanceObserved,
        );
        assert!(dispatch_history(store.events(), owner).is_err());
    }

    #[test]
    fn manifest_owner_must_match_durable_typed_turn_owner() {
        use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
        use chatarium_store::authored::{commit_user_message, local_turn_scope};
        use chatarium_store::{EventStore, MemoryEventStore};

        let own = LocalConversationId::new();
        let forged = LocalConversationId::new();
        let turn = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        commit_user_message(
            &mut store,
            &AuthoredUserMessage::new(own, turn, LocalMessageId::new(), "owned"),
        )
        .unwrap();
        let manifest = capture(
            forged,
            "authored",
            &[],
            &ContextPlan::compose(ContextPolicy::dispatch(), "", "", []),
        )
        .unwrap();
        let payload = attach_to_dispatch_payload(
            json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "details": {
                    "local_turn_id": turn.to_string(),
                    "request_id": turn.to_string(),
                },
            })
            .to_string(),
            manifest,
        )
        .unwrap();
        store
            .append_scoped(
                Some(local_turn_scope(turn)),
                EventKind::DispatchAttempted,
                payload,
            )
            .unwrap();
        assert!(dispatch_history(store.events(), own).is_err());
        assert!(dispatch_history(store.events(), forged).is_err());
    }

    #[test]
    fn history_preserves_more_than_twelve_attempts_newest_first() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let mut last = String::new();
        for _ in 0..27 {
            let (turn, _) = append_authored_attempt(&mut store, owner);
            last = turn;
        }
        let rows = dispatch_history(store.events(), owner).unwrap();
        assert_eq!(rows.len(), 27);
        assert_eq!(rows[0].turn_id, last);
        assert!(rows.windows(2).all(|pair| pair[0].sequence > pair[1].sequence));
    }

    #[test]
    fn transport_interruption_does_not_erase_positive_acceptance_evidence() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let (turn, _) = append_authored_attempt(&mut store, owner);
        let accepted = append_transport_observation(
            &mut store,
            &turn,
            &turn,
            EventKind::RemoteAcceptanceObserved,
        );
        let interrupted = append_transport_observation(
            &mut store,
            &turn,
            &turn,
            EventKind::TransportInterrupted,
        );
        let rows = dispatch_history(store.events(), owner).unwrap();
        assert_eq!(rows[0].transport.accepted_sequence, Some(accepted));
        assert_eq!(rows[0].transport.interrupted_sequence, Some(interrupted));
        assert_eq!(
            rows[0].transport.status(),
            "INTERRUPTED AFTER REMOTE ACCEPTANCE"
        );
    }

    #[test]
    fn conflicting_terminal_observations_are_labeled_not_misreported() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let (turn, _) = append_authored_attempt(&mut store, owner);
        append_transport_observation(
            &mut store,
            &turn,
            &turn,
            EventKind::RemoteFailureObserved,
        );
        append_transport_observation(
            &mut store,
            &turn,
            &turn,
            EventKind::AssistantCompletionObserved,
        );
        let rows = dispatch_history(store.events(), owner).unwrap();
        assert_eq!(
            rows[0].transport.status(),
            "CONFLICTING TERMINAL EVIDENCE · inspect turn audit"
        );
    }

    #[test]
    fn legacy_dispatch_does_not_fabricate_manifest() {
        let owner = LocalConversationId::new();
        let event = EventEnvelope {
            sequence: 1,
            at_unix_ms: 0,
            scope: Some("local-turn:legacy".to_owned()),
            kind: EventKind::DispatchAttempted,
            payload: "{}".to_owned(),
        };
        assert!(dispatch_history(&[event], owner).unwrap().is_empty());
    }
}
