//! Durable audit of next-request-only local memory use.
//!
//! Pre-send selection is UI state. Once an authored turn commits, the exact
//! one-shot memory set used by that request becomes a durable turn-correlated
//! fact. This does not change persistent memory Admit/Exclude state.

use crate::authored::{DecodedUserMessageCommit, decode_user_message_commit, local_turn_scope};
use crate::local_memory_audit::{LocalMemoryArtifactRecord, replay_local_memory_audit};
use crate::local_memory_context_audit::replay_admitted_local_memory_context;
use crate::local_memory_supersession_audit::{
    replay_local_memory_supersession_audit, superseded_memory_ids,
};
use crate::{EventEnvelope, EventStore};
use chatarium_core::local_memory::LocalMemoryId;
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-memory-turn-selection-audit";
const VERSION: u64 = 1;
pub const MAX_ONE_SHOT_MEMORIES_PER_TURN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemoryTurnSelectionRecord {
    pub conversation_id: LocalConversationId,
    pub turn_id: LocalTurnId,
    pub memory_ids: Vec<LocalMemoryId>,
    pub selection_snapshot_after_sequence: u64,
    pub user_message_sequence: u64,
    pub recorded_sequence: u64,
}

pub fn record_local_memory_turn_selection(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    memory_ids: &[LocalMemoryId],
    selection_snapshot_after_sequence: u64,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(local_turn_scope(turn_id)),
        EventKind::LocalMemoryTurnSelectionRecorded,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "local_memory_turn_selection",
            "conversation_id": conversation_id.to_string(),
            "turn_id": turn_id.to_string(),
            "memory_ids": memory_ids.iter().map(|id| id.get()).collect::<Vec<_>>(),
            "selection_snapshot_after_sequence": selection_snapshot_after_sequence,
        }),
    )
}

/// Validate one exact one-shot selection against a frozen journal high-water mark.
///
/// Returned artifacts preserve the canonical ascending memory-id selection order.
pub fn validate_local_memory_one_shot_snapshot(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
    memory_ids: &[LocalMemoryId],
    selection_snapshot_after_sequence: u64,
) -> Result<Vec<LocalMemoryArtifactRecord>, String> {
    if memory_ids.is_empty() {
        return Err("one-shot local memory selection cannot be empty".to_owned());
    }
    if memory_ids.len() > MAX_ONE_SHOT_MEMORIES_PER_TURN {
        return Err(format!(
            "one-shot local memory selection has {} memories; maximum is {}",
            memory_ids.len(),
            MAX_ONE_SHOT_MEMORIES_PER_TURN
        ));
    }
    if memory_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(
            "one-shot local memory selection must contain strictly ascending unique memory ids"
                .to_owned(),
        );
    }

    let highest_sequence = events.last().map(|event| event.sequence).unwrap_or(0);
    if selection_snapshot_after_sequence > highest_sequence {
        return Err(format!(
            "one-shot local memory snapshot high-water {} exceeds available journal sequence {}",
            selection_snapshot_after_sequence, highest_sequence
        ));
    }

    let snapshot_end = events
        .iter()
        .take_while(|candidate| candidate.sequence <= selection_snapshot_after_sequence)
        .count();
    let snapshot = &events[..snapshot_end];

    let artifacts = replay_local_memory_audit(snapshot)?
        .into_iter()
        .map(|artifact| (artifact.memory_id, artifact))
        .collect::<BTreeMap<_, _>>();
    let superseded = superseded_memory_ids(&replay_local_memory_supersession_audit(snapshot)?);
    let admitted = replay_admitted_local_memory_context(snapshot, conversation_id)?
        .into_iter()
        .map(|record| record.memory_id)
        .collect::<BTreeSet<_>>();

    let mut selected = Vec::with_capacity(memory_ids.len());
    for memory_id in memory_ids {
        let artifact = artifacts.get(memory_id).ok_or_else(|| {
            format!(
                "one-shot local memory selection references memory {} absent from frozen snapshot through event #{}",
                memory_id.get(),
                selection_snapshot_after_sequence
            )
        })?;
        if superseded.contains(memory_id) {
            return Err(format!(
                "one-shot local memory selection references superseded memory {}",
                memory_id.get()
            ));
        }
        if admitted.contains(memory_id) {
            return Err(format!(
                "one-shot local memory selection redundantly selects persistently admitted memory {}",
                memory_id.get()
            ));
        }
        selected.push(artifact.clone());
    }

    Ok(selected)
}

