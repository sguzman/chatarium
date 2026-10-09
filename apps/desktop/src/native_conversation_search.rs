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
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSearchMatch {
    Title,
    Message,
}

/// Match visible message text before falling back to a matching title.
/// Derived titles often repeat the first message; a title hit must not mask
/// the message actions (copy, stage and reader highlighting).
/// Empty search preserves the normal conversation list. Queries never cross
/// into another conversation's projected messages.
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
    find_match_with_position(query, title, visible_messages)
        .map(|(kind, hit)| (kind, hit.map(|(_, preview)| preview)))
}

/// Return the matching position in the exact supplied visible-message slice.
/// A title match has no message position. The index is used only for an
/// explicit user-triggered copy, never as an automatic context admission.
fn find_match_with_position<'a>(
    query: &str,
    title: &str,
    visible_messages: impl IntoIterator<Item = &'a str>,
) -> Option<(NativeSearchMatch, Option<(usize, String)>)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Some((NativeSearchMatch::Title, None));
    }
    if let Some(hit) = visible_messages
        .into_iter()
        .enumerate()
        .find_map(|(index, message)| {
            super::local_archive_search::local_snippet(message, &needle)
                .map(|preview| (index, preview))
        })
    {
        return Some((NativeSearchMatch::Message, Some(hit)));
    }
    title
        .to_lowercase()
        .contains(&needle)
        .then_some((NativeSearchMatch::Title, None))
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

/// Resolve a keyboard-highlighted native search match to a visible message.
/// A selected title-only hit deliberately cannot copy another row's text.
pub fn activate_copy_selection(
    matches: &[(LocalConversationId, Option<usize>)],
    selection: Option<usize>,
) -> Option<(LocalConversationId, usize)> {
    let &(conversation_id, message_index) = selection
        .and_then(|index| matches.get(index))
        .or_else(|| matches.first())?;
    message_index.map(|index| (conversation_id, index))
}

/// Produce an explicitly requested, provenance-bearing reference to exactly
/// one projected native user/assistant message. Missing positions fail closed.
/// Neither this projection nor its caller admits the text to model context.
pub fn copyable_hit_markdown(
    index: &NativeConversationSearchIndex,
    conversation_id: LocalConversationId,
    title: &str,
    message_index: usize,
) -> Option<(String, u64)> {
    let message = index.messages(conversation_id).get(message_index)?;
    let text = super::native_transcript_export::markdown(
        conversation_id,
        title,
        std::slice::from_ref(message),
    );
    Some((text, message.sequence))
}

/// Only an explicit user action may stage a retrieved message as a
/// local-memory draft. This is transient UI data, not a recorded artifact or
/// inference-context admission. Both the exact text and source identity come
/// from the same authorized native conversation projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedNativeMessage {
    pub source_conversation_id: LocalConversationId,
    pub source_event_sequence: u64,
    pub text: String,
}

/// An unedited staged draft is recorded against its native source, even if
/// another conversation is active. After an edit, the staged origin is cleared
/// and normal manual recording uses the active conversation instead.
pub fn recording_source_conversation(
    active: LocalConversationId,
    staged_origin: Option<(LocalConversationId, u64)>,
) -> LocalConversationId {
    staged_origin.map(|(source, _)| source).unwrap_or(active)
}

pub fn stageable_hit(
    index: &NativeConversationSearchIndex,
    conversation_id: LocalConversationId,
    message_index: usize,
) -> Option<StagedNativeMessage> {
    let message = index.messages(conversation_id).get(message_index)?;
    Some(StagedNativeMessage {
        source_conversation_id: conversation_id,
        source_event_sequence: message.sequence,
        text: message.text.clone(),
    })
}

/// Resolve the currently displayed match within a single sidebar row.
/// A stale out-of-range ordinal clamps safely to the newest available hit.
pub fn selected_message_index(row: &NativeSearchRow, ordinal: usize) -> Option<usize> {
    row.matching_message_indices
        .get(ordinal.min(row.matching_message_indices.len().checked_sub(1)?))
        .copied()
}

