//! Incremental typed journal index; conversation ownership remains authoritative.
use super::*;

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
    pub(super) messages: BTreeMap<LocalConversationId, Arc<Vec<DisplayMessage>>>,
    pub(super) owners: Arc<BTreeMap<String, LocalConversationId>>,
    // An authored scope claimed by different local conversations is never
    // safe to attribute to either one, including after journal appends.
    pub(super) conflicted_scopes: Arc<BTreeSet<String>>,
    event_positions: BTreeMap<LocalConversationId, Arc<Vec<usize>>>,
}

impl NativeConversationSearchIndex {
    pub fn build(events: &[EventEnvelope]) -> Self {
        let mut owners = BTreeMap::new();
        let mut conflicted_scopes = BTreeSet::new();
        let mut by_conversation: BTreeMap<LocalConversationId, Vec<usize>> = BTreeMap::new();

        // Typed user messages establish turn ownership and their own search
        // positions in the same decoding pass. Parsing these envelopes again
        // in the second pass made large authored archives needlessly expensive.
        for (position, event) in events.iter().enumerate() {
            if let Ok(Some(DecodedUserMessageCommit::Typed(message))) =
                decode_user_message_commit(event)
            {
                // Only typed authored user turns establish a conversation's
                // native assistant scope. Tool, controller, and legacy
                // payloads must never become native searchable messages.
                let scope = local_turn_scope(message.turn_id);
                if !conflicted_scopes.contains(&scope) {
                    if owners
                        .get(&scope)
                        .is_some_and(|previous| *previous != message.conversation_id)
                    {
                        // Conflicting ownership is ambiguous: retain each
                        // author's own message, but quarantine all assistant
                        // observations for this scope instead of leaking them
                        // across conversation boundaries.
                        owners.remove(&scope);
                        conflicted_scopes.insert(scope);
                    } else {
                        owners.insert(scope, message.conversation_id);
                    }
                }
                by_conversation
                    .entry(message.conversation_id)
                    .or_default()
                    .push(position);
            }
        }

        // Resolve assistant observations after all typed owners are known:
        // historical journals may contain observations ahead of authorship.
        // Their positions are merged and sorted before applying the existing
        // chronological display-message deduplication rules.
        for (position, event) in events.iter().enumerate() {
            if !matches!(
                event.kind,
                EventKind::AssistantSnapshotObserved | EventKind::AssistantCompletionObserved
            ) {
                continue;
            }
            if let Some(owner) = event
                .scope
                .as_deref()
                .and_then(|scope| owners.get(scope))
                .copied()
            {
                by_conversation.entry(owner).or_default().push(position);
            }
        }

        let mut messages = BTreeMap::new();
        let mut event_positions = BTreeMap::new();
        for (owner, mut positions) in by_conversation {
            positions.sort_unstable();
            let visible = projected_display_messages_iter(
                positions.iter().map(|position| &events[*position]),
            );
            messages.insert(owner, Arc::new(visible));
            event_positions.insert(owner, Arc::new(positions));
        }
        // Project only independently verified tool audit records. The native
        // journal is still authoritative; forged standalone event payloads
        // cannot enter the visible transcript or another owner's conversation.
        for (owner, tool_entries) in crate::mcp_transcript::verified_activity(events) {
            let visible = messages
                .entry(owner)
                .or_insert_with(|| Arc::new(Vec::new()));
            Arc::make_mut(visible).extend(tool_entries);
            Arc::make_mut(visible).sort_by_key(|message| message.sequence);
        }
        // Agent lifecycle has its own scoped, verified durable audit and
        // does not become authored or assistant transcript context.
        for (owner, lifecycle) in crate::siwc_activity_transcript::verified_activity(events) {
            let visible = messages
                .entry(owner)
                .or_insert_with(|| Arc::new(Vec::new()));
            Arc::make_mut(visible).extend(lifecycle);
            Arc::make_mut(visible).sort_by_key(|message| message.sequence);
        }
        Self {
            revision: JournalRevision::capture(events),
            messages,
            owners: Arc::new(owners),
            conflicted_scopes: Arc::new(conflicted_scopes),
            event_positions,
        }
    }

