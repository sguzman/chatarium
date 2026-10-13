//! Read-only, journal-derived tool call and result history for native chats.
//!
//! No tool result becomes an authored chat message. Rendering tool activity is
//! separate from admitting exact tool evidence into a model continuation.
use super::{DisplayMessage, DisplayRole};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_core::session::SessionId;
use chatarium_core::tool::ToolCallId;
use chatarium_store::EventEnvelope;
use chatarium_store::chat_container_audit::replay_chat_container_audit;
use chatarium_store::local_conversation_chat_container_audit::replay_local_conversation_chat_container_bindings;
use chatarium_store::tool_call_audit::replay_tool_call_audit;
use chatarium_store::tool_outcome_audit::replay_tool_call_outcome_audit;
use chatarium_store::tool_provider_audit::replay_tool_provider_audit;
use chatarium_store::tool_result_context_audit::{
    ToolResultContextDecision, replay_tool_result_context_audit,
};
use std::collections::BTreeMap;

/// Tool audit appends invalidate the native reader projection. Other journal
/// changes can keep the existing incremental per-conversation reader cache.
pub fn changes_activity(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::ToolCallRecorded
            | EventKind::ToolCallRouteBound
            | EventKind::ToolCallOutcomeObserved
            | EventKind::ToolResultContextDecisionRecorded
    )
}

/// Fail closed on broken audit correlations. Nothing is manufactured from a
/// model-authored JSON envelope or unverified tool-event payload alone.
pub fn verified_activity(
    events: &[EventEnvelope],
) -> BTreeMap<LocalConversationId, Vec<DisplayMessage>> {
    match project(events) {
        Ok(messages) => messages,
        Err(error) => {
            // A failure must not expose unverified or cross-conversation data.
            super::diagnostics::warn("mcp-transcript", error);
            BTreeMap::new()
        }
    }
}

