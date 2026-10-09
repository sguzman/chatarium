//! Search semantics for native Chatarium conversations (not imported remote mirrors).
//! Search only locally projected user/assistant text and the local workspace title.
//! The journal and metadata catalog remain authoritative; the query is ephemeral.

use super::{DisplayMessage, projected_display_messages};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_store::EventEnvelope;
use chatarium_store::authored::{
    DecodedUserMessageCommit, decode_user_message_commit, local_turn_scope,
};
use eframe::egui;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSearchMatch {
    Title,
    Message,
}

/// Return the first reason a local conversation matches a case-insensitive
/// trimmed query. Empty search preserves the normal conversation list.
/// A query never crosses into a different conversation's projected messages.
pub fn find_match<'a>(
    query: &str,
    title: &str,
    visible_messages: impl IntoIterator<Item = &'a str>,
) -> Option<NativeSearchMatch> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || title.to_lowercase().contains(&needle) {
        return Some(NativeSearchMatch::Title);
    }
    visible_messages
        .into_iter()
        .any(|message| message.to_lowercase().contains(&needle))
        .then_some(NativeSearchMatch::Message)
}

/// Select a native conversation by keyboard result position. The same
/// activation contract applies to Enter with no highlighted row (first
/// result) and to a stale index after the filter changed.
pub fn activate_selection(
    matches: &[LocalConversationId],
    selection: Option<usize>,
) -> Option<LocalConversationId> {
    selection
        .and_then(|index| matches.get(index))
        .or_else(|| matches.first())
        .copied()
}

/// Opening a native message hit can seed the local transcript reader's
/// existing search/highlight controls. Titles and empty queries never claim
/// a matching message.
pub fn reader_query_for_result(query: &str, kind: NativeSearchMatch) -> Option<String> {
    if kind != NativeSearchMatch::Message {
        return None;
    }
    let query = query.trim();
    (!query.is_empty()).then(|| query.to_owned())
}

/// A bounded preview from only the selected conversation's projected
/// user/assistant messages. Uses the same UTF-8-safe local archive excerpt.
pub fn matching_message_preview<'a>(
    messages: impl IntoIterator<Item = &'a str>,
    query: &str,
) -> Option<String> {
    let folded = query.trim().to_lowercase();
    if folded.is_empty() {
        return None;
    }
    messages
        .into_iter()
        .find_map(|message| super::local_archive_search::local_snippet(message, &folded))
}

// The index reuses the same typed authored ownership and display-message
// projection as the native conversation view, but scans journal history only
// once rather than once per visible sidebar row, and caches unchanged frames.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JournalRevision {
    event_count: usize,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
    last_scope: Option<String>,
    last_kind: Option<EventKind>,
    last_payload_bytes: Option<usize>,
}

