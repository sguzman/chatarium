//! Search semantics for native Chatarium conversations (not imported remote mirrors).
//! Search only locally projected user/assistant text and the local workspace title.
//! The journal and metadata catalog remain authoritative; the query is ephemeral.

use super::local_conversations::LocalConversationEntry;
use super::{DisplayMessage, derived_conversation_title, projected_display_messages_iter};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_store::EventEnvelope;
use chatarium_store::authored::{
    DecodedUserMessageCommit, decode_user_message_commit, local_turn_scope,
};
use eframe::egui;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
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
    find_match_with_preview(query, title, visible_messages).map(|(kind, _)| kind)
}

/// Search a native conversation and produce its excerpt in the same pass.
/// This avoids searching every preceding message twice on each sidebar redraw.
/// Only projected user/assistant messages supplied by the caller are inspected.
pub fn find_match_with_preview<'a>(
    query: &str,
    title: &str,
    visible_messages: impl IntoIterator<Item = &'a str>,
) -> Option<(NativeSearchMatch, Option<String>)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || title.to_lowercase().contains(&needle) {
        return Some((NativeSearchMatch::Title, None));
    }
    visible_messages
        .into_iter()
        .find_map(|message| super::local_archive_search::local_snippet(message, &needle))
        .map(|preview| (NativeSearchMatch::Message, Some(preview)))
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

/// One visible sidebar row, derived solely from the local catalog and
/// projected native user/assistant messages. The position refers to the
/// catalog snapshot supplied to `cached_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSearchRow {
    pub catalog_index: usize,
    pub title: String,
    pub kind: NativeSearchMatch,
    pub preview: Option<String>,
}

/// These are the only mutable catalog facts that affect native sidebar
/// search and order. Last-opened timestamps are not part of the key, but
/// the ordered IDs are: switching chats can reorder the sidebar.
#[derive(Clone, PartialEq, Eq)]
struct CatalogSearchKey {
    id: LocalConversationId,
    title: Option<String>,
    archived: bool,
}

#[derive(Clone)]
struct NativeRowsCache {
    index: Arc<NativeConversationSearchIndex>,
    query: String,
    show_archived: bool,
    catalog: Vec<CatalogSearchKey>,
    rows: Arc<Vec<NativeSearchRow>>,
}

/// Cache the *results* of local native search across UI redraws. The journal
/// projection already has its own independent invalidation; preserving its
/// Arc identity lets unrelated journal events reuse search matches too.
/// Catalog ordering/title/archive changes and filter edits invalidate here.
pub fn cached_rows(
    ctx: &egui::Context,
    index: &Arc<NativeConversationSearchIndex>,
    entries: &[LocalConversationEntry],
    show_archived: bool,
    query: &str,
) -> Arc<Vec<NativeSearchRow>> {
    let catalog = entries
        .iter()
        .map(|entry| CatalogSearchKey {
            id: entry.id,
            title: entry.title.clone(),
            archived: entry.archived,
        })
        .collect::<Vec<_>>();
    let id = egui::Id::new("chatarium-native-conversation-search-rows");
    if let Some(cache) = ctx.data_mut(|data| data.get_temp::<NativeRowsCache>(id)) {
        if Arc::ptr_eq(&cache.index, index)
            && cache.query == query
            && cache.show_archived == show_archived
            && cache.catalog == catalog
        {
            return cache.rows;
        }
    }

    let mut rows = Vec::new();
    for (catalog_index, entry) in entries.iter().enumerate() {
        if entry.archived && !show_archived {
            continue;
        }
        let messages = index.messages(entry.id);
        let title = entry
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| derived_conversation_title(messages));
        if let Some((kind, preview)) = find_match_with_preview(
            query,
            &title,
            messages.iter().map(|message| message.text.as_str()),
        ) {
            rows.push(NativeSearchRow {
                catalog_index,
                title,
                kind,
                preview,
            });
        }
    }
    let rows = Arc::new(rows);
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            NativeRowsCache {
                index: Arc::clone(index),
                query: query.to_owned(),
                show_archived,
                catalog,
                rows: Arc::clone(&rows),
            },
        )
    });
    rows
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
    last_at_unix_ms: Option<u64>,
    last_payload_bytes: Option<usize>,
    last_payload_fingerprint: Option<u64>,
}

// The journal is append-only during ordinary operation. Still, an archive
// replacement can retain the same event count, sequence, kind, scope and
// payload byte length while changing visible conversation text. Fingerprint
// the boundary event rather than trusting its size alone. The fingerprint is
// a cache invalidation hint, not a cryptographic journal integrity check.
fn payload_fingerprint(event: &EventEnvelope) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    event.payload.hash(&mut hasher);
    hasher.finish()
}

