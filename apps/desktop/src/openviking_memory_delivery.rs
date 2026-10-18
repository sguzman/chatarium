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

#[cfg(test)]
mod tests {
    use super::*;

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
