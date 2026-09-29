//! Typed authored-turn rows derived only from authoritative durable journal events.

use crate::EventEnvelope;
use crate::authored::{DecodedUserMessageCommit, decode_user_message_commit, local_turn_scope};
use chatarium_core::{
    AssistantEvidence, LocalConversationId, LocalEvidence, LocalMessageId, LocalTurnId,
    RemoteEvidence, TurnEvidence,
};
use std::collections::{BTreeMap, BTreeSet};

/// One disposable authored-turn projection row derived from durable events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredTurnRow {
    /// Local conversation identity from the typed user-message commit.
    pub conversation_id: LocalConversationId,
    /// Local turn identity from the typed user-message commit.
    pub turn_id: LocalTurnId,
    /// Local user-message identity from the typed user-message commit.
    pub message_id: LocalMessageId,
    /// Exact user-authored text from the durable commit.
    pub exact_user_text: String,
    /// Durable sequence containing the typed user-message commit.
    pub commit_sequence: u64,
    /// Last durable sequence applied to this turn projection.
    pub last_sequence: u64,
    /// Replayed turn evidence.
    pub evidence: TurnEvidence,
}

struct Accumulator {
    row: AuthoredTurnRow,
    scope: String,
}

/// Derive typed authored turns from journal events without inventing IDs for legacy commits.
pub(crate) fn derive_authored_turns(
    events: &[EventEnvelope],
) -> Result<Vec<AuthoredTurnRow>, String> {
    let mut turns = BTreeMap::<LocalTurnId, Accumulator>::new();
    let mut message_ids = BTreeSet::<LocalMessageId>::new();
    let mut scopes = BTreeMap::<String, LocalTurnId>::new();

    for event in events {
        let Some(decoded) = decode_user_message_commit(event)? else {
            continue;
        };
        let DecodedUserMessageCommit::Typed(message) = decoded else {
            continue;
        };

        let expected_scope = local_turn_scope(message.turn_id);
        if event.scope.as_deref() != Some(expected_scope.as_str()) {
            return Err(format!(
                "typed user-message commit at sequence {} has scope {:?}, expected {:?}",
                event.sequence, event.scope, expected_scope
            ));
        }
        if turns.contains_key(&message.turn_id) {
            return Err(format!(
                "duplicate typed user-message commit for local turn {}",
                message.turn_id
            ));
        }
        if !message_ids.insert(message.message_id) {
            return Err(format!(
                "duplicate local message identity {} in typed authored commits",
                message.message_id
            ));
        }
        if scopes
            .insert(expected_scope.clone(), message.turn_id)
            .is_some()
        {
            return Err(format!("duplicate typed local-turn scope {expected_scope}"));
        }

        turns.insert(
            message.turn_id,
            Accumulator {
                scope: expected_scope,
                row: AuthoredTurnRow {
                    conversation_id: message.conversation_id,
                    turn_id: message.turn_id,
                    message_id: message.message_id,
                    exact_user_text: message.text,
                    commit_sequence: event.sequence,
                    last_sequence: event.sequence,
                    evidence: TurnEvidence::default(),
                },
            },
        );
    }

    for event in events {
        let Some(scope) = event.scope.as_deref() else {
            continue;
        };
        let Some(turn_id) = scopes.get(scope).copied() else {
            continue;
        };
        let accumulator = turns
            .get_mut(&turn_id)
            .expect("scope map only contains known typed turns");

        if event.kind == chatarium_core::EventKind::UserMessageCommitted
            && event.sequence != accumulator.row.commit_sequence
        {
            return Err(format!(
                "typed local turn {} contains an additional user-message commit at sequence {}",
                turn_id, event.sequence
            ));
        }

        accumulator
            .row
            .evidence
            .apply_event_kind(event.kind)
            .map_err(|error| {
                format!(
                    "cannot replay local turn {} at sequence {}: {error}",
                    turn_id, event.sequence
                )
            })?;
        accumulator.row.last_sequence = event.sequence;
    }

    let mut rows = turns
        .into_values()
        .map(|accumulator| {
            debug_assert_eq!(accumulator.scope, local_turn_scope(accumulator.row.turn_id));
            accumulator.row
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.commit_sequence);
    Ok(rows)
}

pub(crate) const fn local_evidence_name(value: LocalEvidence) -> &'static str {
    match value {
        LocalEvidence::DraftOnly => "draft_only",
        LocalEvidence::MessageCommitted => "message_committed",
    }
}