impl JournalRevision {
    fn capture(events: &[EventEnvelope]) -> Self {
        Self {
            event_count: events.len(),
            first_sequence: events.first().map(|event| event.sequence),
            last_sequence: events.last().map(|event| event.sequence),
            last_scope: events.last().and_then(|event| event.scope.clone()),
            last_kind: events.last().map(|event| event.kind),
            last_at_unix_ms: events.last().map(|event| event.at_unix_ms),
            last_payload_bytes: events.last().map(|event| event.payload.len()),
            last_payload_fingerprint: events.last().map(payload_fingerprint),
        }
    }

    fn is_append_only_prefix_of(&self, events: &[EventEnvelope]) -> bool {
        if events.len() < self.event_count {
            return false;
        }
        if self.event_count == 0 {
            return true;
        }
        let Some(last) = events.get(self.event_count - 1) else {
            return false;
        };
        self.first_sequence == events.first().map(|event| event.sequence)
            && self.last_sequence == Some(last.sequence)
            && self.last_scope.as_deref() == last.scope.as_deref()
            && self.last_kind == Some(last.kind)
            && self.last_at_unix_ms == Some(last.at_unix_ms)
            && self.last_payload_bytes == Some(last.payload.len())
            && self.last_payload_fingerprint == Some(payload_fingerprint(last))
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

        let mut by_conversation: BTreeMap<LocalConversationId, Vec<&EventEnvelope>> =
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
                    .push(event);
            }
        }

        let messages = by_conversation
            .into_iter()
            .map(|(owner, own_events)| (owner, projected_display_messages_iter(own_events)))
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

/// The indexed search corpus only depends on native authored messages and
/// their matching assistant snapshot/completion observations. Draft writes,
/// tool events and controller state may advance the append-only journal
/// without changing any searchable native conversation.
fn changes_native_messages(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::UserMessageCommitted
            | EventKind::AssistantSnapshotObserved
            | EventKind::AssistantCompletionObserved
    )
}

#[derive(Clone)]
struct NativeSearchCache {
    observed: JournalRevision,
    index: Arc<NativeConversationSearchIndex>,
}

