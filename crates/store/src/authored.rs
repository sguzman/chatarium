//! Typed durable user-message commits.
//!
//! A user-message commit is purely local. It records exact authored text and local identities
//! before any remote mutation is allowed to depend on that text.

use crate::{EventEnvelope, EventStore};
use chatarium_core::{
    AuthoredUserMessage, EventKind, LocalConversationId, LocalMessageId, LocalTurnId,
};
use serde_json::{Value, json};
use std::str::FromStr;

const USER_MESSAGE_COMMIT_SCHEMA: &str = "chatarium-user-message-commit";
const USER_MESSAGE_COMMIT_VERSION: u64 = 1;

/// Successful durable local commit of one authored user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageCommitReceipt {
    /// Durable journal sequence assigned to the commit event.
    pub sequence: u64,
    /// Exact typed authored message that crossed the store durability boundary.
    pub message: AuthoredUserMessage,
}

/// Decoded historical user-message commit payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedUserMessageCommit {
    /// Versioned typed local authored-message payload.
    Typed(AuthoredUserMessage),
    /// Legacy journal payload containing only exact raw text and no typed local IDs.
    LegacyText(String),
}

/// Commit one exact authored user message through the supplied event store.
///
/// The helper performs no network operation. Concrete persistent stores define the durability
/// boundary; JsonlEventStore does not return from append until write, flush, and sync_data have
/// succeeded.
pub fn commit_user_message(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
) -> std::io::Result<UserMessageCommitReceipt> {
    let payload = serde_json::to_string(&json!({
        "schema": USER_MESSAGE_COMMIT_SCHEMA,
        "version": USER_MESSAGE_COMMIT_VERSION,
        "conversation_id": message.conversation_id.to_string(),
        "turn_id": message.turn_id.to_string(),
        "message_id": message.message_id.to_string(),
        "text": message.text,
    }))
    .map_err(invalid_data)?;

    let sequence = store.append_scoped(
        Some(local_turn_scope(message.turn_id)),
        EventKind::UserMessageCommitted,
        payload,
    )?;

    Ok(UserMessageCommitReceipt {
        sequence,
        message: message.clone(),
    })
}

/// Decode one durable user-message commit without inventing identities for legacy records.
///
/// Returns Ok(None) for non-user-message events.
pub fn decode_user_message_commit(
    event: &EventEnvelope,
) -> Result<Option<DecodedUserMessageCommit>, String> {
    if event.kind != EventKind::UserMessageCommitted {
        return Ok(None);
    }

    let Ok(value) = serde_json::from_str::<Value>(&event.payload) else {
        return Ok(Some(DecodedUserMessageCommit::LegacyText(
            event.payload.clone(),
        )));
    };

    if value.get("schema").and_then(Value::as_str) != Some(USER_MESSAGE_COMMIT_SCHEMA) {
        return Ok(Some(DecodedUserMessageCommit::LegacyText(
            event.payload.clone(),
        )));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "typed user-message commit is missing integer field 'version'".to_owned())?;
    if version != USER_MESSAGE_COMMIT_VERSION {
        return Err(format!("unsupported user-message commit version {version}"));
    }

    let conversation_id = parse_id::<LocalConversationId>(&value, "conversation_id")?;
    let turn_id = parse_id::<LocalTurnId>(&value, "turn_id")?;
    let message_id = parse_id::<LocalMessageId>(&value, "message_id")?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "typed user-message commit is missing string field 'text'".to_owned())?
        .to_owned();

    Ok(Some(DecodedUserMessageCommit::Typed(
        AuthoredUserMessage::new(conversation_id, turn_id, message_id, text),
    )))
}

/// Stable local turn scope used to group durable events for one locally authored turn.
#[must_use]
pub fn local_turn_scope(turn_id: LocalTurnId) -> String {
    format!("local-turn:{turn_id}")
}

fn parse_id<T>(value: &Value, field: &str) -> Result<T, String>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let raw = value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed user-message commit is missing string field '{field}'"))?;
    raw.parse::<T>()
        .map_err(|error| format!("invalid typed user-message commit field '{field}': {error}"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, JsonlEventStore, MemoryEventStore};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn authored() -> AuthoredUserMessage {
        AuthoredUserMessage::new(
            LocalConversationId::new(),
            LocalTurnId::new(),
            LocalMessageId::new(),
            " exact authored text\nwith spacing ",
        )
    }

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-authored-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn typed_commit_round_trips_exact_identity_and_text() {
        let mut store = MemoryEventStore::default();
        let message = authored();
        let receipt = commit_user_message(&mut store, &message).expect("commit");

        assert_eq!(receipt.sequence, 1);
        assert_eq!(receipt.message, message);
        let event = &store.events()[0];
        assert_eq!(event.sequence, 1);
        assert_eq!(
            event.scope.as_deref(),
            Some(local_turn_scope(message.turn_id).as_str())
        );

        assert_eq!(
            decode_user_message_commit(event).unwrap(),
            Some(DecodedUserMessageCommit::Typed(message))
        );
    }

    #[test]
    fn jsonl_commit_reopens_with_same_typed_payload() {
        let path = temp_path("reopen");
        let message = authored();
        {
            let mut store = JsonlEventStore::open(&path).expect("open");
            let receipt = commit_user_message(&mut store, &message).expect("commit");
            assert_eq!(receipt.sequence, 1);
        }

        let reopened = JsonlEventStore::open(&path).expect("reopen");
        assert_eq!(
            decode_user_message_commit(&reopened.events()[0]).unwrap(),
            Some(DecodedUserMessageCommit::Typed(message))
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_raw_text_commit_stays_legacy_without_invented_ids() {
        let mut store = MemoryEventStore::default();
        let legacy = r#"{"looks":"like json","but":"is old user text"}"#;
        store
            .append(EventKind::UserMessageCommitted, legacy.to_owned())
            .expect("legacy append");

        assert_eq!(
            decode_user_message_commit(&store.events()[0]).unwrap(),
            Some(DecodedUserMessageCommit::LegacyText(legacy.to_owned()))
        );
    }

    #[test]
    fn malformed_typed_commit_is_not_silently_downgraded_to_legacy() {
        let mut store = MemoryEventStore::default();
        store
            .append(
                EventKind::UserMessageCommitted,
                serde_json::to_string(&json!({
                    "schema": USER_MESSAGE_COMMIT_SCHEMA,
                    "version": 1,
                    "conversation_id": "not-a-uuid",
                    "turn_id": LocalTurnId::new().to_string(),
                    "message_id": LocalMessageId::new().to_string(),
                    "text": "x",
                }))
                .unwrap(),
            )
            .expect("append");

        let error =
            decode_user_message_commit(&store.events()[0]).expect_err("typed corruption must fail");
        assert!(error.contains("conversation_id"));
    }

    #[test]
    fn unrelated_event_is_not_a_user_message_commit() {
        let event = EventEnvelope {
            sequence: 1,
            at_unix_ms: 1,
            scope: None,
            kind: EventKind::DraftChanged,
            payload: "draft".to_owned(),
        };
        assert_eq!(decode_user_message_commit(&event).unwrap(), None);
    }
}