pub(crate) fn parse_local_evidence(value: &str) -> Option<LocalEvidence> {
    match value {
        "draft_only" => Some(LocalEvidence::DraftOnly),
        "message_committed" => Some(LocalEvidence::MessageCommitted),
        _ => None,
    }
}

pub(crate) const fn remote_evidence_name(value: RemoteEvidence) -> &'static str {
    match value {
        RemoteEvidence::NotAttempted => "not_attempted",
        RemoteEvidence::Dispatching => "dispatching",
        RemoteEvidence::OutcomeUnknown => "outcome_unknown",
        RemoteEvidence::AcceptedObserved => "accepted_observed",
        RemoteEvidence::FailedObserved => "failed_observed",
    }
}

pub(crate) fn parse_remote_evidence(value: &str) -> Option<RemoteEvidence> {
    match value {
        "not_attempted" => Some(RemoteEvidence::NotAttempted),
        "dispatching" => Some(RemoteEvidence::Dispatching),
        "outcome_unknown" => Some(RemoteEvidence::OutcomeUnknown),
        "accepted_observed" => Some(RemoteEvidence::AcceptedObserved),
        "failed_observed" => Some(RemoteEvidence::FailedObserved),
        _ => None,
    }
}

pub(crate) const fn assistant_evidence_name(value: AssistantEvidence) -> &'static str {
    match value {
        AssistantEvidence::None => "none",
        AssistantEvidence::Streaming => "streaming",
        AssistantEvidence::PartialInterrupted => "partial_interrupted",
        AssistantEvidence::CompletedObserved => "completed_observed",
    }
}