pub fn replay_local_memory_turn_selection_audit(
    events: &[EventEnvelope],
) -> Result<Vec<LocalMemoryTurnSelectionRecord>, String> {
    let mut by_turn = BTreeMap::<LocalTurnId, LocalMemoryTurnSelectionRecord>::new();

    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::LocalMemoryTurnSelectionRecorded {
            continue;
        }
        let value = typed_payload(event)?;
        let conversation_id =
            LocalConversationId::from_str(required_string(&value, "conversation_id")?).map_err(
                |error| {
                    format!(
                        "local memory turn selection at sequence {} has invalid conversation id: {error}",
                        event.sequence
                    )
                },
            )?;
        let turn_id = LocalTurnId::from_str(required_string(&value, "turn_id")?).map_err(
            |error| {
                format!(
                    "local memory turn selection at sequence {} has invalid turn id: {error}",
                    event.sequence
                )
            },
        )?;
        let selection_snapshot_after_sequence =
            required_u64(&value, "selection_snapshot_after_sequence")?;
        let memory_ids = required_memory_ids(&value)?;
        validate_scope(event, turn_id)?;

        if by_turn.contains_key(&turn_id) {
            return Err(format!(
                "duplicate local memory turn selection for turn {} at sequence {}",
                turn_id, event.sequence
            ));
        }

        let prior = &events[..index];
        let mut committed = None;
        for candidate in prior {
            let Some(decoded) = decode_user_message_commit(candidate)? else {
                continue;
            };
            let DecodedUserMessageCommit::Typed(message) = decoded else {
                continue;
            };
            if message.turn_id == turn_id {
                committed = Some((message, candidate.sequence));
                break;
            }
        }
        let (message, user_message_sequence) = committed.ok_or_else(|| {
            format!(
                "local memory turn selection at sequence {} references turn {} before typed user-message commit",
                event.sequence, turn_id
            )
        })?;
        if message.conversation_id != conversation_id {
            return Err(format!(
                "local memory turn selection at sequence {} conversation {} disagrees with committed turn conversation {}",
                event.sequence, conversation_id, message.conversation_id
            ));
        }
        if selection_snapshot_after_sequence >= user_message_sequence {
            return Err(format!(
                "local memory turn selection at sequence {} snapshot high-water {} must predate user-message commit {}",
                event.sequence, selection_snapshot_after_sequence, user_message_sequence
            ));
        }

        validate_local_memory_one_shot_snapshot(
            prior,
            conversation_id,
            &memory_ids,
            selection_snapshot_after_sequence,
        )
        .map_err(|error| {
            format!(
                "local memory turn selection at sequence {} is invalid: {error}",
                event.sequence
            )
        })?;

        by_turn.insert(
            turn_id,
            LocalMemoryTurnSelectionRecord {
                conversation_id,
                turn_id,
                memory_ids,
                selection_snapshot_after_sequence,
                user_message_sequence,
                recorded_sequence: event.sequence,
            },
        );
    }

    let mut records = by_turn.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.recorded_sequence);
    Ok(records)
}