/// Ephemeral egui-frame projection; metadata and journal remain authoritative.
/// When only unrelated journal events append, advance the cache cursor without
/// cloning or rebuilding the potentially large message corpus.
pub fn cached_index(
    ctx: &egui::Context,
    events: &[EventEnvelope],
) -> Arc<NativeConversationSearchIndex> {
    let id = egui::Id::new("chatarium-native-conversation-search-index");
    let now = JournalRevision::capture(events);
    if let Some(mut cache) = ctx.data_mut(|data| data.get_temp::<NativeSearchCache>(id)) {
        if cache.observed == now {
            return cache.index;
        }
        if cache.observed.is_append_only_prefix_of(events)
            && events[cache.observed.event_count..]
                .iter()
                .all(|event| !changes_native_messages(event.kind))
        {
            cache.observed = now;
            let reused = Arc::clone(&cache.index);
            ctx.data_mut(|data| data.insert_temp(id, cache));
            return reused;
        }
    }
    let index = Arc::new(NativeConversationSearchIndex::build(events));
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            NativeSearchCache {
                observed: now,
                index: Arc::clone(&index),
            },
        )
    });
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
    fn native_index_cache_ignores_unrelated_draft_and_tool_journal_appends() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let ctx = egui::Context::default();
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let turn = author(&mut store, owner, "initial typed message");
        let base = cached_index(&ctx, store.events());
        store
            .append_scoped(
                None,
                EventKind::DraftChanged,
                "draft input updated".to_owned(),
            )
            .unwrap();
        let after_draft = cached_index(&ctx, store.events());
        assert!(Arc::ptr_eq(&base, &after_draft));
        store
            .append_scoped(
                Some(local_turn_scope(turn)),
                EventKind::ToolCallOutcomeObserved,
                "tool outcome must not be indexed".to_owned(),
            )
            .unwrap();
        let after_tool = cached_index(&ctx, store.events());
        assert!(Arc::ptr_eq(&base, &after_tool));

        store
            .append_scoped(
                Some(local_turn_scope(turn)),
                EventKind::AssistantCompletionObserved,
                serde_json::json!({
                    "schema": "chatarium-responses-turn-observation",
                    "version": 1,
                    "text": "newly observed assistant message",
                    "details": {
                        "local_turn_id": turn.to_string(),
                        "request_id": turn.to_string()
                    }
                })
                .to_string(),
            )
            .unwrap();
        let changed = cached_index(&ctx, store.events());
        assert!(!Arc::ptr_eq(&base, &changed));
        assert!(
            changed
                .messages(owner)
                .iter()
                .any(|message| { message.text == "newly observed assistant message" })
        );
    }

    #[test]
    fn sidebar_results_cache_tracks_query_catalog_and_visible_messages() {
        use super::super::local_conversations::LocalConversationCatalog;
        use chatarium_store::{EventStore, MemoryEventStore};

        let ctx = egui::Context::default();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);

        let mut store = MemoryEventStore::default();
        author(&mut store, first, "Rust renderer");
        author(&mut store, second, "Python tools");
        let index = cached_index(&ctx, store.events());
        let entries = catalog.entries();

        let initial = cached_rows(&ctx, &index, &entries, false, "rust");
        assert_eq!(initial.len(), 1);
        assert_eq!(entries[initial[0].catalog_index].id, first);
        assert_eq!(initial[0].kind, NativeSearchMatch::Message);
        let repeated = cached_rows(&ctx, &index, &entries, false, "rust");
        assert!(Arc::ptr_eq(&initial, &repeated));

        store
            .append_scoped(None, EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        let unchanged_index = cached_index(&ctx, store.events());
        assert!(Arc::ptr_eq(&index, &unchanged_index));
        let unchanged = cached_rows(&ctx, &unchanged_index, &entries, false, "rust");
        assert!(Arc::ptr_eq(&initial, &unchanged));

        let changed_query = cached_rows(&ctx, &index, &entries, false, "python");
        assert!(!Arc::ptr_eq(&initial, &changed_query));
        assert_eq!(changed_query.len(), 1);
        assert_eq!(entries[changed_query[0].catalog_index].id, second);

        catalog.rename(first, Some("Python project".to_owned()), 3).unwrap();
        let renamed_entries = catalog.entries();
        let renamed = cached_rows(&ctx, &index, &renamed_entries, false, "python");
        assert_eq!(renamed.len(), 2);
        assert_eq!(renamed[0].kind, NativeSearchMatch::Title);
        assert!(!Arc::ptr_eq(&changed_query, &renamed));

        catalog.set_archived(first, true, 4).unwrap();
        let archived_entries = catalog.entries();
        let hidden = cached_rows(&ctx, &index, &archived_entries, false, "python");
        assert_eq!(hidden.len(), 1);
        let shown = cached_rows(&ctx, &index, &archived_entries, true, "python");
        assert_eq!(shown.len(), 2);

        author(&mut store, second, "A new rust discussion");
        let advanced_index = cached_index(&ctx, store.events());
        assert!(!Arc::ptr_eq(&index, &advanced_index));
        let fresh = cached_rows(&ctx, &advanced_index, &archived_entries, false, "rust");
        assert_eq!(fresh.len(), 1);
        assert_eq!(archived_entries[fresh[0].catalog_index].id, second);
    }

    #[test]
    fn native_index_cache_rebuilds_if_journal_replaces_prior_tail() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let ctx = egui::Context::default();
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, owner, "first");
        let cached = cached_index(&ctx, store.events());
        let mut replaced = store.events().to_vec();
        replaced.last_mut().unwrap().scope = Some("different-scope".to_owned());
        let next = cached_index(&ctx, &replaced);
        assert!(!Arc::ptr_eq(&cached, &next));
    }

    #[test]
    fn native_index_cache_rebuilds_when_same_sized_tail_payload_changes() {
        use chatarium_store::{EventStore, MemoryEventStore};
        let ctx = egui::Context::default();
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, owner, "first");
        let original = cached_index(&ctx, store.events());
        assert_eq!(original.messages(owner)[0].text, "first");

        // Preserve every old revision discriminator, including the byte
        // length, while changing the source text of a durable user message.
        let mut replaced = store.events().to_vec();
        let tail = replaced.last_mut().unwrap();
        let new_payload = tail.payload.replacen("first", "other", 1);
        assert_ne!(new_payload, tail.payload);
        assert_eq!(new_payload.len(), tail.payload.len());
        tail.payload = new_payload;

        let refreshed = cached_index(&ctx, &replaced);
        assert!(!Arc::ptr_eq(&original, &refreshed));
        assert_eq!(refreshed.messages(owner)[0].text, "other");
        assert!(Arc::ptr_eq(&refreshed, &cached_index(&ctx, &replaced)));
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
    fn combined_match_and_preview_stops_at_first_hit_without_scanning_twice() {
        use std::cell::Cell;

        let visited = Cell::new(0);
        let messages = ["unrelated", "México ↵ matters", "later matching text"];
        let counted = messages
            .iter()
            .copied()
            .inspect(|_| visited.set(visited.get() + 1));
        let result = find_match_with_preview(" MÉXICO ", "Other title", counted);
        let expected = (
            NativeSearchMatch::Message,
            Some("México ↵ matters".to_owned()),
        );
        assert_eq!(result, Some(expected));
        assert_eq!(visited.get(), 2);

        visited.set(0);
        let counted = messages
            .iter()
            .copied()
            .inspect(|_| visited.set(visited.get() + 1));
        let title_result = find_match_with_preview("mex", "MEX title", counted);
        assert_eq!(title_result, Some((NativeSearchMatch::Title, None)));
        assert_eq!(visited.get(), 0);

        assert_eq!(
            find_match_with_preview("absent", "Other", ["not a hit"]),
            None
        );
        assert_eq!(
            find_match_with_preview("  ", "", ["anything"]),
            Some((NativeSearchMatch::Title, None))
        );
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
