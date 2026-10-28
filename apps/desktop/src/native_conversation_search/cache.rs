//! Ephemeral sidebar row and reader-hit caches scoped to the native projection.
use super::*;

#[derive(Clone)]
pub(super) struct NativeRowsCache {
    index: Arc<NativeConversationSearchIndex>,
    query: String,
    show_archived: bool,
    catalog: Vec<CatalogSearchKey>,
    // Retain the exact per-owner projections used to build these rows. A new
    // journal index can share all but one owner's message Arc; keeping those
    // identities lets sidebar refresh skip unchanged, potentially huge chats.
    pub(super) row_sources: Vec<Option<Arc<Vec<DisplayMessage>>>>,
    rows: Arc<Vec<NativeSearchRow>>,
}

/// Cache exact native reader highlight positions across redraws. The
/// source Arc is the authority: appends to unrelated conversations can reuse
/// positions, while edits to this conversation's projection invalidate them.
/// Imported/mirrored transcript readers are deliberately outside this cache.
#[derive(Clone)]
struct NativeReaderHitCache {
    conversation_id: LocalConversationId,
    message_projection: Option<Arc<Vec<DisplayMessage>>>,
    query: String,
    hits: Arc<Vec<(usize, usize, usize)>>,
}

/// Read-only hit positions in the current native user/assistant projection.
/// No hidden tool data, memory, or other conversation enters this projection.
pub fn cached_reader_hits(
    ctx: &egui::Context,
    index: &Arc<NativeConversationSearchIndex>,
    conversation_id: LocalConversationId,
    query: &str,
) -> Arc<Vec<(usize, usize, usize)>> {
    let projection = index.messages.get(&conversation_id).cloned();
    let cache_id = egui::Id::new("chatarium-native-transcript-reader-hits");
    if let Some(cache) = ctx.data_mut(|data| data.get_temp::<NativeReaderHitCache>(cache_id)) {
        let unchanged = match (&cache.message_projection, &projection) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if cache.conversation_id == conversation_id && cache.query == query && unchanged {
            return cache.hits;
        }
    }
    let hits = if query.trim().is_empty() {
        Vec::new()
    } else {
        projection
            .as_deref()
            .map(|messages| {
                messages
                    .iter()
                    .enumerate()
                    .flat_map(|(message_index, message)| {
                        crate::offline_reader::search_hits(&message.text, query)
                            .into_iter()
                            .map(move |(start, end)| (message_index, start, end))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let hits = Arc::new(hits);
    ctx.data_mut(|data| {
        data.insert_temp(
            cache_id,
            NativeReaderHitCache {
                conversation_id,
                message_projection: projection,
                query: query.to_owned(),
                hits: Arc::clone(&hits),
            },
        )
    });
    hits
}

/// Build one native search row with a single message scan. Same-run
/// release-mode profiling favors this over the former two-pass row builder.
/// The older path remains a test-only comparison reference.
pub(super) fn collect_native_search_row(
    needle: &str,
    catalog_index: usize,
    title: String,
    messages: &[DisplayMessage],
) -> Option<NativeSearchRow> {
    let mut matching_message_indices = Vec::new();
    let mut preview = None;
    let mut authored_words = 0_usize;
    let mut authored_bytes = 0_usize;
    // One pass computes both the filtered hits and the sidebar totals. With
    // no query, skip case folding entirely; the cached row is reused on
    // subsequent egui frames until its conversation projection changes.
    for (position, message) in messages.iter().enumerate() {
        if matches!(message.role, DisplayRole::User | DisplayRole::Assistant) {
            authored_words += message.text.split_whitespace().count();
            authored_bytes += message.text.len();
        }
        if !needle.is_empty() && message.text.to_lowercase().contains(needle) {
            if matching_message_indices.is_empty() {
                preview = crate::local_archive_search::local_snippet(&message.text, needle);
            }
            matching_message_indices.push(position);
        }
    }
    let summary = NativeRowSummary {
        message_count: messages.len(),
        authored_words,
        authored_bytes,
        latest_excerpt: messages
            .last()
            .map(|message| {
                message
                    .text
                    .split_whitespace()
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|value| !value.is_empty()),
    };
    let message_index = matching_message_indices.first().copied();
    let kind = if needle.is_empty()
        || (message_index.is_none() && title.to_lowercase().contains(needle))
    {
        NativeSearchMatch::Title
    } else if message_index.is_some() {
        NativeSearchMatch::Message
    } else {
        return None;
    };
    Some(NativeSearchRow {
        catalog_index,
        title,
        kind,
        preview,
        message_index,
        matching_message_indices,
        summary,
    })
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
    let previous = ctx.data_mut(|data| data.get_temp::<NativeRowsCache>(id));
    if let Some(cache) = &previous {
        if Arc::ptr_eq(&cache.index, index)
            && cache.query == query
            && cache.show_archived == show_archived
            && cache.catalog == catalog
        {
            return Arc::clone(&cache.rows);
        }
    }

    // Query and catalog changes require a full scan. A new index caused by
    // a journal append may still share each unaffected conversation's Arc.
    // Reuse both matching rows and cached misses without inspecting their
    // transcripts again. The catalog snapshot fixes the row indices/order.
    let reusable = previous.as_ref().is_some_and(|cache| {
        cache.query == query
            && cache.show_archived == show_archived
            && cache.catalog == catalog
            && cache.row_sources.len() == entries.len()
    });
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    let mut row_sources = Vec::with_capacity(entries.len());
    for (catalog_index, entry) in entries.iter().enumerate() {
        let projection = index.messages.get(&entry.id);
        let unchanged = reusable
            && previous.as_ref().is_some_and(|cache| {
                match (cache.row_sources[catalog_index].as_ref(), projection) {
                    (Some(old), Some(new)) => Arc::ptr_eq(old, new),
                    (None, None) => true,
                    _ => false,
                }
            });
        if !entry.archived || show_archived {
            let row = if unchanged {
                // Rows are ordered by catalog_index. A failed lookup is a
                // genuine cached miss, not permission to scan another owner.
                previous.as_ref().and_then(|cache| {
                    cache
                        .rows
                        .binary_search_by_key(&catalog_index, |row| row.catalog_index)
                        .ok()
                        .map(|position| cache.rows[position].clone())
                })
            } else {
                let messages = index.messages(entry.id);
                let title = entry
                    .title
                    .as_deref()
                    .filter(|title| !title.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| derived_conversation_title(messages));
                collect_native_search_row(&needle, catalog_index, title, messages)
            };
            if let Some(row) = row {
                rows.push(row);
            }
        }
        row_sources.push(projection.cloned());
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
                row_sources,
                rows: Arc::clone(&rows),
            },
        )
    });
    rows
}