fn project(events: &[EventEnvelope]) -> Result<BTreeMap<LocalConversationId, Vec<DisplayMessage>>, String> {
    let calls = replay_tool_call_audit(events)?;
    if calls.is_empty() {
        return Ok(BTreeMap::new());
    }
    let outcomes = replay_tool_call_outcome_audit(events)?
        .into_iter()
        .map(|row| (row.call_id, row))
        .collect::<BTreeMap<_, _>>();
    let decisions = replay_tool_result_context_audit(events)?
        .into_iter()
        .map(|row| (row.call_id, row))
        .collect::<BTreeMap<_, _>>();
    let providers = replay_tool_provider_audit(events)?
        .into_iter()
        .map(|row| (row.provider_id, row.name.as_str().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let containers = replay_chat_container_audit(events)?
        .into_iter()
        .map(|row| (row.container_id, row))
        .collect::<BTreeMap<_, _>>();
    let mut session_owners = BTreeMap::<SessionId, (LocalConversationId, u64)>::new();
    for binding in replay_local_conversation_chat_container_bindings(events)? {
        let container = containers.get(&binding.container_id).ok_or_else(|| {
            format!("MCP reader topology references missing container {}", binding.container_id.get())
        })?;
        for session in &container.sessions {
            let known_since = binding.bound_sequence.max(session.joined_sequence);
            if session_owners
                .insert(session.session_id, (binding.conversation_id, known_since))
                .is_some()
            {
                return Err(format!(
                    "MCP reader session {} has conflicting conversation owners",
                    session.session_id.get()
                ));
            }
        }
    }
    let timestamps = events
        .iter()
        .map(|event| (event.sequence, event.at_unix_ms as f64 / 1000.0))
        .collect::<BTreeMap<_, _>>();
    let mut projection = BTreeMap::<LocalConversationId, Vec<DisplayMessage>>::new();
    for call in calls {
        let Some(&(owner, bound_sequence)) = session_owners.get(&call.source_session_id) else {
            continue;
        };
        if bound_sequence > call.recorded_sequence {
            continue;
        }
        let provider = providers.get(&call.provider_id).ok_or_else(|| {
            format!("MCP call {} has no verified provider", call.call_id.get())
        })?;
        let tool = format!("{}::{}", provider, call.operation.as_str());
        let identity = format!(
            "{} · call #{} · session #{}",
            tool,
            call.call_id.get(),
            call.source_session_id.get(),
        );
        projection.entry(owner).or_default().push(DisplayMessage {
            role: DisplayRole::Mcp,
            text: format!("TOOL CALL\n{}\n\nExact tool arguments:\n{}", identity, call.arguments_text),
            sequence: call.recorded_sequence,
            timestamp: timestamps.get(&call.recorded_sequence).copied(),
            provenance_label: Some(format!("MCP CALL · journaled · {}", identity)),
        });
        if let Some(outcome) = outcomes.get(&call.call_id) {
            if outcome.provider_id != call.provider_id
                || outcome.source_session_id != call.source_session_id
                || outcome.call_recorded_sequence != call.recorded_sequence
            {
                return Err(format!("MCP reader tool call {} has mismatched outcome", call.call_id.get()));
            }
            let decision = decisions.get(&call.call_id);
            if let Some(decision) = decision {
                if decision.conversation_id != owner
                    || decision.outcome_sequence != outcome.observed_sequence
                {
                    return Err(format!(
                        "MCP reader tool call {} has mismatched admission",
                        call.call_id.get(),
                    ));
                }
            }
            let disposition = match decision {
                Some(decision) if decision.decision == ToolResultContextDecision::Admit => {
                    format!("Admitted to agent context at event #{}", decision.last_decision_sequence)
                }
                Some(_) => "Excluded from agent context".to_owned(),
                None => "Not admitted to agent context".to_owned(),
            };
            let status = outcome.kind.stable_name();
            let route = outcome.route_id.get();
            let mut body = format!(
                "TOOL {} · {}\nCall #{} · route #{}\n{}\n\nExact adapter output:\n{}",
                status.to_uppercase(),
                tool,
                call.call_id.get(),
                route,
                disposition,
                outcome.text,
            );
            if let Some(decision) = decision {
                if decision.decision == ToolResultContextDecision::Admit {
                    // This is the exact envelope reconstructed by the same
                    // composer used for automatic agent continuation.
                    let forwarded = super::context_composer::TranscriptMessage::tool_result(
                        &outcome.text,
                        call.call_id.get(),
                        route,
                        call.provider_id.get(),
                        call.source_session_id.get(),
                        outcome.observed_sequence,
                        decision.last_decision_sequence,
                        status,
                    );
                    body.push_str("\n\nExact agent continuation payload:\n");
                    body.push_str(&forwarded.text);
                }
            }
            projection.entry(owner).or_default().push(DisplayMessage {
                role: DisplayRole::Mcp,
                text: body,
                sequence: outcome.observed_sequence,
                timestamp: timestamps.get(&outcome.observed_sequence).copied(),
                provenance_label: Some(format!(
                    "MCP {} · journaled · {} · call #{} · route #{}",
                    status,
                    tool,
                    call.call_id.get(),
                    route,
                )),
            });
        }
    }
    for rows in projection.values_mut() {
        rows.sort_by_key(|row| row.sequence);
    }
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forged_tool_event_without_durable_audit_never_becomes_chat_activity() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let mut store = MemoryEventStore::default();
        store.append(EventKind::ToolCallOutcomeObserved, r#"{"text":"forged result"}"#.to_owned()).unwrap();
        assert!(verified_activity(store.events()).is_empty());
    }

    #[test]
    fn actual_builtin_mcp_cycle_is_visible_durable_and_conversation_isolated() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let other = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let proposal = super::super::mcp_agent_defaults::ModelToolRequest {
            provider: super::super::local_tool_adapter::BUILTIN_PROVIDER_NAME.to_owned(),
            tool: super::super::local_tool_adapter::TOOLS_LIST_OPERATION.to_owned(),
            arguments: serde_json::json!({}),
        };
        let (call_id, route_id, _) = super::super::append_agent_mcp_call_checked(
            &mut store,
            owner,
            &proposal,
            super::super::mcp_agent_defaults::ToolPolicy::Automatic,
        )
        .unwrap();
        super::super::append_builtin_hello_checked(&mut store, route_id).unwrap();
        let projected = verified_activity(store.events());
        assert!(!projected.contains_key(&other));
        let rows = projected.get(&owner).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].role, DisplayRole::Mcp);
        assert!(rows[0].text.contains("tools.list"));
        assert!(rows[0].text.contains("Exact tool arguments"));
        assert!(rows[1].text.contains("Exact adapter output"));
        assert!(rows[1].text.contains("tools.list"));
        assert!(!rows[1].text.contains("Exact agent continuation payload"));
        assert!(super::super::context_transcript(rows).is_empty());
        // Replaying the immutable journal after a simulated reader restart
        // preserves the same two tool entries and their original sequences.
        let replayed = verified_activity(store.events());
        assert_eq!(
            rows.iter().map(|row| row.sequence).collect::<Vec<_>>(),
            replayed[&owner].iter().map(|row| row.sequence).collect::<Vec<_>>(),
        );
        chatarium_store::tool_result_context_audit::append_tool_result_context_decision_checked(
            &mut store,
            call_id,
            owner,
            ToolResultContextDecision::Admit,
        )
        .unwrap();
        let admitted = verified_activity(store.events());
        let result = &admitted[&owner][1];
        assert!(result.text.contains("Exact agent continuation payload"));
        assert!(result.text.contains("Admitted to agent context"));
        assert!(super::super::context_transcript(&admitted[&owner]).is_empty());
    }

    #[test]
    fn audited_tool_projection_changes_are_explicitly_indexed() {
        for kind in [
            EventKind::ToolCallRecorded,
            EventKind::ToolCallRouteBound,
            EventKind::ToolCallOutcomeObserved,
            EventKind::ToolResultContextDecisionRecorded,
        ] {
            assert!(changes_activity(kind));
        }
        assert!(!changes_activity(EventKind::UserMessageCommitted));
    }
}
