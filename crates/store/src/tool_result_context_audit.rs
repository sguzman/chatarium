//! Explicit, reversible admission of observed tool outcomes to one local conversation.
//!
//! A completed tool call is not automatically model context. This audit grants
//! only read-context eligibility, never execution authority or transcript authorship.
//! Every decision is checked against the original outcome's historical owning
//! conversation, so a later topology change cannot claim another session's data.

use crate::chat_container_audit::replay_chat_container_audit;
use crate::local_conversation_chat_container_audit::replay_local_conversation_chat_container_bindings;
use crate::tool_outcome_audit::{
    ToolCallOutcomeKind, ToolCallOutcomeRecord, replay_tool_call_outcome_audit,
};
use crate::{EventEnvelope, EventStore};
use chatarium_core::routing::RouteId;
use chatarium_core::session::SessionId;
use chatarium_core::tool::{ToolCallId, ToolProviderId};
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-tool-result-context-audit";
const VERSION: u64 = 1;
/// Bounded user-selected inference evidence; larger terminal outcomes remain
/// inspectable but cannot silently enter context or be truncated.
pub const MAX_CONTEXT_TOOL_RESULT_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultContextDecision {
    Admit,
    Exclude,
}
impl ToolResultContextDecision {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Exclude => "exclude",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultContextRecord {
    pub call_id: ToolCallId,
    pub route_id: RouteId,
    pub provider_id: ToolProviderId,
    pub source_session_id: SessionId,
    pub conversation_id: LocalConversationId,
    pub outcome_sequence: u64,
    pub outcome_kind: ToolCallOutcomeKind,
    pub decision: ToolResultContextDecision,
    pub first_decision_sequence: u64,
    pub last_decision_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedToolResult {
    pub record: ToolResultContextRecord,
    /// Exact adapter-observed terminal result/error, not interpreted as an instruction.
    pub text: String,
}

/// Low-level append: callers should use checked pre-append validation below.
pub fn record_tool_result_context_decision(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    route_id: RouteId,
    outcome_sequence: u64,
    conversation_id: LocalConversationId,
    decision: ToolResultContextDecision,
) -> std::io::Result<u64> {
    let payload = event_value(call_id, route_id, outcome_sequence, conversation_id, decision);
    store.append_scoped(
        Some(context_scope(conversation_id, call_id)),
        EventKind::ToolResultContextDecisionRecorded,
        serde_json::to_string(&payload).map_err(invalid_data)?,
    )
}

/// Refuse invalid/no-op decisions before mutating the authoritative journal.
pub fn append_tool_result_context_decision_checked(
    store: &mut impl EventStore,
    call_id: ToolCallId,
    conversation_id: LocalConversationId,
    decision: ToolResultContextDecision,
) -> Result<EventEnvelope, String> {
    let outcome = replay_tool_call_outcome_audit(store.events())?
        .into_iter()
        .find(|item| item.call_id == call_id)
        .ok_or_else(|| format!("tool call {} has no observed terminal outcome", call_id.get()))?;
    if let Some(existing) = replay_tool_result_context_audit(store.events())?
        .into_iter()
        .find(|item| item.call_id == call_id)
    {
        if existing.decision == decision && existing.conversation_id == conversation_id {
            return Err(format!(
                "tool call {} already has context decision {}",
                call_id.get(),
                decision.stable_name()
            ));
        }
    }

    let next = u64::try_from(store.events().len())
        .map_err(|error| error.to_string())?
        .checked_add(1)
        .ok_or_else(|| "tool result context sequence exhausted".to_owned())?;
    let mut prospective = store.events().to_vec();
    prospective.push(EventEnvelope {
        sequence: next,
        at_unix_ms: 0,
        scope: Some(context_scope(conversation_id, call_id)),
        kind: EventKind::ToolResultContextDecisionRecorded,
        payload: event_value(
            call_id,
            outcome.route_id,
            outcome.observed_sequence,
            conversation_id,
            decision,
        )
        .to_string(),
    });
    let verified = replay_tool_result_context_audit(&prospective)?;
    if !verified.iter().any(|record| {
        record.call_id == call_id
            && record.conversation_id == conversation_id
            && record.decision == decision
            && record.last_decision_sequence == next
    }) {
        return Err("prospective tool result context decision did not replay".to_owned());
    }

    record_tool_result_context_decision(
        store,
        call_id,
        outcome.route_id,
        outcome.observed_sequence,
        conversation_id,
        decision,
    )
    .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool result context append produced no event".to_owned())
}

/// Replay each decision against the outcome and conversation ownership as they
/// existed when the terminal result was originally observed.
pub fn replay_tool_result_context_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolResultContextRecord>, String> {
    let mut records = BTreeMap::<ToolCallId, ToolResultContextRecord>::new();
    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ToolResultContextDecisionRecorded {
            continue;
        }
        let value = typed_payload(event)?;
        let call_id = ToolCallId::new(required_u64(&value, "call_id")?);
        let route_id = RouteId::new(required_u64(&value, "route_id")?);
        let outcome_sequence = required_u64(&value, "outcome_sequence")?;
        let conversation_id =
            LocalConversationId::from_str(required_string(&value, "conversation_id")?)
                .map_err(|error| format!("tool result context at #{} has invalid conversation: {error}", event.sequence))?;
        let decision = match required_string(&value, "decision")? {
            "admit" => ToolResultContextDecision::Admit,
            "exclude" => ToolResultContextDecision::Exclude,
            other => return Err(format!("tool result context at #{} has unsupported decision '{other}'", event.sequence)),
        };
        if event.scope.as_deref() != Some(context_scope(conversation_id, call_id).as_str()) {
            return Err(format!("tool result context at #{} has invalid scope", event.sequence));
        }

        let prior = &events[..index];
        let outcome = replay_tool_call_outcome_audit(prior)?
            .into_iter()
            .find(|item| item.call_id == call_id)
            .ok_or_else(|| {
                format!(
                    "tool result context at #{} references call {} before terminal outcome",
                    event.sequence, call_id.get()
                )
            })?;
        if route_id != outcome.route_id || outcome_sequence != outcome.observed_sequence {
            return Err(format!(
                "tool result context at #{} disagrees with immutable call/route/outcome correlation",
                event.sequence
            ));
        }

        // Verify ownership at outcome time, not the later decision time.
        // This prevents retrospective session reassignment from importing data.
        let outcome_index = prior
            .iter()
            .position(|item| item.sequence == outcome.observed_sequence)
            .ok_or_else(|| "tool outcome sequence not found in journal prefix".to_owned())?;
        let owner = conversation_for_outcome_session(
            &events[..=outcome_index],
            outcome.source_session_id,
        )?;
        if owner != Some(conversation_id) {
            return Err(format!(
                "tool result context at #{} cannot admit tool call {} to conversation {}: source session {} has different/no ownership at outcome time",
                event.sequence,
                call_id.get(),
                conversation_id,
                outcome.source_session_id.get()
            ));
        }

        if outcome.text.len() > MAX_CONTEXT_TOOL_RESULT_BYTES {
            return Err(format!(
                "tool result context at #{} exceeds {} byte context cap",
                event.sequence, MAX_CONTEXT_TOOL_RESULT_BYTES
            ));
        }

        match records.get_mut(&call_id) {
            Some(record) => {
                if record.conversation_id != conversation_id
                    || record.route_id != route_id
                    || record.outcome_sequence != outcome_sequence
                {
                    return Err(format!(
                        "tool result context at #{} conflicts with previous admission identity",
                        event.sequence
                    ));
                }
                record.decision = decision;
                record.last_decision_sequence = event.sequence;
            }
            None => {
                records.insert(
                    call_id,
                    ToolResultContextRecord {
                        call_id,
                        route_id,
                        provider_id: outcome.provider_id,
                        source_session_id: outcome.source_session_id,
                        conversation_id,
                        outcome_sequence,
                        outcome_kind: outcome.kind,
                        decision,
                        first_decision_sequence: event.sequence,
                        last_decision_sequence: event.sequence,
                    },
                );
            }
        }
    }
    let mut result = records.into_values().collect::<Vec<_>>();
    result.sort_by_key(|record| record.first_decision_sequence);
    Ok(result)
}

/// Resolve the local conversation that owned the source session *when* a
/// validated terminal outcome was observed. Returns None for standalone
/// sessions not attached to a local chat container at that time.
pub fn tool_outcome_owning_conversation(
    events: &[EventEnvelope],
    outcome: &ToolCallOutcomeRecord,
) -> Result<Option<LocalConversationId>, String> {
    let position = events.iter().position(|item| {
        item.sequence == outcome.observed_sequence
            && item.kind == EventKind::ToolCallOutcomeObserved
    }).ok_or_else(|| format!(
        "tool call {} terminal outcome sequence {} is absent",
        outcome.call_id.get(),
        outcome.observed_sequence
    ))?;
    conversation_for_outcome_session(&events[..=position], outcome.source_session_id)
}

/// Exact, explicitly admitted outcomes for one local conversation only.
pub fn replay_admitted_tool_results(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<AdmittedToolResult>, String> {
    let outcomes = replay_tool_call_outcome_audit(events)?
        .into_iter()
        .map(|item| (item.call_id, item))
        .collect::<BTreeMap<_, _>>();
    let mut result = Vec::new();
    for record in replay_tool_result_context_audit(events)? {
        if record.conversation_id != conversation_id
            || record.decision != ToolResultContextDecision::Admit
        {
            continue;
        }
        let outcome = outcomes.get(&record.call_id).ok_or_else(|| {
            format!("admitted tool call {} has no terminal outcome", record.call_id.get())
        })?;
        if outcome.observed_sequence != record.outcome_sequence
            || outcome.route_id != record.route_id
            || outcome.source_session_id != record.source_session_id
            || outcome.provider_id != record.provider_id
            || outcome.kind != record.outcome_kind
        {
            return Err(format!("tool call {} outcome changed after admission", record.call_id.get()));
        }
        result.push(AdmittedToolResult {
            record,
            text: outcome.text.clone(),
        });
    }
    result.sort_by_key(|item| item.record.last_decision_sequence);
    Ok(result)
}

fn conversation_for_outcome_session(
    events: &[EventEnvelope],
    source_session: SessionId,
) -> Result<Option<LocalConversationId>, String> {
    let containers = replay_chat_container_audit(events)?;
    let bindings = replay_local_conversation_chat_container_bindings(events)?;
    let mut owner = None;
    for binding in bindings {
        let container = containers.iter().find(|item| item.container_id == binding.container_id)
            .ok_or_else(|| format!("tool result owner references absent chat container {}", binding.container_id.get()))?;
        if container.sessions.iter().any(|item| item.session_id == source_session) {
            if owner.replace(binding.conversation_id).is_some() {
                return Err(format!(
                    "source session {} is claimed by multiple local conversations",
                    source_session.get()
                ));
            }
        }
    }
    Ok(owner)
}

#[must_use]
pub fn context_scope(conversation_id: LocalConversationId, call_id: ToolCallId) -> String {
    format!("tool-result-context:{conversation_id}:{}", call_id.get())
}

fn event_value(
    call_id: ToolCallId,
    route_id: RouteId,
    outcome_sequence: u64,
    conversation_id: LocalConversationId,
    decision: ToolResultContextDecision,
) -> Value {
    json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "tool_result_context_decision",
        "call_id": call_id.get(),
        "route_id": route_id.get(),
        "outcome_sequence": outcome_sequence,
        "conversation_id": conversation_id.to_string(),
        "decision": decision.stable_name(),
    })
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload)
        .map_err(|error| format!("malformed tool result context at #{}: {error}", event.sequence))?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
        || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        || value.get("record").and_then(Value::as_str) != Some("tool_result_context_decision")
    {
        return Err(format!(
            "tool result context at #{} has invalid schema/version/record",
            event.sequence
        ));
    }
    Ok(value)
}

