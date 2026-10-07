//! Deterministic read-only discovery over explicit local memory artifacts.
//!
//! Search never records, admits, excludes, supersedes, or otherwise mutates a
//! memory artifact. Superseded predecessors are hidden by default but remain
//! discoverable when historical results are explicitly requested.

use crate::EventEnvelope;
use crate::local_memory_audit::replay_local_memory_audit;
use crate::local_memory_label_audit::replay_active_local_memory_labels;
use crate::local_memory_supersession_audit::{
    current_memory_successor, replay_local_memory_supersession_audit,
};
use chatarium_core::LocalConversationId;
use chatarium_core::local_memory::{LocalMemoryId, LocalMemoryLabel};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemorySearchItem {
    pub memory_id: LocalMemoryId,
    pub source_conversation_id: LocalConversationId,
    pub text: String,
    pub recorded_sequence: u64,
    pub labels: Vec<LocalMemoryLabel>,
    pub superseded_by: Option<LocalMemoryId>,
}

/// One exact active-label facet over the currently eligible discovery corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMemoryLabelFacet {
    pub label: LocalMemoryLabel,
    pub artifact_count: usize,
}

/// One exact source-conversation facet over the eligible discovery corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalMemorySourceFacet {
    pub source_conversation_id: LocalConversationId,
    pub artifact_count: usize,
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
    search_local_memory_filtered(events, query, include_superseded, None)
}

/// Search with an optional exact active-label facet.
///
/// The free-text query continues to match artifact text and active label text by
/// literal case-insensitive substring. When `exact_label` is supplied, an
/// artifact must additionally carry that exact active label.
pub fn search_local_memory_filtered(
    events: &[EventEnvelope],
    query: &str,
    include_superseded: bool,
    exact_label: Option<&LocalMemoryLabel>,
) -> Result<Vec<LocalMemorySearchItem>, String> {
    search_local_memory_faceted(events, query, include_superseded, exact_label, None)
}