impl JournalRevision {
    fn capture(events: &[EventEnvelope]) -> Self {
        Self {
            event_count: events.len(),
            first_sequence: events.first().map(|event| event.sequence),
            last_sequence: events.last().map(|event| event.sequence),
            last_scope: events.last().and_then(|event| event.scope.clone()),
            last_kind: events.last().map(|event| event.kind),
            last_payload_bytes: events.last().map(|event| event.payload.len()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NativeConversationSearchIndex {
    revision: JournalRevision,
    messages: BTreeMap<LocalConversationId, Vec<DisplayMessage>>,
}

impl NativeConversationSearchIndex {
    pub fn build(events: &[EventEnvelope]) -> Self {
        let mut owners = BTreeMap::new();
        for event in events {
            if let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                decode_user_message_commit(event)
            {
                // Only typed authored user turns establish a conversation's
                // native assistant scope. The tool and controller journals
                // never become native searchable message bodies.
                owners.insert(local_turn_scope(message.turn_id), message.conversation_id);
            }
        }

        let mut by_conversation: BTreeMap<LocalConversationId, Vec<EventEnvelope>> =
            BTreeMap::new();
        for event in events {
            let owner = match event.kind {
                EventKind::UserMessageCommitted => match decode_user_message_commit(event) {
                    Ok(Some(DecodedUserMessageCommit::Typed(message))) => {
                        Some(message.conversation_id)
                    }
                    _ => None,
                },
                EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => {
                    event
                        .scope
                        .as_deref()
                        .and_then(|scope| owners.get(scope))
                        .copied()
                }
                _ => None,
            };
            if let Some(owner) = owner {
                by_conversation
                    .entry(owner)
                    .or_default()
                    .push(event.clone());
            }
        }

        let messages = by_conversation
            .into_iter()
            .map(|(owner, own_events)| (owner, projected_display_messages(&own_events)))
            .collect();
        Self {
            revision: JournalRevision::capture(events),
            messages,
        }
    }

    pub fn messages(&self, conversation_id: LocalConversationId) -> &[DisplayMessage] {
        self.messages
            .get(&conversation_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn matches_journal(&self, events: &[EventEnvelope]) -> bool {
        self.revision == JournalRevision::capture(events)
    }
}

/// Ephemeral egui-frame projection; journal events and owning conversation
/// metadata remain authoritative. No search query or body is persisted here.
pub fn cached_index(
    ctx: &egui::Context,
    events: &[EventEnvelope],
) -> Arc<NativeConversationSearchIndex> {
    let id = egui::Id::new("chatarium-native-conversation-search-index");
    if let Some(existing) =
        ctx.data_mut(|data| data.get_temp::<Arc<NativeConversationSearchIndex>>(id))
    {
        if existing.matches_journal(events) {
            return existing;
        }
    }
    let index = Arc::new(NativeConversationSearchIndex::build(events));
    ctx.data_mut(|data| data.insert_temp(id, Arc::clone(&index)));
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author(
        store: &mut impl chatarium_store::EventStore,
        owner: LocalConversationId,
        text: &str,
    ) -> chatarium_core::LocalTurnId {
        use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
        let turn = LocalTurnId::new();
        chatarium_store::authored::commit_user_message(
            store,
            &AuthoredUserMessage::new(owner, turn, LocalMessageId::new(), text),
        )
        .unwrap();
        turn
    }

    #[test]
    fn native_index_matches_reference_projection_and_isolates_other_conversations() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let first_turn = author(&mut store, first, "First owner mentions RUST");
        let second_turn = author(&mut store, second, "Other owner mentions PYTHON");
        let assistant = |turn: chatarium_core::LocalTurnId, body: &str| {
            serde_json::json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "text": body,
                "details": {
                    "local_turn_id": turn.to_string(),
                    "request_id": turn.to_string(),
                }
            })
            .to_string()
        };
        store
            .append_scoped(
                Some(local_turn_scope(first_turn)),
                EventKind::AssistantSnapshotObserved,
                assistant(first_turn, "partial answer"),
            )
            .unwrap();
        store
            .append_scoped(
                Some(local_turn_scope(second_turn)),
                EventKind::AssistantCompletionObserved,
                assistant(second_turn, "second answer private to second"),
            )
            .unwrap();
        store
            .append_scoped(
                Some(local_turn_scope(first_turn)),
                EventKind::AssistantCompletionObserved,
                assistant(first_turn, "completed first answer"),
            )
            .unwrap();
        // An event with a legitimate assistant scope but a forbidden kind
        // must never be indexed as a native conversation message.
        store
            .append_scoped(
                Some(local_turn_scope(first_turn)),
                EventKind::ToolCallOutcomeObserved,
                "SECRET RESULT NOT A CHAT MESSAGE".to_owned(),
            )
            .unwrap();
        let index = NativeConversationSearchIndex::build(store.events());
        for owner in [first, second] {
            let reference = super::super::projected_local_display_messages(store.events(), owner);
            let actual = index.messages(owner);
            assert_eq!(actual.len(), reference.len());
            assert_eq!(
                actual.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
                reference
                    .iter()
                    .map(|m| m.text.as_str())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                actual.iter().map(|m| m.sequence).collect::<Vec<_>>(),
                reference.iter().map(|m| m.sequence).collect::<Vec<_>>()
            );
        }
        let first_text = index
            .messages(first)
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(first_text.contains("RUST"));
        assert!(first_text.contains("completed first answer"));
        assert!(!first_text.contains("partial answer"));
        assert!(!first_text.contains("second answer private"));
        assert!(!first_text.contains("SECRET RESULT"));
        assert_eq!(
            find_match(
                "RUST",
                "",
                index.messages(first).iter().map(|m| m.text.as_str())
            ),
            Some(NativeSearchMatch::Message)
        );
        assert_eq!(
            find_match(
                "RUST",
                "",
                index.messages(second).iter().map(|m| m.text.as_str())
            ),
            None
        );
    }

    #[test]
    fn native_index_empty_and_journal_append_revision_are_distinct() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let empty = NativeConversationSearchIndex::build(store.events());
        assert!(empty.messages(owner).is_empty());
        assert!(empty.matches_journal(store.events()));
        author(&mut store, owner, "A newly durable chat");
        assert!(!empty.matches_journal(store.events()));
        let updated = NativeConversationSearchIndex::build(store.events());
        assert!(updated.matches_journal(store.events()));
        assert_eq!(updated.messages(owner).len(), 1);
    }

    #[test]
    fn cached_native_index_reuses_unchanged_journal_then_rebuilds_on_append() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let ctx = egui::Context::default();
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let first = cached_index(&ctx, store.events());
        let same = cached_index(&ctx, store.events());
        assert!(Arc::ptr_eq(&first, &same));
        author(&mut store, owner, "new message");
        let later = cached_index(&ctx, store.events());
        assert!(!Arc::ptr_eq(&first, &later));
        assert_eq!(later.messages(owner).len(), 1);
    }

    #[test]
    fn title_matches_do_not_invent_reader_hits_but_message_matches_can_jump() {
        assert_eq!(
            reader_query_for_result("  México  ", NativeSearchMatch::Message),
            Some("México".to_owned())
        );
        assert_eq!(
            reader_query_for_result(" query ", NativeSearchMatch::Title),
            None
        );
        assert_eq!(
            reader_query_for_result("  ", NativeSearchMatch::Message),
            None
        );
    }

    #[test]
    fn native_search_enter_targets_selection_or_first_match_safely() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        assert_eq!(activate_selection(&[], None), None);
        assert_eq!(activate_selection(&[first, second], None), Some(first));
        assert_eq!(activate_selection(&[first, second], Some(1)), Some(second));
        assert_eq!(activate_selection(&[first, second], Some(99)), Some(first));
    }

