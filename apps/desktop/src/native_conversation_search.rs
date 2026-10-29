//! Search semantics for native Chatarium conversations (not imported remote mirrors).
//! Search only locally projected user/assistant text and the local workspace title.
//! The journal and metadata catalog remain authoritative; the query is ephemeral.

use super::local_conversations::LocalConversationEntry;
use super::{
    DisplayMessage, DisplayRole, derived_conversation_title, projected_display_messages_iter,
};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_store::EventEnvelope;
use chatarium_store::authored::{
    DecodedUserMessageCommit, decode_user_message_commit, local_turn_scope,
};
use eframe::egui;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

mod cache;
mod index;
mod index_background;
mod matching;
mod navigation;
mod reader_background;

#[cfg(test)]
use cache::{NativeRowsCache, collect_native_search_row};
pub use cache::{background_cached_rows, cached_reader_hits, cached_rows};
pub use index::{NativeConversationSearchIndex, cached_index, prime_prebuilt_index};
pub use index_background::background_index;
use matching::find_match_with_position;
pub use matching::{NativeSearchMatch, find_match, find_match_with_preview};
pub use navigation::*;
pub use reader_background::{background_reader_hits, background_without_activity};

/// Lightweight native sidebar metadata, cached alongside search results.
/// In particular, the word/byte scan must not run again on every UI frame
/// for every conversation in a long local archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRowSummary {
    pub message_count: usize,
    pub authored_words: usize,
    pub authored_bytes: usize,
    pub latest_excerpt: Option<String>,
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
    pub summary: NativeRowSummary,
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

#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod stress_tests;
#[cfg(test)]
mod tests;