pub(crate) fn parse_assistant_evidence(value: &str) -> Option<AssistantEvidence> {
    match value {
        "none" => Some(AssistantEvidence::None),
        "streaming" => Some(AssistantEvidence::Streaming),
        "partial_interrupted" => Some(AssistantEvidence::PartialInterrupted),
        "completed_observed" => Some(AssistantEvidence::CompletedObserved),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authored::commit_user_message;
    use crate::{EventStore, MemoryEventStore};
    use chatarium_core::{AuthoredUserMessage, EventKind};

    fn authored() -> AuthoredUserMessage {
        AuthoredUserMessage::new(
            LocalConversationId::new(),
            LocalTurnId::new(),
            LocalMessageId::new(),
            " exact authored text\nwith spacing ",
        )
    }

    #[test]
    fn typed_commit_projects_exact_identity_text_and_committed_evidence() {
        let message = authored();
        let mut store = MemoryEventStore::default();
        let receipt = commit_user_message(&mut store, &message).unwrap();

        let rows = derive_authored_turns(store.events()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conversation_id, message.conversation_id);
        assert_eq!(rows[0].turn_id, message.turn_id);
        assert_eq!(rows[0].message_id, message.message_id);
        assert_eq!(rows[0].exact_user_text, message.text);
        assert_eq!(rows[0].commit_sequence, receipt.sequence);
        assert_eq!(rows[0].last_sequence, receipt.sequence);
        assert_eq!(rows[0].evidence.local, LocalEvidence::MessageCommitted);
        assert_eq!(rows[0].evidence.remote, RemoteEvidence::NotAttempted);
        assert_eq!(rows[0].evidence.assistant, AssistantEvidence::None);
    }

    #[test]
    fn replay_preserves_remote_ambiguity() {
        let message = authored();
        let scope = local_turn_scope(message.turn_id);
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, &message).unwrap();
        store
            .append_scoped(
                Some(scope.clone()),
                EventKind::DispatchAttempted,
                String::new(),
            )
            .unwrap();
        store
            .append_scoped(Some(scope), EventKind::TransportInterrupted, String::new())
            .unwrap();

        let row = derive_authored_turns(store.events()).unwrap().remove(0);
        assert_eq!(row.evidence.remote, RemoteEvidence::OutcomeUnknown);
        assert_eq!(row.last_sequence, 3);
    }

    #[test]
    fn accepted_partial_output_remains_partial_after_interruption() {
        let message = authored();
        let scope = local_turn_scope(message.turn_id);
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, &message).unwrap();
        for kind in [
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::AssistantStreamStarted,
            EventKind::AssistantDeltaObserved,
            EventKind::TransportInterrupted,
        ] {
            store
                .append_scoped(Some(scope.clone()), kind, String::new())
                .unwrap();
        }

        let row = derive_authored_turns(store.events()).unwrap().remove(0);
        assert_eq!(row.evidence.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(
            row.evidence.assistant,
            AssistantEvidence::PartialInterrupted
        );
        assert_eq!(row.last_sequence, 6);
    }

    #[test]
    fn later_completion_advances_partial_turn() {
        let message = authored();
        let scope = local_turn_scope(message.turn_id);
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, &message).unwrap();
        for kind in [
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::AssistantStreamStarted,
            EventKind::TransportInterrupted,
            EventKind::AssistantCompletionObserved,
        ] {
            store
                .append_scoped(Some(scope.clone()), kind, String::new())
                .unwrap();
        }

        let row = derive_authored_turns(store.events()).unwrap().remove(0);
        assert_eq!(row.evidence.assistant, AssistantEvidence::CompletedObserved);
    }

    #[test]
    fn unrelated_scope_does_not_affect_turn() {
        let message = authored();
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, &message).unwrap();
        store
            .append_scoped(
                Some("conversation:unrelated".to_owned()),
                EventKind::DispatchAttempted,
                String::new(),
            )
            .unwrap();

        let row = derive_authored_turns(store.events()).unwrap().remove(0);
        assert_eq!(row.evidence.remote, RemoteEvidence::NotAttempted);
        assert_eq!(row.last_sequence, 1);
    }

    #[test]
    fn legacy_commit_does_not_invent_typed_turn() {
        let mut store = MemoryEventStore::default();
        store
            .append(
                EventKind::UserMessageCommitted,
                "legacy exact text".to_owned(),
            )
            .unwrap();
        assert!(derive_authored_turns(store.events()).unwrap().is_empty());
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let message = authored();
        let mut store = MemoryEventStore::default();
        let payload = serde_json::json!({
            "schema": "chatarium-user-message-commit",
            "version": 1,
            "conversation_id": message.conversation_id.to_string(),
            "turn_id": message.turn_id.to_string(),
            "message_id": message.message_id.to_string(),
            "text": message.text,
        })
        .to_string();
        store
            .append_scoped(
                Some("local-turn:not-the-turn".to_owned()),
                EventKind::UserMessageCommitted,
                payload,
            )
            .unwrap();

        assert!(
            derive_authored_turns(store.events())
                .unwrap_err()
                .contains("expected")
        );
    }

    #[test]
    fn dispatch_before_commit_in_same_scope_is_rejected() {
        let message = authored();
        let scope = local_turn_scope(message.turn_id);
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(scope.clone()),
                EventKind::DispatchAttempted,
                String::new(),
            )
            .unwrap();
        commit_user_message(&mut store, &message).unwrap();

        let error = derive_authored_turns(store.events()).unwrap_err();
        assert!(error.contains("dispatch evidence appeared before local message commit"));
    }

    #[test]
    fn duplicate_typed_turn_commit_is_rejected() {
        let message = authored();
        let mut store = MemoryEventStore::default();
        commit_user_message(&mut store, &message).unwrap();
        commit_user_message(&mut store, &message).unwrap();
        let error = derive_authored_turns(store.events()).unwrap_err();
        assert!(error.contains("duplicate typed user-message commit"));
    }
}
