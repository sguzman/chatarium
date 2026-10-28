//! Reverse-map the provenance of admitted MCP results across requests.
use super::*;

/// Exact per-request inference evidence for a single historical tool result.
/// When a manifest's detail list is truncated, absence is UNKNOWN, not proof
/// that a result was omitted from the composed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProvenanceDisposition {
    Included,
    AdmittedButOmitted,
    UnknownBecauseManifestTruncated,
}

impl ToolProvenanceDisposition {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Included => "INCLUDED in composed request",
            Self::AdmittedButOmitted => "ADMITTED but OMITTED from request",
            Self::UnknownBecauseManifestTruncated => "UNKNOWN · manifest details truncated",
        }
    }
}

pub struct ToolProvenanceEntry<'a> {
    pub dispatch: &'a RecordedDispatchManifest,
    pub disposition: ToolProvenanceDisposition,
}

pub struct ToolProvenanceReport<'a> {
    pub call_id: u64,
    pub included: usize,
    pub admitted_but_omitted: usize,
    pub uncertain_truncated: usize,
    pub not_eligible_in_complete_manifests: usize,
    pub identity: Option<&'a ListedToolEvidence>,
    /// Newest dispatches first; only direct evidence or uncertain truncated
    /// coverage is listed. Full negative coverage is counted, not guessed.
    pub entries: Vec<ToolProvenanceEntry<'a>>,
}

/// Pure reverse lookup across the historically replayed conversation, without
/// consulting today's live admission state. The same immutable call must
/// retain provider, route, source session and terminal outcome identity.
pub fn reverse_tool_provenance(
    history: &[RecordedDispatchManifest],
    call_id: u64,
) -> Result<ToolProvenanceReport<'_>, String> {
    if call_id == 0 {
        return Err("tool call ID must be a nonzero integer".to_owned());
    }
    let mut included = 0;
    let mut admitted_but_omitted = 0;
    let mut uncertain_truncated = 0;
    let mut not_eligible_in_complete_manifests = 0;
    let mut identity: Option<&ListedToolEvidence> = None;
    let mut entries = Vec::new();
    let mut owner: Option<&str> = None;
    for dispatch in history {
        if let Some(existing) = owner {
            if existing != dispatch.conversation_id {
                return Err("reverse lookup input mixes local conversations".to_owned());
            }
        } else {
            owner = Some(dispatch.conversation_id.as_str());
        }
        if let Some(evidence) = dispatch.listed.iter().find(|row| row.call_id == call_id) {
            if let Some(existing) = identity {
                if existing.route_id != evidence.route_id
                    || existing.provider_id != evidence.provider_id
                    || existing.source_session_id != evidence.source_session_id
                    || existing.outcome_sequence != evidence.outcome_sequence
                    || existing.outcome_kind != evidence.outcome_kind
                {
                    return Err(format!(
                        "tool call {call_id} has contradictory immutable identity across dispatches"
                    ));
                }
            } else {
                identity = Some(evidence);
            }
            let disposition = if evidence.included {
                included += 1;
                ToolProvenanceDisposition::Included
            } else {
                admitted_but_omitted += 1;
                ToolProvenanceDisposition::AdmittedButOmitted
            };
            entries.push(ToolProvenanceEntry {
                dispatch,
                disposition,
            });
        } else if dispatch.truncated {
            uncertain_truncated += 1;
            entries.push(ToolProvenanceEntry {
                dispatch,
                disposition: ToolProvenanceDisposition::UnknownBecauseManifestTruncated,
            });
        } else {
            not_eligible_in_complete_manifests += 1;
        }
    }
    Ok(ToolProvenanceReport {
        call_id,
        included,
        admitted_but_omitted,
        uncertain_truncated,
        not_eligible_in_complete_manifests,
        identity,
        entries,
    })
}

/// A portable and content-free reverse provenance report. Entries include
/// every positive or uncertain match; nothing from prompts or tool bodies.
/// Missing pre-manifest history cannot be retroactively reconstructed.
pub fn reverse_tool_provenance_json(report: &ToolProvenanceReport<'_>) -> Value {
    json!({
        "schema": "chatarium-tool-reverse-provenance-export",
        "version": 1,
        "call_id": report.call_id,
        "coverage": "manifested dispatches for the selected conversation only",
        "counts": {
            "included": report.included,
            "admitted_but_omitted": report.admitted_but_omitted,
            "unknown_due_to_truncation": report.uncertain_truncated,
            "not_eligible_in_complete_manifests": report.not_eligible_in_complete_manifests,
        },
        "tool_identity": report.identity.map(|row| json!({
            "route_id": row.route_id,
            "provider_id": row.provider_id,
            "source_session_id": row.source_session_id,
            "outcome_sequence": row.outcome_sequence,
            "outcome_kind": row.outcome_kind,
        })),
        "dispatches": report.entries.iter().map(|entry| {
            json!({
                "conversation_id": entry.dispatch.conversation_id,
                "turn_id": entry.dispatch.turn_id,
                "request_class": entry.dispatch.request_class,
                "dispatch_sequence": entry.dispatch.sequence,
                "disposition": match entry.disposition {
                    ToolProvenanceDisposition::Included => "included",
                    ToolProvenanceDisposition::AdmittedButOmitted => "admitted_but_omitted",
                    ToolProvenanceDisposition::UnknownBecauseManifestTruncated => "unknown_truncated",
                },
                "remote_outcome_observation": entry.dispatch.transport.status(),
                "composition_reason": entry.dispatch.listed.iter()
                    .find(|row| row.call_id == report.call_id)
                    .and_then(|row| row.composition_reason)
                    .map(ToolCompositionReason::stable_name),
            })
        }).collect::<Vec<_>>(),
        "limits": {
            "legacy_unmanifested_requests_are_unknown": true,
            "manifest_detail_rows_per_request": MAX_MANIFEST_TOOL_ROWS,
            "remote_model_attention_cannot_be_inferred": true,
        },
    })
}

/// Cross-panel selection uses a conversation-scoped, transient UI key.
/// It never modifies the durable evidence journal or admission decisions.
pub fn reverse_query_id(conversation_id: LocalConversationId) -> egui::Id {
    egui::Id::new(("chatarium-reverse-mcp-query", conversation_id.to_string()))
}

pub fn select_reverse_provenance(
    ctx: &egui::Context,
    conversation_id: LocalConversationId,
    call_id: u64,
) {
    if call_id > 0 {
        ctx.data_mut(|data| {
            data.insert_temp(reverse_query_id(conversation_id), call_id.to_string())
        });
    }
}
