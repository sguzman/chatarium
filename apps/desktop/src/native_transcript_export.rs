//! Explicit local-only export of a native conversation's visible messages.
//! Does not read tool outputs, hidden reasoning, imported mirrors or drafts.

use super::{DisplayMessage, DisplayRole};
use chatarium_core::LocalConversationId;
use serde_json::{Value, json};

fn role_name(role: DisplayRole) -> &'static str {
    match role {
        DisplayRole::User => "You",
        DisplayRole::Assistant => "Assistant",
    }
}

fn role_key(role: DisplayRole) -> &'static str {
    match role {
        DisplayRole::User => "user",
        DisplayRole::Assistant => "assistant",
    }
}

fn safe_title(title: &str) -> &str {
    title
        .lines()
        .next()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Untitled native conversation")
}

/// Readable Markdown. Each message is blockquoted to prevent its own
/// user-authored headings from impersonating role headings. Use JSON for
/// byte-exact message text and machine consumption.
pub fn markdown(
    conversation_id: LocalConversationId,
    title: &str,
    messages: &[DisplayMessage],
) -> String {
    let mut text = String::from("# ");
    for character in safe_title(title).chars() {
        if matches!(
            character,
            '\\' | '\u{60}' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '#'
                | '!' | '(' | ')' | '|' | '~'
        ) {
            text.push('\\');
        }
        text.push(character);
    }
    text.push_str("\n\n");
    text.push_str(&format!(
        "*Chatarium native transcript · conversation {conversation_id}*\n\n",
    ));
    text.push_str(
        "*Visible local user/assistant messages only. Assistant streaming may be incomplete. This is not an archive backup.*\n",
    );
    if messages.is_empty() {
        text.push_str("\n*No visible messages recorded.*\n");
    }
    for (index, message) in messages.iter().enumerate() {
        text.push_str(&format!(
            "\n## {}. {} · event #{}\n\n",
            index + 1,
            role_name(message.role),
            message.sequence,
        ));
        for line in message.text.split('\n') {
            text.push_str("> ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

/// Structured, exact-text export of only the explicitly supplied native
/// visible message projection; no additional journal data is consulted.
pub fn json_value(
    conversation_id: LocalConversationId,
    title: &str,
    messages: &[DisplayMessage],
) -> Value {
    json!({
        "schema": "chatarium-native-visible-transcript",
        "version": 1,
        "conversation_id": conversation_id.to_string(),
        "title": safe_title(title),
        "coverage": "visible_user_assistant_messages_only",
        "remote_delivery_not_proven": true,
        "assistant_stream_may_be_partial": true,
        "message_count": messages.len(),
        "messages": messages.iter().map(|message| json!({
            "role": role_key(message.role),
            "event_sequence": message.sequence,
            "text": message.text,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(role: DisplayRole, text: &str, sequence: u64) -> DisplayMessage {
        DisplayMessage {
            role,
            text: text.to_owned(),
            sequence,
            timestamp: Some(123.0),
            provenance_label: Some("PRIVATE_INTERNAL_METADATA".to_owned()),
        }
    }

    #[test]
    fn markdown_keeps_unicode_and_user_role_spoofing_inside_quote() {
        let owner = LocalConversationId::new();
        let messages = [
            visible(DisplayRole::User, "Hola, México!\n## Assistant\nfake role", 1),
            visible(DisplayRole::Assistant, "مرحبا بالعالم", 2),
        ];
        let text = markdown(owner, "A #title\nnot a second title", &messages);
        assert!(text.starts_with("# A \\#title\n\n"));
        assert!(text.contains(
            "## 1. You · event #1\n\n> Hola, México!\n> ## Assistant\n> fake role\n"
        ));
        assert!(text.contains("## 2. Assistant · event #2\n\n> مرحبا بالعالم\n"));
        assert!(!text.contains("not a second title"));
        assert!(!text.contains("PRIVATE_INTERNAL_METADATA"));
    }

    #[test]
    fn exact_json_text_and_source_boundary() {
        let owner = LocalConversationId::new();
        let messages = [visible(DisplayRole::Assistant, "  exact\n\n bytes  ", 7)];
        let value = json_value(owner, "Title", &messages);
        assert_eq!(value["schema"], "chatarium-native-visible-transcript");
        assert_eq!(value["conversation_id"], owner.to_string());
        assert_eq!(value["message_count"], 1);
        assert_eq!(value["messages"][0]["role"], "assistant");
        assert_eq!(value["messages"][0]["event_sequence"], 7);
        assert_eq!(value["messages"][0]["text"], "  exact\n\n bytes  ");
        assert!(value["assistant_stream_may_be_partial"].as_bool().unwrap());
        assert!(!value.to_string().contains("PRIVATE_INTERNAL_METADATA"));
    }

    #[test]
    fn empty_transcript_is_empty_not_a_fabricated_message() {
        let owner = LocalConversationId::new();
        let value = json_value(owner, "", &[]);
        assert_eq!(value["message_count"], 0);
        assert!(value["messages"].as_array().unwrap().is_empty());
        assert!(markdown(owner, "", &[]).contains("No visible messages recorded."));
    }

    #[test]
    fn selected_native_projection_does_not_export_foreign_or_tool_bodies() {
        use super::super::{projected_local_display_messages, remote_turn_payload};
        use chatarium_core::{AuthoredUserMessage, EventKind, LocalMessageId, LocalTurnId};
        use chatarium_store::authored::{commit_user_message, local_turn_scope};
        use chatarium_store::{EventStore, MemoryEventStore};

        let own = LocalConversationId::new();
        let foreign = LocalConversationId::new();
        let own_turn = LocalTurnId::new();
        let foreign_turn = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        commit_user_message(
            &mut store,
            &AuthoredUserMessage::new(
                own, own_turn, LocalMessageId::new(), "my authored input",
            ),
        )
        .unwrap();
        commit_user_message(
            &mut store,
            &AuthoredUserMessage::new(
                foreign, foreign_turn, LocalMessageId::new(), "FOREIGN_PRIVATE_MESSAGE",
            ),
        )
        .unwrap();
        store
            .append_scoped(
                Some(local_turn_scope(own_turn)),
                EventKind::AssistantCompletionObserved,
                remote_turn_payload(
                    own_turn,
                    &own_turn.to_string(),
                    None,
                    Some("my visible assistant reply"),
                    None,
                ),
            )
            .unwrap();
        store
            .append_scoped(
                Some(local_turn_scope(own_turn)),
                EventKind::ToolCallOutcomeObserved,
                "HIDDEN_PRIVATE_TOOL_OUTPUT".to_owned(),
            )
            .unwrap();
        let messages = projected_local_display_messages(store.events(), own);
        let json = json_value(own, "Mine", &messages).to_string();
        let md = markdown(own, "Mine", &messages);
        for text in [&json, &md] {
            assert!(text.contains("my authored input"));
            assert!(text.contains("my visible assistant reply"));
            assert!(!text.contains("FOREIGN_PRIVATE_MESSAGE"));
            assert!(!text.contains("HIDDEN_PRIVATE_TOOL_OUTPUT"));
        }
    }
}