/// Search with optional exact active-label and source-conversation facets.
pub fn search_local_memory_faceted(
    events: &[EventEnvelope],
    query: &str,
    include_superseded: bool,
    exact_label: Option<&LocalMemoryLabel>,
    exact_source_conversation_id: Option<LocalConversationId>,
) -> Result<Vec<LocalMemorySearchItem>, String> {
    let artifacts = replay_local_memory_audit(events)?;
    let supersessions = replay_local_memory_supersession_audit(events)?;
    let needle = query.trim().to_lowercase();

    let mut items = artifacts
        .into_iter()
        .map(|artifact| {
            let labels = replay_active_local_memory_labels(events, artifact.memory_id)?;
            Ok((artifact, labels))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .filter_map(|(artifact, labels)| {
            let superseded_by = current_memory_successor(&supersessions, artifact.memory_id);
            if superseded_by.is_some() && !include_superseded {
                return None;
            }
            if exact_label.is_some_and(|required| !labels.iter().any(|label| label == required)) {
                return None;
            }
            if exact_source_conversation_id
                .is_some_and(|required| artifact.source_conversation_id != required)
            {
                return None;
            }
            let text_matches = artifact.text.to_lowercase().contains(&needle);
            let label_matches = labels
                .iter()
                .any(|label| label.as_str().to_lowercase().contains(&needle));
            if !needle.is_empty() && !text_matches && !label_matches {
                return None;
            }
            Some(LocalMemorySearchItem {
                memory_id: artifact.memory_id,
                source_conversation_id: artifact.source_conversation_id,
                text: artifact.text,
                recorded_sequence: artifact.recorded_sequence,
                labels,
                superseded_by,
            })
        })
        .collect::<Vec<_>>();

    items.sort_by_key(|item| std::cmp::Reverse(item.recorded_sequence));
    Ok(items)
}

/// Build deterministic exact-label facets for the currently eligible memory corpus.
///
/// Counts respect the superseded-history toggle and only active labels. This is a
/// pure read projection and grants no context authority.
pub fn local_memory_label_facets(
    events: &[EventEnvelope],
    include_superseded: bool,
) -> Result<Vec<LocalMemoryLabelFacet>, String> {
    let mut counts = BTreeMap::<LocalMemoryLabel, usize>::new();
    for item in search_local_memory_faceted(events, "", include_superseded, None, None)? {
        for label in item.labels {
            *counts.entry(label).or_default() += 1;
        }
    }

    Ok(counts
        .into_iter()
        .map(|(label, artifact_count)| LocalMemoryLabelFacet {
            label,
            artifact_count,
        })
        .collect())
}

/// Build deterministic source-conversation facets for the eligible corpus.
pub fn local_memory_source_facets(
    events: &[EventEnvelope],
    include_superseded: bool,
) -> Result<Vec<LocalMemorySourceFacet>, String> {
    let mut counts = BTreeMap::<LocalConversationId, usize>::new();
    for item in search_local_memory_faceted(events, "", include_superseded, None, None)? {
        *counts.entry(item.source_conversation_id).or_default() += 1;
    }

    Ok(counts
        .into_iter()
        .map(
            |(source_conversation_id, artifact_count)| LocalMemorySourceFacet {
                source_conversation_id,
                artifact_count,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_memory_audit::record_local_memory_artifact;
    use crate::local_memory_label_audit::{
        record_local_memory_label_added, record_local_memory_label_removed,
    };
    use crate::local_memory_supersession_audit::record_local_memory_superseded;
    use crate::{EventStore, MemoryEventStore};

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
    fn active_labels_participate_in_search_but_removed_labels_do_not() {
        let source = LocalConversationId::new();
        let memory_id = LocalMemoryId::new(1);
        let label = LocalMemoryLabel::new("project:chatarium").unwrap();
        let mut store = MemoryEventStore::default();
        record_local_memory_artifact(&mut store, memory_id, source, "unrelated text").unwrap();
        record_local_memory_label_added(&mut store, memory_id, &label).unwrap();

        let by_label = search_local_memory(store.events(), "CHATARIUM", false).unwrap();
        assert_eq!(by_label.len(), 1);
        assert_eq!(by_label[0].labels, vec![label.clone()]);

        record_local_memory_label_removed(&mut store, memory_id, &label).unwrap();
        assert!(
            search_local_memory(store.events(), "chatarium", false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exact_label_filter_is_conjunctive_with_free_text_search() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let project = LocalMemoryLabel::new("project:chatarium").unwrap();
        let topic = LocalMemoryLabel::new("topic:routing").unwrap();

        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(1),
            source,
            "routing architecture",
        )
        .unwrap();
        record_local_memory_artifact(
            &mut store,
            LocalMemoryId::new(2),
            source,
            "memory architecture",
        )
        .unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(1), &project).unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(1), &topic).unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(2), &project).unwrap();

        let project_results =
            search_local_memory_filtered(store.events(), "", false, Some(&project)).unwrap();
        assert_eq!(project_results.len(), 2);

        let routing_project =
            search_local_memory_filtered(store.events(), "routing", false, Some(&project)).unwrap();
        assert_eq!(routing_project.len(), 1);
        assert_eq!(routing_project[0].memory_id, LocalMemoryId::new(1));

        let routing_topic =
            search_local_memory_filtered(store.events(), "memory", false, Some(&topic)).unwrap();
        assert!(routing_topic.is_empty());
    }

    #[test]
    fn label_facets_count_only_active_labels_in_eligible_corpus() {
        let source = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        let project = LocalMemoryLabel::new("project:chatarium").unwrap();
        let stale = LocalMemoryLabel::new("stale").unwrap();

        for (id, text) in [(1, "old"), (2, "new"), (3, "other")] {
            record_local_memory_artifact(&mut store, LocalMemoryId::new(id), source, text).unwrap();
        }
        record_local_memory_label_added(&mut store, LocalMemoryId::new(1), &project).unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(1), &stale).unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(2), &project).unwrap();
        record_local_memory_label_added(&mut store, LocalMemoryId::new(3), &project).unwrap();
        record_local_memory_label_removed(&mut store, LocalMemoryId::new(1), &stale).unwrap();
        record_local_memory_superseded(&mut store, LocalMemoryId::new(1), LocalMemoryId::new(2))
            .unwrap();

        let current = local_memory_label_facets(store.events(), false).unwrap();
        assert_eq!(
            current,
            vec![LocalMemoryLabelFacet {
                label: project.clone(),
                artifact_count: 2,
            }]
        );

        let history = local_memory_label_facets(store.events(), true).unwrap();
        assert_eq!(
            history,
            vec![LocalMemoryLabelFacet {
                label: project,
                artifact_count: 3,
            }]
        );
    }

    #[test]
    fn source_conversation_filter_and_facets_are_exact() {
        let source_a = LocalConversationId::new();
        let source_b = LocalConversationId::new();
        let mut store = MemoryEventStore::default();

        record_local_memory_artifact(&mut store, LocalMemoryId::new(1), source_a, "one").unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(2), source_a, "two").unwrap();
        record_local_memory_artifact(&mut store, LocalMemoryId::new(3), source_b, "three").unwrap();

        let from_a =
            search_local_memory_faceted(store.events(), "", false, None, Some(source_a)).unwrap();
        assert_eq!(from_a.len(), 2);
        assert!(
            from_a
                .iter()
                .all(|item| item.source_conversation_id == source_a)
        );

        let facets = local_memory_source_facets(store.events(), false).unwrap();
        assert_eq!(facets.len(), 2);
        assert_eq!(
            facets
                .iter()
                .find(|facet| facet.source_conversation_id == source_a)
                .unwrap()
                .artifact_count,
            2
        );
        assert_eq!(
            facets
                .iter()
                .find(|facet| facet.source_conversation_id == source_b)
                .unwrap()
                .artifact_count,
            1
        );
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