fn required_u64(value: &Value, key: &str) -> Result<u64, String> {
    value.get(key).and_then(Value::as_u64)
        .ok_or_else(|| format!("tool result context missing unsigned '{key}'"))
}
fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value.get(key).and_then(Value::as_str)
        .ok_or_else(|| format!("tool result context missing string '{key}'"))
}
fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_container_audit::{
        record_chat_container_created, record_chat_session_lifecycle_transition,
        record_chat_session_successor_bound,
    };
    use crate::local_conversation_chat_container_audit::record_local_conversation_chat_container_bound;
    use crate::routing_audit::{
        RouteUserDecision, record_route_dispatched, record_route_proposed,
        record_route_user_decision,
    };
    use crate::session_audit::{record_local_session_registered, record_session_endpoint_bound};
    use crate::tool_call_audit::{record_tool_call, record_tool_call_route_bound};
    use crate::tool_outcome_audit::record_tool_call_outcome;
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use crate::MemoryEventStore;
    use chatarium_core::chat_container::{
        ChatContainerId, ContextHandoffId, SessionLifecyclePhase,
        SessionLifecycleTransition, SessionSuccessorBinding,
    };
    use chatarium_core::routing::{
        RouteClass, RouteEndpointId, RouteGate, RoutePolicy, RouteRequest,
    };
    use chatarium_core::session::SessionEndpointBinding;
    use chatarium_core::tool::{
        ToolOperationName, ToolProviderEndpointBinding, ToolProviderName,
    };

    const SESSION: SessionId = SessionId::new(1);
    const SOURCE: RouteEndpointId = RouteEndpointId::new(10);
    const DEST: RouteEndpointId = RouteEndpointId::new(20);
    const PROVIDER: ToolProviderId = ToolProviderId::new(3);
    const CALL: ToolCallId = ToolCallId::new(5);
    const ROUTE: RouteId = RouteId::new(7);

    fn delivered(store: &mut impl EventStore, conversation: LocalConversationId, text: &str) {
        record_local_session_registered(store, SESSION).unwrap();
        record_chat_container_created(store, ChatContainerId::new(1), SESSION).unwrap();
        record_local_conversation_chat_container_bound(store, conversation, ChatContainerId::new(1)).unwrap();
        record_session_endpoint_bound(store, SessionEndpointBinding::new(SESSION, SOURCE)).unwrap();
        record_tool_provider_registered(store, PROVIDER, &ToolProviderName::new("chatarium.builtin").unwrap()).unwrap();
        record_tool_provider_endpoint_bound(
            store, ToolProviderEndpointBinding::new(PROVIDER, DEST)
        ).unwrap();
        record_tool_call(
            store, CALL, SESSION, PROVIDER, &ToolOperationName::new("hello").unwrap(), "{}"
        ).unwrap();
        let route = RouteRequest {
            id: ROUTE, source: SOURCE, destination: DEST, class: RouteClass::ToolCall
        };
        record_route_proposed(store, route, RoutePolicy::RequireApproval).unwrap();
        record_tool_call_route_bound(store, CALL, ROUTE).unwrap();
        record_route_user_decision(store, ROUTE, RouteUserDecision::Allow).unwrap();
        let mut gate = RouteGate::new(route, RoutePolicy::RequireApproval);
        gate.user_allow().unwrap();
        let permit = gate.authorize_dispatch(ROUTE).unwrap();
        record_route_dispatched(store, permit).unwrap();
        record_tool_call_outcome(store, CALL, ROUTE, ToolCallOutcomeKind::Result, text).unwrap();
    }

    #[test]
    fn result_requires_explicit_admission_and_can_be_excluded_again() {
        let mut store = MemoryEventStore::default();
        let conversation = LocalConversationId::new();
        delivered(&mut store, conversation, " exact tool result\n ");
        assert!(replay_admitted_tool_results(store.events(), conversation).unwrap().is_empty());

        let admission = append_tool_result_context_decision_checked(
            &mut store, CALL, conversation, ToolResultContextDecision::Admit
        ).unwrap();
        assert_eq!(admission.kind, EventKind::ToolResultContextDecisionRecorded);
        let current = replay_admitted_tool_results(store.events(), conversation).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].text, " exact tool result\n ");
        assert_eq!(current[0].record.source_session_id, SESSION);
        assert_eq!(current[0].record.route_id, ROUTE);

        assert!(append_tool_result_context_decision_checked(
            &mut store, CALL, conversation, ToolResultContextDecision::Admit
        ).unwrap_err().contains("already has"));

        append_tool_result_context_decision_checked(
            &mut store, CALL, conversation, ToolResultContextDecision::Exclude
        ).unwrap();
        assert!(replay_admitted_tool_results(store.events(), conversation).unwrap().is_empty());
        assert_eq!(replay_tool_call_outcome_audit(store.events()).unwrap().len(), 1);
    }

    #[test]
    fn rejects_foreign_conversation_and_oversized_result_without_append() {
        let mut store = MemoryEventStore::default();
        let conversation = LocalConversationId::new();
        delivered(&mut store, conversation, "result");
        let before = store.events().len();
        let error = append_tool_result_context_decision_checked(
            &mut store, CALL, LocalConversationId::new(), ToolResultContextDecision::Admit
        ).unwrap_err();
        assert!(error.contains("different/no ownership"));
        assert_eq!(store.events().len(), before);

        let mut store = MemoryEventStore::default();
        let owner = LocalConversationId::new();
        delivered(
            &mut store,
            owner,
            &"a".repeat(MAX_CONTEXT_TOOL_RESULT_BYTES + 1),
        );
        let before = store.events().len();
        let error = append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Admit,
        )
        .unwrap_err();
        assert!(error.contains("context cap"));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn original_owner_may_admit_after_session_rollover() {
        let mut store = MemoryEventStore::default();
        let owner = LocalConversationId::new();
        delivered(&mut store, owner, "predecessor tool output");
        let terminal = replay_tool_call_outcome_audit(store.events()).unwrap().remove(0);
        assert_eq!(
            tool_outcome_owning_conversation(store.events(), &terminal).unwrap(),
            Some(owner)
        );

        record_chat_session_lifecycle_transition(
            &mut store,
            ChatContainerId::new(1),
            SessionLifecycleTransition::new(
                SESSION,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .unwrap(),
        )
        .unwrap();
        let successor = SessionId::new(2);
        record_local_session_registered(&mut store, successor).unwrap();
        record_chat_session_successor_bound(
            &mut store,
            SessionSuccessorBinding::new(
                ChatContainerId::new(1),
                SESSION,
                successor,
                ContextHandoffId::new(1),
            )
            .unwrap(),
        )
        .unwrap();

        append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Admit,
        )
        .unwrap();
        let admitted = replay_admitted_tool_results(store.events(), owner).unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].text, "predecessor tool output");
        assert_eq!(admitted[0].record.source_session_id, SESSION);
    }

    #[test]
    fn later_conversation_binding_cannot_retroactively_claim_outcome() {
        let mut store = MemoryEventStore::default();
        record_local_session_registered(&mut store, SESSION).unwrap();
        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(SESSION, SOURCE),
        )
        .unwrap();
        record_tool_provider_registered(
            &mut store,
            PROVIDER,
            &ToolProviderName::new("chatarium.builtin").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(PROVIDER, DEST),
        )
        .unwrap();
        record_tool_call(
            &mut store,
            CALL,
            SESSION,
            PROVIDER,
            &ToolOperationName::new("hello").unwrap(),
            "{}",
        )
        .unwrap();
        let route = RouteRequest {
            id: ROUTE,
            source: SOURCE,
            destination: DEST,
            class: RouteClass::ToolCall,
        };
        record_route_proposed(&mut store, route, RoutePolicy::RequireApproval).unwrap();
        record_tool_call_route_bound(&mut store, CALL, ROUTE).unwrap();
        record_route_user_decision(&mut store, ROUTE, RouteUserDecision::Allow).unwrap();
        let mut gate = RouteGate::new(route, RoutePolicy::RequireApproval);
        gate.user_allow().unwrap();
        record_route_dispatched(
            &mut store,
            gate.authorize_dispatch(ROUTE).unwrap(),
        )
        .unwrap();
        record_tool_call_outcome(
            &mut store,
            CALL,
            ROUTE,
            ToolCallOutcomeKind::Result,
            "orphan",
        )
        .unwrap();
        let owner = LocalConversationId::new();
        record_chat_container_created(&mut store, ChatContainerId::new(1), SESSION).unwrap();
        record_local_conversation_chat_container_bound(
            &mut store,
            owner,
            ChatContainerId::new(1),
        )
        .unwrap();

        let before = store.events().len();
        let error = append_tool_result_context_decision_checked(
            &mut store,
            CALL,
            owner,
            ToolResultContextDecision::Admit,
        )
        .unwrap_err();
        assert!(error.contains("different/no ownership"));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn cannot_admit_unknown_or_undispatched_call() {
        let mut store = MemoryEventStore::default();
        let err = append_tool_result_context_decision_checked(
            &mut store, CALL, LocalConversationId::new(), ToolResultContextDecision::Admit
        ).unwrap_err();
        assert!(err.contains("no observed terminal outcome"));
    }
}
