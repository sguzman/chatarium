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
const ATTACHED_MESSAGE_COMMIT_VERSION: u64 = 2;

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
    commit_user_message_with_attachment(store, message, None)
}

/// Commit optional, exact, bounded attachment bytes atomically with the authored text.
/// The v2 envelope is used only when a file is present; old text-only v1 readers
/// keep their existing representation and event identity.
pub fn commit_user_message_with_attachment(
    store: &mut impl EventStore,
    message: &AuthoredUserMessage,
    attachment: Option<&Value>,
) -> std::io::Result<UserMessageCommitReceipt> {
    if let Some(value) = attachment {
        validate_attachment(value).map_err(invalid_data)?;
    }
    let mut envelope = json!({
        "schema": USER_MESSAGE_COMMIT_SCHEMA,
        "version": if attachment.is_some() { ATTACHED_MESSAGE_COMMIT_VERSION } else { USER_MESSAGE_COMMIT_VERSION },
        "conversation_id": message.conversation_id.to_string(),
        "turn_id": message.turn_id.to_string(),
        "message_id": message.message_id.to_string(),
        "text": message.text,
    });
    if let Some(attachment) = attachment {
        envelope["attachment"] = attachment.clone();
    }
    let payload = serde_json::to_string(&envelope).map_err(invalid_data)?;

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
    if version != USER_MESSAGE_COMMIT_VERSION && version != ATTACHED_MESSAGE_COMMIT_VERSION {
        return Err(format!("unsupported user-message commit version {version}"));
    }
    match (version, value.get("attachment")) {
        (ATTACHED_MESSAGE_COMMIT_VERSION, Some(attachment)) => validate_attachment(attachment)?,
        (ATTACHED_MESSAGE_COMMIT_VERSION, None) => {
            return Err("attached user-message commit has no attachment".to_owned());
        }
        (USER_MESSAGE_COMMIT_VERSION, Some(_)) => {
            return Err("text-only v1 commit unexpectedly includes attachment".to_owned());
        }
        _ => {}
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

/// Decode exact attachment bytes from an authored commit for verification or replay.
/// Do not include this value in diagnostics, search indexes, or display strings.
pub fn decode_user_message_attachment(event: &EventEnvelope) -> Result<Option<Value>, String> {
    if event.kind != EventKind::UserMessageCommitted {
        return Ok(None);
    }
    if !matches!(
        decode_user_message_commit(event)?,
        Some(DecodedUserMessageCommit::Typed(_))
    ) {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&event.payload)
        .map_err(|error| format!("cannot parse typed authored commit: {error}"))?;
    Ok(value.get("attachment").cloned())
}

fn validate_attachment(attachment: &Value) -> Result<(), String> {
    let object = attachment
        .as_object()
        .ok_or_else(|| "attachment record must be an object".to_owned())?;
    let filename = object
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(|| "attachment filename missing".to_owned())?;
    if filename.is_empty() || filename.len() > 128 || filename.chars().any(char::is_control) {
        return Err("attachment filename invalid".to_owned());
    }
    let byte_len = object
        .get("byte_len")
        .and_then(Value::as_u64)
        .ok_or_else(|| "attachment byte length missing".to_owned())?;
    if byte_len == 0 || byte_len > 128 * 1024 {
        return Err("attachment byte length outside local bounds".to_owned());
    }
    for field in [
        "prepared_at_unix_ms",
        "read_elapsed_ms",
        "encoding_elapsed_ms",
    ] {
        let numeric = object
            .get(field)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("attachment {field} missing"))?;
        if !numeric.is_finite() || numeric < 0.0 {
            return Err(format!("attachment {field} invalid"));
        }
    }
    let mime = object
        .get("mime")
        .and_then(Value::as_str)
        .ok_or_else(|| "attachment mime missing".to_owned())?;
    if !matches!(
        mime,
        "text/plain"
            | "text/csv"
            | "application/json"
            | "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "image/webp"
    ) {
        return Err("attachment mime not allowed".to_owned());
    }
    let content = object
        .get("content")
        .and_then(Value::as_object)
        .ok_or_else(|| "attachment content missing".to_owned())?;
    let (field, expected_kind) = if mime.starts_with("image/") {
        ("image_url", "input_image")
    } else {
        ("file_data", "input_file")
    };
    if content.get("type").and_then(Value::as_str) != Some(expected_kind) {
        return Err("attachment content kind mismatch".to_owned());
    }
    if expected_kind == "input_file"
        && content.get("filename").and_then(Value::as_str) != Some(filename)
    {
        return Err("attachment filename mismatch".to_owned());
    }
    let expected_mime = match filename
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ext) => match ext.as_str() {
            "txt" | "md" => "text/plain",
            "csv" => "text/csv",
            "json" => "application/json",
            "pdf" => "application/pdf",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            _ => return Err("attachment filename extension not allowed".to_owned()),
        },
        None => return Err("attachment requires a file extension".to_owned()),
    };
    if mime != expected_mime {
        return Err("attachment extension and MIME disagree".to_owned());
    }
    let payload = content
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| "attachment base64 source missing".to_owned())?;
    let prefix = format!("data:{mime};base64,");
    let encoded = payload
        .strip_prefix(&prefix)
        .ok_or_else(|| "attachment data URL MIME disagrees".to_owned())?;
    if payload.len() > 180_000 || encoded.len() != (byte_len as usize).div_ceil(3) * 4 {
        return Err("attachment encoded byte length invalid or oversized".to_owned());
    }
    let padding = if encoded.ends_with("==") {
        2
    } else if encoded.ends_with('=') {
        1
    } else {
        0
    };
    if encoded.len() / 4 * 3 - padding != byte_len as usize
        || !encoded
            .bytes()
            .take(encoded.len() - padding)
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/'))
        || !encoded
            .bytes()
            .skip(encoded.len() - padding)
            .all(|byte| byte == b'=')
    {
        return Err("attachment base64 payload malformed or length disagrees".to_owned());
    }
    Ok(())
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
    fn attached_commit_reopens_with_verified_bytes_and_exact_text() {
        let path = temp_path("attachment");
        let message = authored();
        let attachment = json!({
            "filename": "notes.txt",
            "mime": "text/plain",
            "byte_len": 3,
            "prepared_at_unix_ms": 123,
            "read_elapsed_ms": 0.2,
            "encoding_elapsed_ms": 0.1,
            "content": {
                "type": "input_file",
                "filename": "notes.txt",
                "file_data": "data:text/plain;base64,YWJj",
            },
        });
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            commit_user_message_with_attachment(&mut store, &message, Some(&attachment)).unwrap();
        }
        let store = JsonlEventStore::open(&path).unwrap();
        let event = &store.events()[0];
        assert_eq!(
            decode_user_message_commit(event).unwrap(),
            Some(DecodedUserMessageCommit::Typed(message))
        );
        assert_eq!(
            decode_user_message_attachment(event).unwrap(),
            Some(attachment)
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_or_unbounded_attachment_never_enters_journal() {
        let mut store = MemoryEventStore::default();
        let message = authored();
        let bad = json!({
            "filename": "bad.txt",
            "mime": "text/plain",
            "byte_len": 200_000,
            "prepared_at_unix_ms": 1,
            "read_elapsed_ms": 0.1,
            "encoding_elapsed_ms": 0.1,
            "content": {
                "type": "input_file",
                "filename": "bad.txt",
                "file_data": "data:text/plain;base64,YWJj",
            },
        });
        assert!(commit_user_message_with_attachment(&mut store, &message, Some(&bad)).is_err());
        assert!(store.events().is_empty());
    }

    #[test]
    fn forged_attachment_byte_lengths_and_mime_fail_closed() {
        let message = authored();
        let base = json!({
            "filename": "notes.txt",
            "mime": "text/plain",
            "byte_len": 3,
            "prepared_at_unix_ms": 1000,
            "read_elapsed_ms": 0.1,
            "encoding_elapsed_ms": 0.1,
            "content": {
                "type": "input_file",
                "filename": "notes.txt",
                "file_data": "data:text/plain;base64,YWJj"
            }
        });
        for changed in [
            ("byte_len", json!(2)),
            ("byte_len", json!(4)),
            ("mime", json!("application/pdf")),
        ] {
            let mut forged = base.clone();
            forged[changed.0] = changed.1;
            let mut store = MemoryEventStore::default();
            assert!(
                commit_user_message_with_attachment(&mut store, &message, Some(&forged)).is_err()
            );
            assert!(store.events().is_empty());
        }
        for corrupted in ["YWJ!", "YW=J", "YWJj="] {
            let mut forged = base.clone();
            forged["content"]["file_data"] = format!("data:text/plain;base64,{corrupted}");
            let mut store = MemoryEventStore::default();
            assert!(
                commit_user_message_with_attachment(&mut store, &message, Some(&forged)).is_err()
            );
            assert!(store.events().is_empty());
        }
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
