//! Deterministic read-only discovery over explicit local memory artifacts.
//!
//! Search never records, admits, excludes, supersedes, or otherwise mutates a
//! memory artifact. Superseded predecessors are hidden by default but remain
//! discoverable when historical results are explicitly requested.

use crate::EventEnvelope;
use crate::local_memory_audit::replay_local_memory_audit;
use crate::local_memory_supersession_audit::{
    current_memory_successor, replay_local_memory_supersession_audit,
};
use chatarium_core::LocalConversationId;
use chatarium_core::local_memory::LocalMemoryId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemorySearchItem {
    pub memory_id: LocalMemoryId,
    pub source_conversation_id: LocalConversationId,
    pub text: String,
    pub recorded_sequence: u64,
    pub superseded_by: Option<LocalMemoryId>,
}

/// Search immutable local memory text with deterministic case-insensitive
/// substring matching.
///
/// An empty query browses the eligible corpus. Results are newest-first.
/// Superseded predecessors are excluded unless `include_superseded` is true.
pub fn search_local_memory(
    events: &[EventEnvelope],
    query: &str,
    include_superseded: bool,
) -> Result<Vec<LocalMemorySearchItem>, String> {
    let artifacts = replay_local_memory_audit(events)?;
    let supersessions = replay_local_memory_supersession_audit(events)?;
    let needle = query.trim().to_lowercase();

    let mut items = artifacts
        .into_iter()
        .filter_map(|artifact| {
            let superseded_by = current_memory_successor(&supersessions, artifact.memory_id);
            if superseded_by.is_some() && !include_superseded {
                return None;
            }
            if !needle.is_empty() && !artifact.text.to_lowercase().contains(&needle) {
                return None;
            }
            Some(LocalMemorySearchItem {
                memory_id: artifact.memory_id,
                source_conversation_id: artifact.source_conversation_id,
                text: artifact.text,
                recorded_sequence: artifact.recorded_sequence,
                superseded_by,
            })
        })
        .collect::<Vec<_>>();

    items.sort_by_key(|item| std::cmp::Reverse(item.recorded_sequence));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::local_memory_audit::record_local_memory_artifact;
    use crate::local_memory_supersession_audit::record_local_memory_superseded;

    #[test]
    fn current_view_hides_superseded_history_and_preserves_exact_text() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(1), source, " Original Fact ")
            .unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(2), source, "Corrected Fact")
            .unwrap();
        record_local_memory_superseded(&mut store, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();

        let current = search_local_memory(store.events(), "", false).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].memory_id, LocalMemoryId::new(2));
        assert_eq!(current[0].text, "Corrected Fact");
        assert_eq!(current[0].superseded_by, None);

        let history = search_local_memory(store.events(), "", true).unwrap();
        assert_eq!(history.len(), 2);
        let predecessor = history
            .iter()
            .find(|item| item.memory_id == LocalMemoryId::new(1))
            .unwrap();
        assert_eq!(predecessor.text, " Original Fact ");
        assert_eq!(predecessor.superseded_by, Some(LocalMemoryId::new(2)));
    }

    #[test]
    fn query_is_case_insensitive_literal_substring_and_read_only() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(1),
            source,
            "Alpha BETA gamma",
        )
        .unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(2), source, "something else")
            .unwrap();

        let before = store.events().len();
        let results = search_local_memory(store.events(), " beta ", false).unwrap();
        assert_eq!(store.events().len(), before);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_id, LocalMemoryId::new(1));
        assert_eq!(results[0].text, "Alpha BETA gamma");
    }

    #[test]
    fn results_are_newest_first() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        for (id, text) in [(1, "one"), (2, "two"), (3, "three")] {
            record_local_memory_artifact(&mut store, LocalMemoryId::new(id), source, text).unwrap();
        }

        let results = search_local_memory(store.events(), "", false).unwrap();
        assert_eq!(
            results
                .iter()
                .map(|item| item.memory_id)
                .collect::<Vec<_>>(),
            vec![
                LocalMemoryId::new(3),
                LocalMemoryId::new(2),
                LocalMemoryId::new(1),
            ]
        );
    }
}