/// Cycle a selected hit in one conversation without touching other rows,
/// authoring a message, recording a memory or changing context permissions.
pub fn cycle_matching_message(
    row: &NativeSearchRow,
    ordinal: usize,
    delta: isize,
) -> Option<(usize, usize)> {
    let count = row.matching_message_indices.len();
    let current = ordinal.min(count.checked_sub(1)?);
    let next = (current as isize + delta).rem_euclid(count as isize) as usize;
    Some((next, row.matching_message_indices[next]))
}

/// Compute which *reader search hit* corresponds to the first occurrence in
/// a particular matching source message, so navigating to a later match does
/// not jump back to the first message in the conversation.
pub fn reader_hit_ordinal(
    messages: &[DisplayMessage],
    query: &str,
    message_index: usize,
) -> Option<usize> {
    let message = messages.get(message_index)?;
    if super::offline_reader::search_hits(&message.text, query).is_empty() {
        return None;
    }
    Some(
        messages[..message_index]
            .iter()
            .map(|previous| super::offline_reader::search_hits(&previous.text, query).len())
            .sum(),
    )
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
    /// Index into the same conversation's visible user/assistant projection.
    /// None for empty searches and title matches.
    pub message_index: Option<usize>,
    /// All matching visible-message positions in this conversation. The
    /// individual texts are not cloned into the sidebar search cache.
    pub matching_message_indices: Vec<usize>,
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
        if let Some((kind, hit)) = find_match_with_position(
            query,
            &title,
            messages.iter().map(|message| message.text.as_str()),
        ) {
            let (message_index, preview, matching_message_indices) = match hit {
                Some((first, preview)) => {
                    let needle = query.trim().to_lowercase();
                    let mut indices = vec![first];
                    indices.extend(messages.iter().enumerate().skip(first + 1).filter_map(
                        |(position, message)| {
                            message
                                .text
                                .to_lowercase()
                                .contains(&needle)
                                .then_some(position)
                        },
                    ));
                    (Some(first), Some(preview), indices)
                }
                None => (None, None, Vec::new()),
            };
            rows.push(NativeSearchRow {
                catalog_index,
                title,
                kind,
                preview,
                message_index,
                matching_message_indices,
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
    // A separately loaded equal-length archive must not inherit cached
    // projections solely because its first/last event metadata is identical.
    source_buffer_addr: usize,
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
// Record the backing-slice address as a second cheap identity hint: a newly
// loaded equal-length journal with a changed interior may have the same tail.
// Authoritative event contents remain immutable under ordinary append-only use.
fn payload_fingerprint(event: &EventEnvelope) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    event.payload.hash(&mut hasher);
    hasher.finish()
}

impl JournalRevision {
    fn capture(events: &[EventEnvelope]) -> Self {
        Self {
            event_count: events.len(),
            source_buffer_addr: events.as_ptr() as usize,
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
        // Equal-length snapshots are handled by exact revision equality.
        // If that equality failed (e.g. a new archive backing allocation),
        // there is no journal suffix to incrementally replay.
        if events.len() <= self.event_count {
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
    // Arc values allow a newly streamed assistant snapshot to reuse every
    // other conversation's projected message bodies without cloning them.
    messages: BTreeMap<LocalConversationId, Arc<Vec<DisplayMessage>>>,
    owners: Arc<BTreeMap<String, LocalConversationId>>,
    event_positions: BTreeMap<LocalConversationId, Arc<Vec<usize>>>,
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

        let mut by_conversation: BTreeMap<LocalConversationId, Vec<usize>> = BTreeMap::new();
        for (position, event) in events.iter().enumerate() {
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
                by_conversation.entry(owner).or_default().push(position);
            }
        }

        let mut messages = BTreeMap::new();
        let mut event_positions = BTreeMap::new();
        for (owner, positions) in by_conversation {
            let visible = projected_display_messages_iter(
                positions.iter().map(|position| &events[*position]),
            );
            messages.insert(owner, Arc::new(visible));
            event_positions.insert(owner, Arc::new(positions));
        }
        Self {
            revision: JournalRevision::capture(events),
            messages,
            owners: Arc::new(owners),
            event_positions,
        }
    }

    /// Incrementally replay typed local user commits and known assistant
    /// observations from an immutable journal suffix. Project only affected
    /// conversations; all unrelated conversation vectors stay shared.
    /// Malformed/legacy authorship, unknown assistant scopes or remapped
    /// existing turn owners fall back to a full rebuild.
    fn extend_native_appends(
        &self,
        events: &[EventEnvelope],
        first_new_position: usize,
    ) -> Option<Self> {
        let mut owners = Arc::clone(&self.owners);

        // Resolve every new typed turn before attributing assistant events,
        // matching the full builder's two-pass ownership semantics.
        for event in &events[first_new_position..] {
            if event.kind != EventKind::UserMessageCommitted {
                continue;
            }
            let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                decode_user_message_commit(event)
            else {
                return None;
            };
            let scope = local_turn_scope(message.turn_id);
            if owners
                .get(&scope)
                .is_some_and(|previous| *previous != message.conversation_id)
            {
                // A reassigned historical scope could change old projections.
                return None;
            }
            Arc::make_mut(&mut owners).insert(scope, message.conversation_id);
        }

        let mut positions = self.event_positions.clone();
        let mut touched = BTreeSet::new();
        for (position, event) in events.iter().enumerate().skip(first_new_position) {
            let owner = match event.kind {
                EventKind::UserMessageCommitted => {
                    let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                        decode_user_message_commit(event)
                    else {
                        return None;
                    };
                    message.conversation_id
                }
                EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved => {
                    *event.scope.as_deref().and_then(|scope| owners.get(scope))?
                }
                _ => continue,
            };
            Arc::make_mut(
                positions
                    .entry(owner)
                    .or_insert_with(|| Arc::new(Vec::new())),
            )
            .push(position);
            touched.insert(owner);
        }

        let mut messages = self.messages.clone();
        for owner in touched {
            let owned_positions = positions.get(&owner)?;
            let visible = projected_display_messages_iter(
                owned_positions.iter().map(|position| &events[*position]),
            );
            messages.insert(owner, Arc::new(visible));
        }
        Some(Self {
            revision: JournalRevision::capture(events),
            messages,
            owners,
            event_positions: positions,
        })
    }

    pub fn messages(&self, conversation_id: LocalConversationId) -> &[DisplayMessage] {
        self.messages
            .get(&conversation_id)
            .map(|messages| messages.as_slice())
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
        if cache.observed.is_append_only_prefix_of(events) {
            let first_new_position = cache.observed.event_count;
            if events[first_new_position..]
                .iter()
                .all(|event| !changes_native_messages(event.kind))
            {
                cache.observed = now;
                let reused = Arc::clone(&cache.index);
                ctx.data_mut(|data| data.insert_temp(id, cache));
                return reused;
            }
            if let Some(extended) = cache
                .index
                .extend_native_appends(events, first_new_position)
            {
                let index = Arc::new(extended);
                ctx.data_mut(|data| {
                    data.insert_temp(
                        id,
                        NativeSearchCache {
                            observed: now,
                            index: Arc::clone(&index),
                        },
                    )
                });
                return index;
            }
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
mod stress_tests;
#[cfg(test)]
mod perf_tests;

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
        // Neutral catalog titles ensure the initial hits come from message
        // bodies; derived titles may repeat the first message.
        catalog.rename(first, Some("One".to_owned()), 1).unwrap();
        catalog.rename(second, Some("Two".to_owned()), 2).unwrap();

        let mut store = MemoryEventStore::default();
        author(&mut store, first, "Rust renderer");
        author(&mut store, second, "Python tools");
        let index = cached_index(&ctx, store.events());
        let entries = catalog.entries();

        let initial = cached_rows(&ctx, &index, &entries, false, "rust");
        assert_eq!(initial.len(), 1);
        assert_eq!(entries[initial[0].catalog_index].id, first);
        assert_eq!(initial[0].kind, NativeSearchMatch::Message);
        assert_eq!(initial[0].message_index, Some(0));
        assert_eq!(
            index.messages(first)[initial[0].message_index.unwrap()].text,
            "Rust renderer"
        );
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

        catalog
            .rename(first, Some("Python project".to_owned()), 3)
            .unwrap();
        let renamed_entries = catalog.entries();
        let renamed = cached_rows(&ctx, &index, &renamed_entries, false, "python");
        assert_eq!(renamed.len(), 2);
        assert_eq!(renamed[0].kind, NativeSearchMatch::Title);
        assert_eq!(renamed[0].message_index, None);
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
    fn streaming_assistant_updates_only_reproject_the_owned_conversation() {
        use chatarium_store::{EventStore, MemoryEventStore};

        fn assert_same_messages(actual: &[DisplayMessage], expected: &[DisplayMessage]) {
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.role, expected.role);
                assert_eq!(actual.text, expected.text);
                assert_eq!(actual.sequence, expected.sequence);
                assert_eq!(actual.timestamp, expected.timestamp);
                assert_eq!(actual.provenance_label, expected.provenance_label);
            }
        }

        let ctx = egui::Context::default();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let first_turn = author(&mut store, first, "First conversation");
        author(&mut store, second, "Second private conversation");

        let initial = cached_index(&ctx, store.events());
        let other_messages = initial.messages.get(&second).unwrap();

        for (kind, text) in [
            (EventKind::AssistantSnapshotObserved, "partial reply"),
            (EventKind::AssistantCompletionObserved, "completed reply"),
        ] {
            store
                .append_scoped(
                    Some(local_turn_scope(first_turn)),
                    kind,
                    serde_json::json!({
                        "schema": "chatarium-responses-turn-observation",
                        "version": 1,
                        "text": text,
                        "details": {
                            "local_turn_id": first_turn.to_string(),
                            "request_id": first_turn.to_string(),
                        }
                    })
                    .to_string(),
                )
                .unwrap();
            let current = cached_index(&ctx, store.events());
            let full = NativeConversationSearchIndex::build(store.events());
            for owner in [first, second] {
                assert_same_messages(current.messages(owner), full.messages(owner));
            }
            assert!(Arc::ptr_eq(
                other_messages,
                current.messages.get(&second).unwrap()
            ));
            assert!(
                !current
                    .messages(first)
                    .iter()
                    .any(|m| m.text.contains("private"))
            );
            assert!(current.messages(first).iter().any(|m| m.text == text));
        }

        // A new native authored turn is replayed without touching the
        // unrelated conversation's projection.
        author(&mut store, first, "A new turn");
        let after_user = cached_index(&ctx, store.events());
        assert_same_messages(
            after_user.messages(first),
            NativeConversationSearchIndex::build(store.events()).messages(first),
        );
        assert!(Arc::ptr_eq(
            other_messages,
            after_user.messages.get(&second).unwrap()
        ));
    }

    #[test]
    fn many_conversations_preserve_unaffected_projection_on_new_turns() {
        use chatarium_store::{EventStore, MemoryEventStore};

        let ctx = egui::Context::default();
        let mut store = MemoryEventStore::default();
        let conversations = (0..128)
            .map(|index| {
                let owner = LocalConversationId::new();
                for message in 0..4 {
                    author(
                        &mut store,
                        owner,
                        &format!("archive {index} entry {message}"),
                    );
                }
                owner
            })
            .collect::<Vec<_>>();
        let target = conversations[32];
        let index = cached_index(&ctx, store.events());
        let untouched = conversations
            .iter()
            .filter(|owner| **owner != target)
            .map(|owner| (*owner, Arc::clone(index.messages.get(owner).unwrap())))
            .collect::<Vec<_>>();

        // The full index would scan hundreds of historical events here.
        // The incremental projection must preserve all other message Arcs.
        let new_turn = author(&mut store, target, "new isolated user turn");
        let after_user = cached_index(&ctx, store.events());
        assert_eq!(after_user.messages(target).len(), 5);
        for (owner, previous) in &untouched {
            assert!(Arc::ptr_eq(
                previous,
                after_user.messages.get(owner).unwrap()
            ));
        }
        store
            .append_scoped(
                Some(local_turn_scope(new_turn)),
                EventKind::AssistantCompletionObserved,
                serde_json::json!({
                    "schema": "chatarium-responses-turn-observation",
                    "version": 1,
                    "text": "new isolated assistant reply",
                    "details": {
                        "local_turn_id": new_turn.to_string(),
                        "request_id": new_turn.to_string(),
                    }
                })
                .to_string(),
            )
            .unwrap();
        let after_answer = cached_index(&ctx, store.events());
        let rebuilt = NativeConversationSearchIndex::build(store.events());
        for owner in &conversations {
            let actual = after_answer.messages(*owner);
            let expected = rebuilt.messages(*owner);
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.text, expected.text);
                assert_eq!(actual.sequence, expected.sequence);
                assert_eq!(actual.role, expected.role);
            }
        }
        for (owner, previous) in &untouched {
            assert!(Arc::ptr_eq(
                previous,
                after_answer.messages.get(owner).unwrap()
            ));
        }

        // Unknown assistant ownership forces the conservative full rebuild.
        store
            .append_scoped(
                Some("unknown-local-turn".to_owned()),
                EventKind::AssistantCompletionObserved,
                "UNOWNED_PRIVATE_OUTPUT".to_owned(),
            )
            .unwrap();
        let fallback = cached_index(&ctx, store.events());
        assert!(!Arc::ptr_eq(
            &untouched[0].1,
            fallback.messages.get(&untouched[0].0).unwrap()
        ));
        assert!(conversations.iter().all(|owner| {
            fallback
                .messages(*owner)
                .iter()
                .all(|message| !message.text.contains("UNOWNED_PRIVATE_OUTPUT"))
        }));
    }

    #[test]
    fn batched_new_authorship_and_assistant_use_full_builder_ordering() {
        use chatarium_store::{EventStore, MemoryEventStore};

        let ctx = egui::Context::default();
        let mut store = MemoryEventStore::default();
        let other = LocalConversationId::new();
        author(&mut store, other, "untouched conversation");
        let original = cached_index(&ctx, store.events());

        let owner = LocalConversationId::new();
        let turn = author(&mut store, owner, "new conversation");
        store
            .append_scoped(
                Some(local_turn_scope(turn)),
                EventKind::AssistantCompletionObserved,
                serde_json::json!({
                    "schema": "chatarium-responses-turn-observation",
                    "version": 1,
                    "text": "new answer",
                    "details": {
                        "local_turn_id": turn.to_string(),
                        "request_id": turn.to_string()
                    }
                })
                .to_string(),
            )
            .unwrap();

        let extended = cached_index(&ctx, store.events());
        let rebuilt = NativeConversationSearchIndex::build(store.events());
        let expected = rebuilt.messages(owner);
        let actual = extended.messages(owner);
        assert_eq!(actual.len(), expected.len());
        assert_eq!(
            actual
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>()
        );
        assert!(Arc::ptr_eq(
            original.messages.get(&other).unwrap(),
            extended.messages.get(&other).unwrap()
        ));
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
    fn equal_length_replaced_archive_with_same_tail_rebuilds_native_search() {
        use super::super::local_conversations::LocalConversationCatalog;
        use chatarium_store::{EventStore, MemoryEventStore};

        let ctx = egui::Context::default();
        let owner = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, owner, "unchanged first");
        author(&mut store, owner, "interior old");
        author(&mut store, owner, "unchanged tail");

        let cached = cached_index(&ctx, store.events());
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(owner, 1);
        catalog
            .rename(owner, Some("Neutral title".to_owned()), 2)
            .unwrap();
        let entries = catalog.entries();
        assert_eq!(cached_rows(&ctx, &cached, &entries, false, "old").len(), 1);

        // Preserve event count, first/last envelopes and replacement size.
        // Only the interior of a separately allocated journal is different.
        let mut swapped = store.events().to_vec();
        let original = swapped[1].payload.clone();
        swapped[1].payload = original.replacen("interior old", "interior new", 1);
        assert_ne!(swapped[1].payload, original);
        assert_eq!(swapped[1].payload.len(), original.len());
        assert_eq!(swapped.first(), store.events().first());
        assert_eq!(swapped.last(), store.events().last());

        let rebuilt = cached_index(&ctx, &swapped);
        assert!(!Arc::ptr_eq(&cached, &rebuilt));
        assert_eq!(rebuilt.messages(owner)[1].text, "interior new");
        assert!(cached_rows(&ctx, &rebuilt, &entries, false, "old").is_empty());
        let updated = cached_rows(&ctx, &rebuilt, &entries, false, "new");
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].message_index, Some(1));
        assert!(Arc::ptr_eq(&rebuilt, &cached_index(&ctx, &swapped)));
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
    fn keyboard_copy_targets_only_the_highlighted_visible_message() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        assert_eq!(activate_copy_selection(&[], None), None);
        let candidates = [(first, None), (second, Some(4))];
        assert_eq!(activate_copy_selection(&candidates, None), None);
        assert_eq!(activate_copy_selection(&candidates, Some(0)), None);
        assert_eq!(
            activate_copy_selection(&candidates, Some(1)),
            Some((second, 4))
        );
        assert_eq!(activate_copy_selection(&candidates, Some(99)), None);
        assert_eq!(
            activate_copy_selection(&[(first, Some(2))], None),
            Some((first, 2))
        );
    }

    #[test]
    fn explicit_hit_export_contains_only_one_owned_visible_message() {
        use chatarium_store::{EventStore, MemoryEventStore};

        let own = LocalConversationId::new();
        let other = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, own, "first unrelated message");
        author(&mut store, own, "wanted visible message");
        author(&mut store, other, "FOREIGN_PRIVATE_MESSAGE");
        store
            .append_scoped(
                None,
                EventKind::ToolCallOutcomeObserved,
                "HIDDEN_TOOL_SECRET".to_owned(),
            )
            .unwrap();

        let ctx = egui::Context::default();
        let index = cached_index(&ctx, store.events());
        let messages = index.messages(own);
        let hit = find_match_with_position(
            "wanted",
            "Neutral title",
            messages.iter().map(|message| message.text.as_str()),
        )
        .unwrap();
        let position = hit.1.unwrap().0;
        let (markdown, event_sequence) =
            copyable_hit_markdown(&index, own, "Neutral title", position).unwrap();

        assert_eq!(event_sequence, messages[position].sequence);
        assert!(markdown.contains("wanted visible message"));
        assert!(markdown.contains(&own.to_string()));
        assert!(markdown.contains(&format!("event #{event_sequence}")));
        assert!(!markdown.contains("first unrelated message"));
        assert!(!markdown.contains("FOREIGN_PRIVATE_MESSAGE"));
        assert!(!markdown.contains("HIDDEN_TOOL_SECRET"));
        assert!(copyable_hit_markdown(&index, own, "Neutral title", 99).is_none());
    }

    #[test]
    fn staging_a_native_message_keeps_exact_source_and_excludes_foreign_data() {
        use chatarium_store::{EventStore, MemoryEventStore};

        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, first, "first visible source");
        author(&mut store, second, "private foreign message");
        store
            .append_scoped(
                None,
                EventKind::ToolCallOutcomeObserved,
                "HIDDEN_TOOL".to_owned(),
            )
            .unwrap();

        let index = NativeConversationSearchIndex::build(store.events());
        let staged = stageable_hit(&index, first, 0).unwrap();
        assert_eq!(staged.source_conversation_id, first);
        assert_eq!(
            staged.source_event_sequence,
            index.messages(first)[0].sequence
        );
        assert_eq!(staged.text, "first visible source");
        assert!(!staged.text.contains("private foreign"));
        assert!(!staged.text.contains("HIDDEN_TOOL"));
        assert!(stageable_hit(&index, first, 1).is_none());
        assert_eq!(
            stageable_hit(&index, second, 0)
                .unwrap()
                .source_conversation_id,
            second
        );
    }

    #[test]
    fn derived_title_hit_preserves_explicit_copy_and_memory_staging() {
        use super::super::local_conversations::LocalConversationCatalog;
        use chatarium_store::{EventStore, MemoryEventStore};

        let source = LocalConversationId::new();
        let foreign = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(
            &mut store,
            source,
            "Reusable statement from the first message",
        );
        author(&mut store, foreign, "PRIVATE FOREIGN MESSAGE");

        let mut catalog = LocalConversationCatalog::default();
        catalog.create(source, 1);
        catalog.create(foreign, 2);
        let ctx = egui::Context::default();
        let index = cached_index(&ctx, store.events());
        let entries = catalog.entries();
        let rows = cached_rows(&ctx, &index, &entries, false, "reusable");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, NativeSearchMatch::Message);
        assert_eq!(rows[0].message_index, Some(0));
        assert!(rows[0].preview.as_deref().unwrap().contains("Reusable"));
        assert_eq!(entries[rows[0].catalog_index].id, source);

        let (owner, message_index) =
            activate_copy_selection(&[(source, rows[0].message_index)], None).unwrap();
        let (exported, sequence) =
            copyable_hit_markdown(&index, owner, &rows[0].title, message_index).unwrap();
        let staged = stageable_hit(&index, owner, message_index).unwrap();
        assert_eq!(staged.source_event_sequence, sequence);
        assert_eq!(staged.source_conversation_id, source);
        assert_eq!(staged.text, "Reusable statement from the first message");
        assert!(exported.contains(&staged.text));
        assert!(!exported.contains("PRIVATE FOREIGN MESSAGE"));
        assert_eq!(
            reader_query_for_result(" reusable ", rows[0].kind),
            Some("reusable".to_owned())
        );

        // A custom title alone must never fabricate a message to copy or stage.
        catalog
            .rename(source, Some("Title-only keyword".to_owned()), 3)
            .unwrap();
        let renamed = catalog.entries();
        let title_only = cached_rows(&ctx, &index, &renamed, false, "keyword");
        assert_eq!(title_only.len(), 1);
        assert_eq!(title_only[0].kind, NativeSearchMatch::Title);
        assert_eq!(title_only[0].message_index, None);
    }

    #[test]
    fn multiple_matching_messages_have_independent_provenance_and_reader_targets() {
        use super::super::local_conversations::LocalConversationCatalog;
        use chatarium_store::{EventStore, MemoryEventStore};

        let source = LocalConversationId::new();
        let foreign = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, source, "target first");
        author(&mut store, source, "not a match");
        author(&mut store, source, "target second");
        author(&mut store, foreign, "PRIVATE target foreign");
        author(&mut store, source, "target third");

        let mut catalog = LocalConversationCatalog::default();
        catalog.create(source, 1);
        catalog.create(foreign, 2);
        catalog
            .rename(source, Some("Independent title".to_owned()), 3)
            .unwrap();
        catalog
            .rename(foreign, Some("Unrelated".to_owned()), 4)
            .unwrap();
        let ctx = egui::Context::default();
        let index = cached_index(&ctx, store.events());
        let entries = catalog.entries();
        let rows = cached_rows(&ctx, &index, &entries, false, "target");
        let row = rows
            .iter()
            .find(|row| entries[row.catalog_index].id == source)
            .unwrap();
        assert_eq!(row.matching_message_indices, vec![0, 2, 3]);
        assert_eq!(row.message_index, Some(0));
        assert_eq!(selected_message_index(row, 1), Some(2));
        assert_eq!(selected_message_index(row, 2), Some(3));
        assert_eq!(selected_message_index(row, 999), Some(3));
        assert_eq!(cycle_matching_message(row, 0, 1), Some((1, 2)));
        assert_eq!(cycle_matching_message(row, 2, 1), Some((0, 0)));
        assert_eq!(cycle_matching_message(row, 0, -1), Some((2, 3)));
        assert_eq!(cycle_matching_message(row, 999, 1), Some((0, 0)));
        assert_eq!(
            reader_hit_ordinal(index.messages(source), "target", 2),
            Some(1)
        );
        assert_eq!(
            reader_hit_ordinal(index.messages(source), "target", 1),
            None
        );

        for (ordinal, expected) in [
            (0, "target first"),
            (1, "target second"),
            (2, "target third"),
        ] {
            let position = selected_message_index(row, ordinal).unwrap();
            let staged = stageable_hit(&index, source, position).unwrap();
            let (copied, seq) =
                copyable_hit_markdown(&index, source, &row.title, position).unwrap();
            assert_eq!(staged.text, expected);
            assert_eq!(staged.source_event_sequence, seq);
            assert_eq!(staged.source_conversation_id, source);
            assert!(copied.contains(expected));
            assert!(!copied.contains("PRIVATE target foreign"));
        }

        // A repeated query reuses its cached snapshot until it changes.
        assert!(Arc::ptr_eq(
            &rows,
            &cached_rows(&ctx, &index, &entries, false, "target"),
        ));
        let title_only = cached_rows(&ctx, &index, &entries, false, "independent");
        let row = title_only
            .iter()
            .find(|row| entries[row.catalog_index].id == source)
            .unwrap();
        assert_eq!(row.kind, NativeSearchMatch::Title);
        assert!(row.matching_message_indices.is_empty());
        assert_eq!(selected_message_index(row, 0), None);
        assert_eq!(cycle_matching_message(row, 0, 1), None);
    }

    #[test]
    fn recording_staged_message_remains_excluded_until_destination_admits_it() {
        use chatarium_core::local_memory::LocalMemoryId;
        use chatarium_store::local_memory_audit::{
            record_local_memory_artifact, replay_local_memory_audit,
        };
        use chatarium_store::local_memory_context_audit::{
            LocalMemoryContextDecision, record_local_memory_context_decision,
            replay_admitted_local_memory_context,
        };
        use chatarium_store::{EventStore, MemoryEventStore};

        let source = LocalConversationId::new();
        let destination = LocalConversationId::new();
        let unrelated = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        author(&mut store, source, "preserved source statement");

        let projected = NativeConversationSearchIndex::build(store.events());
        let staged = stageable_hit(&projected, source, 0).unwrap();
        let staged_origin = Some((staged.source_conversation_id, staged.source_event_sequence));
        assert_eq!(
            recording_source_conversation(destination, staged_origin),
            source
        );
        assert_eq!(
            recording_source_conversation(destination, None),
            destination
        );
        let memory_id = LocalMemoryId::new(1);
        record_local_memory_artifact(
            &mut store,
            memory_id,
            recording_source_conversation(destination, staged_origin),
            staged.text,
        )
        .unwrap();

        let artifacts = replay_local_memory_audit(store.events()).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].source_conversation_id, source);
        assert_eq!(artifacts[0].text, "preserved source statement");

        // Recording a memory is never itself an inference-context admission.
        assert!(
            replay_admitted_local_memory_context(store.events(), destination)
                .unwrap()
                .is_empty()
        );
        record_local_memory_context_decision(
            &mut store,
            memory_id,
            destination,
            LocalMemoryContextDecision::Admit,
        )
        .unwrap();
        let admitted = replay_admitted_local_memory_context(store.events(), destination).unwrap();
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].source_conversation_id, source);
        assert!(
            replay_admitted_local_memory_context(store.events(), unrelated)
                .unwrap()
                .is_empty()
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
    fn matching_positions_identify_only_visible_first_message_hits() {
        let messages = ["not this", "مرحبا México", "MÉXICO later"];
        let found = find_match_with_position("méxico", "Other", messages);
        assert_eq!(
            found,
            Some((
                NativeSearchMatch::Message,
                Some((1, "مرحبا México".to_owned())),
            ))
        );
        assert_eq!(
            find_match_with_position("mex", "Mex title", messages),
            Some((NativeSearchMatch::Title, None))
        );
        assert_eq!(
            find_match_with_position("", "Whatever", messages),
            Some((NativeSearchMatch::Title, None))
        );
        assert_eq!(find_match_with_position("absent", "Other", messages), None);
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
        assert_eq!(
            visited.get(),
            messages.len(),
            "a title-only fallback must verify there is no visible message match"
        );

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
    fn unicode_query_uses_title_when_no_visible_message_matches() {
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