    /// Incrementally replay typed local user commits and known assistant
    /// observations from an immutable journal suffix. Project only affected
    /// conversations; all unrelated conversation vectors stay shared.
    /// Malformed/legacy authorship, unknown assistant scopes or remapped
    /// existing turn owners fall back to a full rebuild.
    pub(super) fn extend_native_appends(
        &self,
        events: &[EventEnvelope],
        first_new_position: usize,
    ) -> Option<Self> {
        // MCP correlation can revise an earlier audit row, so fall back to
        // the full verified projection. SIWC lifecycle updates are different:
        // re-project only the activity cards while preserving existing
        // immutable authored/assistant transcript vectors.
        if events[first_new_position..]
            .iter()
            .any(|event| crate::mcp_transcript::changes_activity(event.kind))
        {
            return None;
        }
        let activity_changed = events[first_new_position..]
            .iter()
            .any(|event| event.kind == EventKind::SiwcAgentActivityObserved);
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
            if self.conflicted_scopes.contains(&scope) {
                // A quarantined scope cannot be revived incrementally.
                return None;
            }
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
            let mut visible = projected_display_messages_iter(
                owned_positions.iter().map(|position| &events[*position]),
            );
            // A new ordinary message must not erase already verified
            // non-authored MCP/activity cards from the same conversation.
            if let Some(existing) = self.messages.get(&owner) {
                visible.extend(
                    existing
                        .iter()
                        .filter(|entry| {
                            matches!(
                                entry.role,
                                crate::DisplayRole::Mcp | crate::DisplayRole::Activity
                            )
                        })
                        .cloned(),
                );
                visible.sort_by_key(|entry| entry.sequence);
            }
            messages.insert(owner, Arc::new(visible));
        }
        if activity_changed {
            // Replay every observation for integrity, but copy/update only
            // conversations named by the newly appended journal suffix.
            // A streaming delta must not reallocate other chats' cards.
            let mut verified = crate::siwc_activity_transcript::verified_activity(events);
            if verified.is_empty() {
                // A failed audit replay invalidates every previously cached
                // activity card, including cards outside the appended suffix.
                // Remove only those cards so unrelated authored conversations
                // keep their shared projection and are not needlessly rebuilt.
                for entries in messages.values_mut() {
                    if entries
                        .iter()
                        .any(|entry| entry.role == crate::DisplayRole::Activity)
                    {
                        Arc::make_mut(entries)
                            .retain(|entry| entry.role != crate::DisplayRole::Activity);
                    }
                }
            }
            let affected = events[first_new_position..]
                .iter()
                .filter(|event| event.kind == EventKind::SiwcAgentActivityObserved)
                .filter_map(|event| event.scope.as_deref())
                .filter_map(|scope| scope.strip_prefix("siwc-activity:"))
                .filter_map(|scoped| scoped.split_once(':'))
                .filter_map(|(owner, _turn)| owner.parse::<LocalConversationId>().ok())
                .collect::<BTreeSet<_>>();
            for owner in affected {
                let entries = messages
                    .entry(owner)
                    .or_insert_with(|| Arc::new(Vec::new()));
                let visible = Arc::make_mut(entries);
                visible.retain(|entry| entry.role != crate::DisplayRole::Activity);
                visible.extend(verified.remove(&owner).unwrap_or_default());
                visible.sort_by_key(|entry| entry.sequence);
            }
        }
        Some(Self {
            revision: JournalRevision::capture(events),
            messages,
            owners,
            conflicted_scopes: Arc::clone(&self.conflicted_scopes),
            event_positions: positions,
        })
    }

    pub fn message_projection(
        &self,
        conversation_id: LocalConversationId,
    ) -> Option<Arc<Vec<DisplayMessage>>> {
        self.messages.get(&conversation_id).cloned()
    }

    pub fn messages(&self, conversation_id: LocalConversationId) -> &[DisplayMessage] {
        self.messages
            .get(&conversation_id)
            .map(|messages| messages.as_slice())
            .unwrap_or(&[])
    }

    pub(super) fn matches_journal(&self, events: &[EventEnvelope]) -> bool {
        self.revision == JournalRevision::capture(events)
    }
}
/// The indexed search corpus only depends on native authored messages and
/// their matching assistant snapshot/completion observations. Draft writes,
/// tool events and controller state may advance the append-only journal
/// without changing any searchable native conversation.
fn changes_native_messages(kind: EventKind) -> bool {
    crate::mcp_transcript::changes_activity(kind)
        || matches!(
            kind,
            EventKind::SiwcAgentActivityObserved
                | EventKind::UserMessageCommitted
                | EventKind::AssistantSnapshotObserved
                | EventKind::AssistantCompletionObserved
        )
}

#[derive(Clone)]
struct NativeSearchCache {
    observed: JournalRevision,
    index: Arc<NativeConversationSearchIndex>,
}

/// Seed the egui cache with an index already built on the startup worker.
///
/// This avoids constructing the full native search corpus on the first GUI
/// frame. The worker source and UI-owned event clone must have equivalent
/// journal revision metadata; differing backing-slice pointers are expected.
pub fn prime_prebuilt_index(
    ctx: &egui::Context,
    events: &[EventEnvelope],
    index: Arc<NativeConversationSearchIndex>,
) -> Result<(), String> {
    let observed = JournalRevision::capture(events);
    let mut prepared = index.revision.clone();
    prepared.source_buffer_addr = observed.source_buffer_addr;
    if prepared != observed {
        return Err("background native search index does not match the active event snapshot".to_owned());
    }
    let id = egui::Id::new("chatarium-native-conversation-search-index");
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            NativeSearchCache {
                observed,
                index,
            },
        )
    });
    Ok(())
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
mod bootstrap_cache_tests {
    use super::*;
    use chatarium_store::{EventStore, MemoryEventStore};

    #[test]
    fn prebuilt_background_index_is_used_without_first_frame_rebuild() {
        let mut journal = MemoryEventStore::default();
        journal
            .append(EventKind::DraftChanged, "native".to_owned())
            .unwrap();
        let worker_events = journal.events().to_vec();
        let prepared = Arc::new(NativeConversationSearchIndex::build(&worker_events));
        let ui_events = worker_events.clone();
        let ctx = egui::Context::default();
        prime_prebuilt_index(&ctx, &ui_events, Arc::clone(&prepared)).unwrap();
        let displayed = cached_index(&ctx, &ui_events);
        assert!(Arc::ptr_eq(&displayed, &prepared));

        let mut divergent = ui_events.clone();
        divergent[0].payload = "changed".to_owned();
        assert!(prime_prebuilt_index(&ctx, &divergent, prepared).is_err());
    }
}
