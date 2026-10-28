//! Validate and reconstruct the exact durable dispatch history.
use super::*;

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

    /// Stable, chronological transport evidence with exact journal sequences.
    /// These are observed event kinds, not proofs of remote model attention.
    pub fn timeline(&self) -> Vec<(u64, &'static str)> {
        let mut evidence = Vec::new();
        for (sequence, label) in [
            (self.accepted_sequence, "remote acceptance observed"),
            (self.first_output_sequence, "assistant output observed"),
            (self.failure_sequence, "remote failure observed"),
            (self.completed_sequence, "assistant completion observed"),
            (self.interrupted_sequence, "transport interrupted"),
        ] {
            if let Some(sequence) = sequence {
                evidence.push((sequence, label));
            }
        }
        evidence.sort_unstable_by_key(|(sequence, _)| *sequence);
        evidence
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

/// Optional per-result composition reason. Older v1 manifests carry only
/// included/omitted and are never assigned a guessed historical reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCompositionReason {
    Included,
    ExcludedByPolicy,
    ExcludedByBudget,
    OmittedEmpty,
    NotSelectedForDispatch,
}

impl ToolCompositionReason {
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::ExcludedByPolicy => "excluded_by_policy",
            Self::ExcludedByBudget => "excluded_by_budget",
            Self::OmittedEmpty => "omitted_empty",
            Self::NotSelectedForDispatch => "not_selected_for_dispatch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "included" => Some(Self::Included),
            "excluded_by_policy" => Some(Self::ExcludedByPolicy),
            "excluded_by_budget" => Some(Self::ExcludedByBudget),
            "omitted_empty" => Some(Self::OmittedEmpty),
            "not_selected_for_dispatch" => Some(Self::NotSelectedForDispatch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedToolEvidence {
    pub call_id: u64,
    pub route_id: u64,
    pub provider_id: u64,
    pub source_session_id: u64,
    pub outcome_sequence: u64,
    pub admitted_sequence: u64,
    pub outcome_kind: String,
    pub included: bool,
    pub composition_reason: Option<ToolCompositionReason>,
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
    pub listed: Vec<ListedToolEvidence>,
    pub truncated: bool,
    pub transport: DispatchTransportTrace,
}

/// Reject malformed *manifested* dispatches rather than guessing. Old
/// DispatchAttempted events without the new field remain valid legacy history
/// and are not misrepresented as having captured outgoing evidence.
pub(super) fn parse_dispatch(
    event: &EventEnvelope,
) -> Result<Option<RecordedDispatchManifest>, String> {
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
                "authored"
                    | "controller_coordination"
                    | "worker_continuation"
                    | "coordination_auto_wake"
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
        let session_id = number("source_session_id")?;
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
        let composition_reason = match item.get("composition_reason") {
            None => None,
            Some(Value::String(value)) => {
                let parsed = ToolCompositionReason::parse(value).ok_or_else(error)?;
                if (disposition == "included") != (parsed == ToolCompositionReason::Included) {
                    return Err(error());
                }
                Some(parsed)
            }
            _ => return Err(error()),
        };
        result_rows.push(ListedToolEvidence {
            call_id,
            route_id,
            provider_id,
            source_session_id: session_id,
            outcome_sequence: outcome_seq,
            admitted_sequence: admission_seq,
            outcome_kind: kind.to_owned(),
            included: disposition == "included",
            composition_reason,
        });
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
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-responses-turn-observation")
        || value.get("version").and_then(Value::as_u64) != Some(1)
        || value
            .pointer("/details/local_turn_id")
            .and_then(Value::as_str)
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
        if let Some(DecodedUserMessageCommit::Typed(message)) = decode_user_message_commit(event)? {
            let id = message.turn_id.to_string();
            if event.scope.as_deref() != Some(format!("local-turn:{id}").as_str())
                || authored
                    .insert(
                        id.clone(),
                        (message.conversation_id.to_string(), event.sequence),
                    )
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
            return Err(format!(
                "journal sequence is not increasing at #{}",
                event.sequence
            ));
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

/// Portable forensic record for a single historical request. Only
/// provenance and size metadata are copied; no prompt, message, or tool body.
pub fn portable_audit_json(item: &RecordedDispatchManifest) -> Value {
    json!({
        "schema": "chatarium-request-context-audit-export",
        "version": 1,
        "request": {
            "conversation_id": item.conversation_id,
            "turn_id": item.turn_id,
            "class": item.request_class,
            "dispatch_attempt_sequence": item.sequence,
        },
        "transport": {
            "status": item.transport.status(),
            "accepted_sequence": item.transport.accepted_sequence,
            "first_output_sequence": item.transport.first_output_sequence,
            "failure_sequence": item.transport.failure_sequence,
            "completed_sequence": item.transport.completed_sequence,
            "interrupted_sequence": item.transport.interrupted_sequence,
            "last_observation_sequence": item.transport.last_observation_sequence,
        },
        "context": {
            "included_items": item.included_items,
            "omitted_items": item.omitted_items,
            "included_utf8_bytes": item.included_bytes,
        },
        "tool_results": {
            "eligible": item.eligible_total,
            "included": item.included_total,
            "omitted": item.omitted_total,
            "list_truncated": item.truncated,
            "listed": item.listed.iter().map(|row| json!({
                "call_id": row.call_id,
                "route_id": row.route_id,
                "provider_id": row.provider_id,
                "source_session_id": row.source_session_id,
                "outcome_sequence": row.outcome_sequence,
                "admitted_sequence": row.admitted_sequence,
                "outcome_kind": row.outcome_kind,
                "disposition": if row.included { "included" } else { "omitted" },
                "composition_reason": row.composition_reason.map(ToolCompositionReason::stable_name),
            })).collect::<Vec<_>>(),
        },
    })
}
