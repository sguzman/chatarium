//! User-visible projection of a validated live C02 mirror page.
//!
//! Hidden/system/tool/reasoning content stays out of the visible transcript. The projection walks
//! the explicit parent chain from current_node rather than assuming array order is the active branch.

use chatarium_protocol::conversation_fetch::{
    ConversationFetchEnvelope, ConversationMessage, ConversationMessageContent,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTranscriptRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTranscriptMessage {
    pub remote_message_id: String,
    pub role: RemoteTranscriptRole,
    pub text: String,
    pub create_time: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTranscriptProjection {
    pub messages: Vec<RemoteTranscriptMessage>,
    /// True when the fetched page explicitly says older pages exist and the parent walk reaches
    /// beyond the messages in this page.
    pub truncated_before: bool,
}

pub fn project_remote_active_transcript(
    envelope: &ConversationFetchEnvelope,
) -> Result<RemoteTranscriptProjection, String> {
    let by_id = envelope
        .messages
        .iter()
        .map(|message| (message.id.as_str(), message))
        .collect::<BTreeMap<_, _>>();
    let mut current = envelope.current_node.as_str();
    let mut seen = BTreeSet::<String>::new();
    let mut path = Vec::<&ConversationMessage>::new();
    let mut truncated_before = false;

    loop {
        if !seen.insert(current.to_owned()) {
            return Err(format!(
                "remote conversation active branch contains a parent cycle at {current:?}"
            ));
        }
        let message = by_id.get(current).copied().ok_or_else(|| {
            format!(
                "remote conversation current/parent node {current:?} is missing from fetched page"
            )
        })?;
        path.push(message);

        let Some(parent) = message.parent_id.as_deref() else {
            break;
        };
        if by_id.contains_key(parent) {
            current = parent;
            continue;
        }
        if envelope.page_info.has_previous_page {
            truncated_before = true;
            break;
        }
        return Err(format!(
            "remote conversation active branch references missing parent {parent:?} without previous-page evidence"
        ));
    }

    path.reverse();
    let mut messages = Vec::new();
    for message in path {
        if message.weight == 0.0
            || message
                .metadata
                .get("is_visually_hidden_from_conversation")
                .and_then(Value::as_bool)
                == Some(true)
        {
            continue;
        }

        let role = match message.author.role.as_str() {
            "user" => RemoteTranscriptRole::User,
            "assistant" => RemoteTranscriptRole::Assistant,
            _ => continue,
        };
        let Some(text) = project_visible_content(&message.content) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }

        messages.push(RemoteTranscriptMessage {
            remote_message_id: message.id.clone(),
            role,
            text,
            create_time: message.create_time,
        });
    }

    Ok(RemoteTranscriptProjection {
        messages,
        truncated_before,
    })
}

fn project_visible_content(content: &ConversationMessageContent) -> Option<String> {
    match content {
        ConversationMessageContent::Thoughts { .. } | ConversationMessageContent::Opaque { .. } => {
            None
        }
        ConversationMessageContent::Parts { parts, .. } => {
            let mut projected = Vec::new();
            for part in parts {
                match part {
                    Value::String(text) if !text.is_empty() => projected.push(text.clone()),
                    Value::Object(object) => {
                        if let Some(text) = object
                            .get("text")
                            .and_then(Value::as_str)
                            .or_else(|| object.get("content").and_then(Value::as_str))
                        {
                            if !text.is_empty() {
                                projected.push(text.to_owned());
                            }
                        } else {
                            let content_type = object
                                .get("content_type")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            projected.push(format!("[non-text content: {content_type}]"));
                        }
                    }
                    Value::Null => {}
                    _ => projected.push("[unsupported content part]".to_owned()),
                }
            }
            Some(projected.join("\n"))
        }
        ConversationMessageContent::Content {
            content_type,
            content,
        } => match content {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            _ => Some(format!("[non-text content: {content_type}]")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_protocol::conversation_fetch::{
        ConversationAuthor, ConversationMessage, ConversationMessageContent, ConversationPageInfo,
    };
    use serde_json::{Map, json};

    fn message(
        id: &str,
        parent: Option<&str>,
        role: &str,
        content: ConversationMessageContent,
    ) -> ConversationMessage {
        ConversationMessage {
            id: id.to_owned(),
            author: ConversationAuthor {
                role: role.to_owned(),
                name: None,
                metadata: Map::new(),
            },
            create_time: 1.0,
            update_time: None,
            content,
            status: "finished_successfully".to_owned(),
            end_turn: Some(true),
            weight: 1.0,
            metadata: Map::new(),
            parent_id: parent.map(ToOwned::to_owned),
            recipient: "all".to_owned(),
            channel: None,
        }
    }

    fn parts(text: &str) -> ConversationMessageContent {
        ConversationMessageContent::Parts {
            content_type: "text".to_owned(),
            parts: vec![json!(text)],
        }
    }

    fn envelope(
        messages: Vec<ConversationMessage>,
        current_node: &str,
    ) -> ConversationFetchEnvelope {
        ConversationFetchEnvelope {
            conversation_id: "remote".to_owned(),
            title: "Live".to_owned(),
            create_time: 1.0,
            update_time: 2.0,
            messages,
            current_node: current_node.to_owned(),
            page_info: ConversationPageInfo {
                start_cursor: "start".to_owned(),
                end_cursor: "end".to_owned(),
                has_previous_page: false,
                has_next_page: false,
            },
        }
    }

    #[test]
    fn projects_active_parent_chain_and_hides_reasoning_tool_system_content() {
        let thoughts = ConversationMessageContent::Thoughts {
            content_type: "thoughts".to_owned(),
            thoughts: vec![json!({"summary": "must stay hidden"})],
            source_analysis_msg_id: "analysis".to_owned(),
        };
        let transcript = project_remote_active_transcript(&envelope(
            vec![
                message("root", None, "system", parts("system")),
                message("user", Some("root"), "user", parts("hello")),
                message("analysis", Some("user"), "assistant", thoughts),
                message(
                    "tool",
                    Some("analysis"),
                    "tool",
                    parts("private tool result"),
                ),
                message("final", Some("tool"), "assistant", parts("visible answer")),
                message(
                    "sibling",
                    Some("user"),
                    "assistant",
                    parts("abandoned branch"),
                ),
            ],
            "final",
        ))
        .unwrap();

        assert_eq!(transcript.messages.len(), 2);
        assert_eq!(transcript.messages[0].role, RemoteTranscriptRole::User);
        assert_eq!(transcript.messages[0].text, "hello");
        assert_eq!(transcript.messages[1].role, RemoteTranscriptRole::Assistant);
        assert_eq!(transcript.messages[1].text, "visible answer");
        assert!(!transcript.truncated_before);
    }

    #[test]
    fn opaque_current_revision_content_is_not_projected_as_visible_text() {
        let opaque = ConversationMessageContent::Opaque {
            content_type: "future_private_content".to_owned(),
            fields: serde_json::Map::from_iter([(
                "payload".to_owned(),
                json!({"shape": "unknown"}),
            )]),
        };
        let transcript = project_remote_active_transcript(&envelope(
            vec![
                message("root", None, "system", parts("system")),
                message("opaque", Some("root"), "assistant", opaque),
                message("final", Some("opaque"), "assistant", parts("visible")),
            ],
            "final",
        ))
        .unwrap();

        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].text, "visible");
    }

    #[test]
    fn missing_parent_is_only_accepted_when_previous_page_is_explicit() {
        let mut page = envelope(
            vec![message(
                "final",
                Some("older-page"),
                "assistant",
                parts("latest"),
            )],
            "final",
        );
        assert!(
            project_remote_active_transcript(&page)
                .unwrap_err()
                .contains("without previous-page evidence")
        );

        page.page_info.has_previous_page = true;
        let projected = project_remote_active_transcript(&page).unwrap();
        assert!(projected.truncated_before);
        assert_eq!(projected.messages.len(), 1);
    }

    #[test]
    fn parent_cycle_is_rejected() {
        let page = envelope(
            vec![
                message("a", Some("b"), "user", parts("a")),
                message("b", Some("a"), "assistant", parts("b")),
            ],
            "b",
        );
        assert!(
            project_remote_active_transcript(&page)
                .unwrap_err()
                .contains("parent cycle")
        );
    }
}
