//! Host-owned at-most-once OpenViking writes and strict read-back checks.
//! Never call this on the egui thread or journal worker. The caller must
//! persist an outbound "dispatched" claim before invoking this function.
use crate::local_http_mcp;
use serde_json::{Value, json};

const MAX_CHECKPOINT_BYTES: usize = 16384;

pub fn checkpoint_uri(conversation: &str, turn: &str) -> Result<String, String> {
    fn valid(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
    if !valid(conversation) || !valid(turn) {
        return Err("invalid OpenViking checkpoint identity".to_owned());
    }
    Ok(format!(
        "viking://~/chatarium/conversations/{conversation}/checkpoints/{turn}.md"
    ))
}

/// OpenViking v0.5.0 read(uris=<one>, limit=<lines>) returns one text block.
/// An MCP write tool saying "Wrote X bytes" is not sufficient confirmation.
pub fn readback_matches(value: &Value, expected: &str) -> bool {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return false;
    };
    content.len() == 1
        && content[0].get("type").and_then(Value::as_str) == Some("text")
        && content[0].get("text").and_then(Value::as_str) == Some(expected)
        && value.get("isError") != Some(&Value::Bool(true))
}

pub fn deliver_create_once(conversation: &str, turn: &str, content: &str) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_CHECKPOINT_BYTES {
        return Err("OpenViking checkpoint content is empty or too large".to_owned());
    }
    let uri = checkpoint_uri(conversation, turn)?;
    let identity = format!("memory-create:{conversation}:{turn}");
    local_http_mcp::invoke(
        conversation,
        &identity,
        local_http_mcp::OPENVIKING,
        "write",
        &json!({
            "uri": uri,
            "content": content,
            "mode": "create",
            "wait": false,
        }),
    )?;
    // Verification is a read, never a second write. Failed or unconfirmed
    // verification makes the outbound operation uncertain.
    let readback = local_http_mcp::invoke(
        conversation,
        &format!("memory-verify:{conversation}:{turn}"),
        local_http_mcp::OPENVIKING,
        "read",
        &json!({"uris": uri, "limit": 100}),
    )?;
    if !readback_matches(&readback, content) {
        return Err("OpenViking write was attempted but exact read-back is unconfirmed".to_owned());
    }
    Ok(())
}

/// A crash-uncertain write may already exist remotely. Read, compare with
/// the immutable local source, and NEVER retry its create operation.
pub fn verify_uncertain_read_only(
    outbox_path: &std::path::Path,
    conversation: &str,
    turn: &str,
    completed_sequence: u64,
) -> Result<bool, String> {
    verify_uncertain_with_read(outbox_path, conversation, turn, completed_sequence, |uri| {
        local_http_mcp::invoke(
            conversation,
            &format!("memory-recover-read:{conversation}:{turn}"),
            local_http_mcp::OPENVIKING,
            "read",
            &json!({"uris": uri, "limit": 100}),
        )
    })
}

fn verify_uncertain_with_read(
    outbox_path: &std::path::Path,
    conversation: &str,
    turn: &str,
    completed_sequence: u64,
    read: impl FnOnce(&str) -> Result<Value, String>,
) -> Result<bool, String> {
    let uri = checkpoint_uri(conversation, turn)?;
    if completed_sequence == 0 {
        return Err("invalid uncertain checkpoint source sequence".to_owned());
    }
    let file =
        crate::openviking_checkpoint_outbox::Outbox::summary_path(outbox_path, conversation, turn)?;
    let size = std::fs::metadata(&file)
        .map_err(|e| format!("uncertain checkpoint summary metadata: {e}"))?
        .len();
    if size == 0 || size > MAX_CHECKPOINT_BYTES as u64 {
        return Err("uncertain checkpoint summary size invalid".to_owned());
    }
    let expected = std::fs::read_to_string(&file)
        .map_err(|e| format!("uncertain checkpoint source unreadable: {e}"))?;
    if expected.len() != size as usize
        || !expected.starts_with(&format!(
            "# Chatarium memory checkpoint\n\nConversation: {conversation}\nTurn: {turn}\nSource journal sequence: {completed_sequence}\n\n"
        ))
    {
        return Err("uncertain checkpoint source identity mismatch".to_owned());
    }
    read(&uri).map(|remote| readback_matches(&remote, &expected))
}

