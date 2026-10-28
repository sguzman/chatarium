//! Desktop recovery helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn projected_working_draft(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> String {
    let scope = local_conversation_scope(conversation_id);
    let scoped_draft = events.iter().rev().find(|event| {
        event.kind == EventKind::DraftChanged && event.scope.as_deref() == Some(scope.as_str())
    });
    let legacy_owner = projected_local_conversation_id(events).ok().flatten();
    let latest_draft = scoped_draft.or_else(|| {
        (legacy_owner == Some(conversation_id))
            .then(|| {
                events
                    .iter()
                    .rev()
                    .find(|event| event.kind == EventKind::DraftChanged && event.scope.is_none())
            })
            .flatten()
    });
    let latest_commit_sequence = events
        .iter()
        .filter_map(|event| {
            let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                decode_user_message_commit(event)
            else {
                return None;
            };
            (message.conversation_id == conversation_id).then_some(event.sequence)
        })
        .next_back()
        .unwrap_or_default();

    match latest_draft {
        Some(event) if event.sequence > latest_commit_sequence => event_text(&event.payload),
        _ => String::new(),
    }
}

pub(crate) fn recover_interrupted_remote_turns(store: &mut impl EventStore) -> Result<usize, String> {
    let mut authored_turns = HashSet::new();
    for event in store.events() {
        let Some(decoded) = decode_user_message_commit(event)? else {
            continue;
        };
        if let DecodedUserMessageCommit::Typed(message) = decoded {
            authored_turns.insert(message.turn_id);
        }
    }

    let mut interrupted = Vec::new();
    for turn_id in authored_turns {
        let scope = local_turn_scope(turn_id);
        let kinds = store
            .events()
            .iter()
            .filter(|event| event.scope.as_deref() == Some(scope.as_str()))
            .map(|event| event.kind)
            .collect::<Vec<_>>();
        // Only the most recent dispatch step determines recovery. An earlier
        // failed/completed model attempt cannot mask an interrupted MCP
        // continuation under the same user-authored turn.
        let latest_dispatch = kinds
            .iter()
            .rposition(|kind| *kind == EventKind::DispatchAttempted)
            .unwrap_or(kinds.len());
        let already_interrupted =
            kinds[latest_dispatch..].contains(&EventKind::TransportInterrupted);
        let evidence = TurnEvidence::replay_event_kinds(kinds)
            .map_err(|error| format!("turn {turn_id} replay failed: {error}"))?;

        let remote_still_live = !already_interrupted
            && matches!(
                evidence.remote,
                RemoteEvidence::Dispatching | RemoteEvidence::AcceptedObserved
            )
            && evidence.assistant != AssistantEvidence::CompletedObserved;

        if remote_still_live {
            interrupted.push(turn_id);
        }
    }

    let mut continuation_interrupted = Vec::new();
    for execution in replay_worker_continuation_execution_audit(store.events())? {
        let transport =
            continuation_execution_transport_state(store.events(), execution.execution_turn_id)?;
        if transport.was_dispatched() && !transport.is_terminal() {
            continuation_interrupted.push(execution.execution_turn_id);
        }
    }

    let mut coordination_interrupted = Vec::new();
    for coordination in replay_controller_coordination_audit(store.events())? {
        let transport = controller_coordination_transport_state(
            store.events(),
            coordination.coordination_turn_id,
        )?;
        if transport.was_dispatched() && !transport.is_terminal() {
            coordination_interrupted.push(coordination.coordination_turn_id);
        }
    }

    for turn_id in &interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this remote turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!("failed to persist restart interruption for turn {turn_id}: {error}")
            })?;
    }

    for turn_id in &continuation_interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this bounded continuation turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!(
                    "failed to persist restart interruption for continuation turn {turn_id}: {error}"
                )
            })?;
    }

    for turn_id in &coordination_interrupted {
        let request_id = turn_id.to_string();
        store
            .append_scoped(
                Some(local_turn_scope(*turn_id)),
                EventKind::TransportInterrupted,
                remote_turn_payload(
                    *turn_id,
                    &request_id,
                    None,
                    None,
                    Some(
                        "Chatarium restarted without a durable terminal outcome for this non-authored controller coordination turn",
                    ),
                ),
            )
            .map_err(|error| {
                format!(
                    "failed to persist restart interruption for controller coordination turn {turn_id}: {error}"
                )
            })?;
    }

    let continuation_turn_ids = replay_worker_continuation_execution_audit(store.events())?
        .into_iter()
        .map(|record| record.execution_turn_id)
        .collect::<Vec<_>>();
    for turn_id in continuation_turn_ids {
        append_worker_continuation_result_if_terminal(store, turn_id)?;
    }

    let coordination_turn_ids = replay_controller_coordination_audit(store.events())?
        .into_iter()
        .map(|record| record.coordination_turn_id)
        .collect::<Vec<_>>();
    for turn_id in coordination_turn_ids {
        append_controller_coordination_result_if_terminal(store, turn_id)?;
    }

    Ok(interrupted.len() + continuation_interrupted.len() + coordination_interrupted.len())
}