    #[test]
    fn snippet_is_bounded_to_matching_context_and_handles_unicode_case_folding() {
        let long = format!("{}find MIDDLE HERE{}", "p".repeat(150), "s".repeat(300));
        let snippet = matching_message_preview([long.as_str()], "middle").unwrap();
        assert!(snippet.contains("MIDDLE"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() < 180);

        let unicode = "İstanbul is a city";
        assert_eq!(
            matching_message_preview([unicode], "i"),
            Some(unicode.to_owned())
        );
        assert_eq!(
            matching_message_preview(["EL NIÑO\nsueña"], "niño"),
            Some("EL NIÑO ↵ sueña".to_owned())
        );
        assert!(matching_message_preview(["Private other conversation"], "absent").is_none());
        assert!(matching_message_preview(["Hello"], "  ").is_none());
    }

    #[test]
    fn snippets_scan_only_supplied_messages_not_tool_or_other_conversation_bodies() {
        assert_eq!(
            matching_message_preview(["unrelated", "SECOND owner match"], "match"),
            Some("SECOND owner match".to_owned())
        );
        assert!(matching_message_preview(["safe native message"], "tool secret").is_none());
    }

    #[test]
    fn empty_query_preserves_empty_or_nonempty_native_conversations() {
        assert_eq!(find_match("", "", []), Some(NativeSearchMatch::Title));
        assert_eq!(
            find_match(" \n ", "Drafted conversation", ["anything"]),
            Some(NativeSearchMatch::Title)
        );
    }

    #[test]
    fn searches_titles_case_insensitively_without_needing_message_text() {
        assert_eq!(
            find_match("  RUST  ", "Rust toolchain design", ["unrelated"]),
            Some(NativeSearchMatch::Title)
        );
        assert_eq!(find_match("rust", "Kotlin", ["unrelated"]), None);
    }

    #[test]
    fn searches_only_passed_local_user_and_assistant_messages() {
        assert_eq!(
            find_match(
                "permission",
                "Archive",
                ["Hello", "Explicit PERMISSION granted"]
            ),
            Some(NativeSearchMatch::Message)
        );
        assert_eq!(find_match("permission", "Archive", ["Hello"]), None);
    }

    #[test]
    fn unicode_query_and_title_take_priority_over_message_hits() {
        assert_eq!(
            find_match("MÉX", "México", ["mex does not need matching"]),
            Some(NativeSearchMatch::Title)
        );
        assert_eq!(
            find_match("ñ", "Untitled", ["El niño"]),
            Some(NativeSearchMatch::Message)
        );
    }
}
