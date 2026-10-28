//! Desktop checkpoint helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn checkpoint_owner_for_turn(
    events: &[EventEnvelope],
    turn_id: LocalTurnId,
) -> Option<LocalConversationId> {
    let scope = local_turn_scope(turn_id);
    let mut owner = None;
    for event in events {
        if event.kind != EventKind::UserMessageCommitted {
            continue;
        }
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        if local_turn_scope(message.turn_id) != scope {
            continue;
        }
        if owner.is_some_and(|existing| existing != message.conversation_id) {
            return None;
        }
        owner = Some(message.conversation_id);
    }
    owner
}

pub(crate) fn checkpoint_completion_eligible(event: &EventEnvelope) -> bool {
    if event.kind != EventKind::AssistantCompletionObserved {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
        return false;
    };
    let Some(text) = payload["text"].as_str() else {
        return false;
    };
    text.trim().chars().count() >= 160
        && matches!(mcp_agent_defaults::parse_model_tool_request(text), Ok(None))
}

/// Recover committed assistant completions after the durable activation
/// boundary. A crash between journaling completion and creating the outbox
/// item must not permanently lose the checkpoint. Never ingest pre-existing
/// transcript history on first installation or assign an ambiguous owner.
pub(crate) fn reconcile_checkpoint_outbox(
    store: &impl EventStore,
    path: &std::path::Path,
) -> Result<usize, String> {
    let mut outbox = openviking_checkpoint_outbox::Outbox::load(path)?;
    let journal_high_water = store.events().last().map_or(0, |event| event.sequence);
    outbox.activate_durable(path, journal_high_water)?;
    let boundary = outbox
        .started_after_sequence()
        .unwrap_or(journal_high_water);
    // Build a unique-owner index once, and fail closed if two authored
    // conversations ever claim the same turn. No O(history^2) replay.
    let mut owners = BTreeMap::<String, Option<LocalConversationId>>::new();
    for event in store.events() {
        if event.kind != EventKind::UserMessageCommitted {
            continue;
        }
        let Ok(Some(DecodedUserMessageCommit::Typed(message))) = decode_user_message_commit(event)
        else {
            continue;
        };
        match owners.entry(local_turn_scope(message.turn_id)) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(Some(message.conversation_id));
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if slot.get() != &Some(message.conversation_id) {
                    slot.insert(None);
                }
            }
        }
    }
    let mut recovered = 0;
    for event in store.events() {
        if event.sequence <= boundary || !checkpoint_completion_eligible(event) {
            continue;
        }
        let Some(scope) = event.scope.as_deref() else {
            continue;
        };
        let Some(turn_string) = scope.strip_prefix("local-turn:") else {
            continue;
        };
        let Some(Some(owner)) = owners.get(scope) else {
            continue;
        };
        if outbox.enqueue_durable(path, &owner.to_string(), turn_string, event.sequence)? {
            recovered += 1;
        }
    }
    Ok(recovered)
}