/// A verified, in-scope checkpoint. This is evidence for later context admission,
/// not a system/developer message or a replacement for the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievedCheckpoint {
    pub source_conversation_id: String,
    pub source_turn_id: String,
    pub source_journal_sequence: u64,
    pub uri: String,
    pub text: String,
}

const MAX_RETRIEVAL_ITEMS: usize = 3;
const MAX_RETRIEVAL_BYTES: usize = 12 * 1024;

/// Select recent confirmed checkpoints from exactly one conversation. No
/// implicit global search, shared namespace, or cross-conversation access.
fn confirmed_sources<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    conversation: &str,
) -> Result<Vec<&'a crate::openviking_checkpoint_outbox::Entry>, String> {
    // Reuse the same strict path identity validation as the writer.
    checkpoint_uri(conversation, "scope-check")?;
    let mut entries: Vec<_> = outbox
        .entries()
        .iter()
        .filter(|entry| {
            entry.conversation_id == conversation
                && entry.state == crate::openviking_checkpoint_outbox::State::Confirmed
        })
        .collect();
    entries.sort_by(|a, b| {
        b.completed_sequence
            .cmp(&a.completed_sequence)
            .then_with(|| b.turn_id.cmp(&a.turn_id))
    });
    entries.truncate(MAX_RETRIEVAL_ITEMS);
    Ok(entries)
}