fn required_memory_ids(value: &Value) -> Result<Vec<LocalMemoryId>, String> {
    value
        .get("memory_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| "typed local memory turn selection is missing array field 'memory_ids'")?
        .iter()
        .map(|value| {
            value
                .as_u64()
                .map(LocalMemoryId::new)
                .ok_or_else(|| {
                    "typed local memory turn selection has non-integer memory id".to_owned()
                })
        })
        .collect()
}

fn append_typed(
    store: &mut impl EventStore,
    scope: Option<String>,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(scope, kind, encoded)
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed local memory turn selection at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "local memory turn selection at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "local memory turn selection at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some("local_memory_turn_selection") {
        return Err(format!(
            "local memory turn selection at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, turn_id: LocalTurnId) -> Result<(), String> {
    let expected = local_turn_scope(turn_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "local memory turn selection at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed local memory turn selection is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed local memory turn selection is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::authored::commit_user_message;
    use crate::local_memory_audit::record_local_memory_artifact;
    use crate::local_memory_context_audit::{
        LocalMemoryContextDecision, record_local_memory_context_decision,
    };
    use crate::local_memory_supersession_audit::record_local_memory_superseded;
    use chatarium_core::{AuthoredUserMessage, LocalMessageId};

    fn message(conversation_id: LocalConversationId, turn_id: LocalTurnId) -> AuthoredUserMessage {
        AuthoredUserMessage::new(
            conversation_id,
            turn_id,
            LocalMessageId::new(),
            "send",
        )
    }

    #[test]
    fn one_shot_selection_round_trips_against_frozen_snapshot() {
        let conversation = LocalConversationId::new();
        let turn_id = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(1),
            conversation,
            "memory one",
        )
        .unwrap();
        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(2),
            conversation,
            "memory two",
        )
        .unwrap();
        let high_water = store.events().last().unwrap().sequence;
        commit_user_message(&mut store, &message(conversation, turn_id)).unwrap();
        record_local_memory_turn_selection(
            &mut store,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1), LocalMemoryId::new(2)],
            high_water,
        )
        .unwrap();

        let records = replay_local_memory_turn_selection_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].turn_id, turn_id);
        assert_eq!(
            records[0].memory_ids,
            vec![LocalMemoryId::new(1), LocalMemoryId::new(2)]
        );
        assert_eq!(records[0].selection_snapshot_after_sequence, high_water);
    }

    #[test]
    fn later_supersession_does_not_rewrite_prior_one_shot_use() {
        let conversation = LocalConversationId::new();
        let turn_id = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(1), conversation, "old")
            .unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(2), conversation, "new")
            .unwrap();
        let high_water = store.events().last().unwrap().sequence;
        commit_user_message(&mut store, &message(conversation, turn_id)).unwrap();
        record_local_memory_turn_selection(
            &mut store,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1)],
            high_water,
        )
        .unwrap();
        record_local_memory_superseded(
            &mut store,
            LocalMemoryId::new(1),
            LocalMemoryId::new(2),
        )
        .unwrap();

        assert_eq!(
            replay_local_memory_turn_selection_audit(store.events())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn superseded_or_persistently_admitted_memory_cannot_be_one_shot_selected() {
        let conversation = LocalConversationId::new();

        let mut superseded = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut superseded,
            LocalMemoryId::new(1),
            conversation,
            "old",
        )
        .unwrap();
        record_local_memory_artifact(
            &mut superseded,
            LocalMemoryId::new(2),
            conversation,
            "new",
        )
        .unwrap();
        record_local_memory_superseded(
            &mut superseded,
            LocalMemoryId::new(1),
            LocalMemoryId::new(2),
        )
        .unwrap();
        let high_water = superseded.events().last().unwrap().sequence;
        let turn_id = LocalTurnId::new();
        commit_user_message(&mut superseded, &message(conversation, turn_id)).unwrap();
        record_local_memory_turn_selection(
            &mut superseded,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1)],
            high_water,
        )
        .unwrap();
        assert!(
            replay_local_memory_turn_selection_audit(superseded.events())
                .unwrap_err()
                .contains("superseded memory")
        );

        let mut admitted = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut admitted,
            LocalMemoryId::new(1),
            conversation,
            "always",
        )
        .unwrap();
        record_local_memory_context_decision(
            &mut admitted,
            LocalMemoryId::new(1),
            conversation,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap();
        let high_water = admitted.events().last().unwrap().sequence;
        let turn_id = LocalTurnId::new();
        commit_user_message(&mut admitted, &message(conversation, turn_id)).unwrap();
        record_local_memory_turn_selection(
            &mut admitted,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1)],
            high_water,
        )
        .unwrap();
        assert!(
            replay_local_memory_turn_selection_audit(admitted.events())
                .unwrap_err()
                .contains("persistently admitted")
        );
    }

    #[test]
    fn selection_requires_sorted_unique_bounded_ids_and_committed_turn() {
        let conversation = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(1), conversation, "one")
            .unwrap();
        let high_water = store.events().last().unwrap().sequence;
        let turn_id = LocalTurnId::new();
        record_local_memory_turn_selection(
            &mut store,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1)],
            high_water,
        )
        .unwrap();
        assert!(
            replay_local_memory_turn_selection_audit(store.events())
                .unwrap_err()
                .contains("before typed user-message commit")
        );

        let mut duplicate = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut duplicate,
            LocalMemoryId::new(1),
            conversation,
            "one",
        )
        .unwrap();
        let high_water = duplicate.events().last().unwrap().sequence;
        let turn_id = LocalTurnId::new();
        commit_user_message(&mut duplicate, &message(conversation, turn_id)).unwrap();
        record_local_memory_turn_selection(
            &mut duplicate,
            conversation,
            turn_id,
            &[LocalMemoryId::new(1), LocalMemoryId::new(1)],
            high_water,
        )
        .unwrap();
        assert!(
            replay_local_memory_turn_selection_audit(duplicate.events())
                .unwrap_err()
                .contains("strictly ascending")
        );
    }
}