/// Retrieve recent, locally confirmed memories for a single owning conversation.
/// Must run off the UI thread. On an offline service or ambiguous read, returns
/// an error; the caller must proceed without external memory, not invent it.
///
/// The persisted local summary is the expected immutable evidence. A remote
/// response is admitted only on an exact match, with no truncation. Cross-chat
/// or cross-project discovery requires a separately authorized scope mechanism.
pub fn retrieve_confirmed_for_conversation(
    outbox_path: &std::path::Path,
    conversation: &str,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    use crate::openviking_checkpoint_outbox::Outbox;
    let outbox = Outbox::load(outbox_path)?;
    let sources = confirmed_sources(&outbox, conversation)?;
    let mut used_bytes = 0usize;
    let mut retrieved = Vec::with_capacity(sources.len());

    for entry in sources {
        let file = Outbox::summary_path(outbox_path, conversation, &entry.turn_id)?;
        let size = std::fs::metadata(&file)
            .map_err(|e| format!("checkpoint summary metadata unavailable: {e}"))?
            .len();
        if size == 0 || size > MAX_CHECKPOINT_BYTES as u64 {
            return Err("confirmed checkpoint has invalid local summary size".to_owned());
        }
        // Never partially inject a checkpoint that does not fit the budget.
        if size > (MAX_RETRIEVAL_BYTES - used_bytes) as u64 {
            break;
        }
        let expected = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read confirmed checkpoint summary: {e}"))?;
        if expected.is_empty() || expected.len() != size as usize {
            return Err("confirmed checkpoint summary changed while reading".to_owned());
        }
        let uri = checkpoint_uri(conversation, &entry.turn_id)?;
        let response = local_http_mcp::invoke(
            conversation,
            &format!("memory-retrieve:{conversation}:{}", entry.turn_id),
            local_http_mcp::OPENVIKING,
            "read",
            &json!({"uris": uri, "limit": 100}),
        )?;
        if !readback_matches(&response, &expected) {
            return Err(
                "confirmed OpenViking memory did not match its local durable source".to_owned(),
            );
        }
        used_bytes += expected.len();
        retrieved.push(RetrievedCheckpoint {
            source_conversation_id: conversation.to_owned(),
            source_turn_id: entry.turn_id.clone(),
            source_journal_sequence: entry.completed_sequence,
            uri,
            text: expected,
        });
    }
    Ok(retrieved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_scopes_to_owner_and_confirmed_entries_only() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-memory-retrieval-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        for (owner, turn, seq, confirm) in [
            ("owner", "older", 2, true),
            ("foreign", "secret", 3, true),
            ("owner", "unconfirmed", 4, false),
            ("owner", "newer", 5, true),
        ] {
            outbox.enqueue_durable(&path, owner, turn, seq).unwrap();
            outbox
                .save_summary_durable(&path, owner, turn, seq, "checkpoint")
                .unwrap();
            if confirm {
                outbox
                    .transition_durable(&path, owner, turn, seq, State::Ready, State::Dispatched)
                    .unwrap();
                outbox
                    .transition_durable(
                        &path,
                        owner,
                        turn,
                        seq,
                        State::Dispatched,
                        State::Confirmed,
                    )
                    .unwrap();
            }
        }
        let selected = confirmed_sources(&outbox, "owner").unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|item| item.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
        assert!(
            confirmed_sources(&outbox, "foreign")
                .unwrap()
                .iter()
                .all(|item| item.conversation_id == "foreign")
        );
        assert!(confirmed_sources(&outbox, "../escape").is_err());
        assert!(confirmed_sources(&outbox, "").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selection_is_bounded_to_latest_three_confirmed_sources() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-retrieval-cap-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        for n in 1..=5 {
            let turn = format!("turn-{n}");
            outbox.enqueue_durable(&path, "owner", &turn, n).unwrap();
            outbox
                .save_summary_durable(&path, "owner", &turn, n, "short checkpoint")
                .unwrap();
            outbox
                .transition_durable(&path, "owner", &turn, n, State::Ready, State::Dispatched)
                .unwrap();
            outbox
                .transition_durable(
                    &path,
                    "owner",
                    &turn,
                    n,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
        }
        assert_eq!(
            confirmed_sources(&outbox, "owner")
                .unwrap()
                .iter()
                .map(|item| item.completed_sequence)
                .collect::<Vec<_>>(),
            vec![5, 4, 3]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn uncertain_checkpoint_recovery_reads_only_and_confirms_exact_bytes() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-readonly-memory-recovery-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        outbox
            .enqueue_durable(&path, "owner", "turn-1", 12)
            .unwrap();
        outbox
            .save_summary_durable(&path, "owner", "turn-1", 12, "Durable fact")
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                "turn-1",
                12,
                State::Ready,
                State::Dispatched,
            )
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                "turn-1",
                12,
                State::Dispatched,
                State::Uncertain,
            )
            .unwrap();
        let source = Outbox::summary_path(&path, "owner", "turn-1").unwrap();
        let content = std::fs::read_to_string(&source).unwrap();
        assert!(
            verify_uncertain_with_read(&path, "owner", "turn-1", 12, |uri| {
                assert_eq!(
                    uri,
                    "viking://~/chatarium/conversations/owner/checkpoints/turn-1.md"
                );
                Ok(json!({"content":[{"type":"text","text":content}]}))
            })
            .unwrap()
        );
        assert!(
            !verify_uncertain_with_read(&path, "owner", "turn-1", 12, |_| {
                Ok(json!({"content":[{"type":"text","text":"different"}]}))
            })
            .unwrap()
        );
        assert!(
            verify_uncertain_with_read(&path, "foreign", "turn-1", 12, |_| {
                panic!("remote read must not run for foreign source")
            })
            .is_err()
        );
        std::fs::write(&source, "tampered body").unwrap();
        assert!(
            verify_uncertain_with_read(&path, "owner", "turn-1", 12, |_| {
                panic!("remote read must not run for corrupted source")
            })
            .is_err()
        );
        assert!(
            outbox
                .transition_durable(
                    &path,
                    "owner",
                    "turn-1",
                    12,
                    State::Uncertain,
                    State::Confirmed
                )
                .unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exact_readback_only_verifies_same_content() {
        let ok = json!({"isError": false, "content": [{"type": "text", "text": "stored"}]});
        assert!(readback_matches(&ok, "stored"));
        assert!(!readback_matches(&ok, "other"));
        assert!(!readback_matches(
            &json!({"isError": true, "content": [{"type":"text","text":"stored"}]}),
            "stored"
        ));
        assert!(!readback_matches(&json!({"content":[]}), "stored"));
        assert!(!readback_matches(
            &json!({"content":[{"type":"text","text":"stored"},{"type":"text","text":"stored"}]}),
            "stored"
        ));
    }

    #[test]
    fn memory_paths_are_private_and_no_traversal_or_foreign_identity_is_permitted() {
        assert_eq!(
            checkpoint_uri("conversation-1", "turn-1").unwrap(),
            "viking://~/chatarium/conversations/conversation-1/checkpoints/turn-1.md"
        );
        assert!(checkpoint_uri("../escape", "turn-1").is_err());
        assert!(checkpoint_uri("conversation-1", "evil/turn").is_err());
    }
}
