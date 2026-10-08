//! Rebuildable, local-only search over the observed catalog and visible mirror projections.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMirrorState {
    Mirrored,
    Partial,
    NotMirrored,
    TransientFailure,
    RateLimited,
    StructuralFailure,
}

impl ArchiveMirrorState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mirrored => "MIRRORED",
            Self::Partial => "MIRRORED · PARTIAL",
            Self::NotMirrored => "NOT MIRRORED",
            Self::TransientFailure => "TRANSIENT FAILURE",
            Self::RateLimited => "RATE LIMITED",
            Self::StructuralFailure => "STRUCTURAL FAILURE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveSearchMode {
    AllLocalData,
    Titles,
    MirroredTranscriptText,
}

impl ArchiveSearchMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::AllLocalData => "ALL LOCAL DATA",
            Self::Titles => "TITLES",
            Self::MirroredTranscriptText => "MIRRORED TRANSCRIPT TEXT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveStateFilter {
    All,
    Mirrored,
    Partial,
    NotMirrored,
    TransientFailure,
    RateLimited,
    StructuralFailure,
}

impl ArchiveStateFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Mirrored => "MIRRORED",
            Self::Partial => "MIRRORED · PARTIAL",
            Self::NotMirrored => "NOT MIRRORED",
            Self::TransientFailure => "TRANSIENT FAILURE",
            Self::RateLimited => "RATE LIMITED",
            Self::StructuralFailure => "STRUCTURAL FAILURE",
        }
    }

    fn accepts(self, state: ArchiveMirrorState) -> bool {
        match self {
            Self::All => true,
            Self::Mirrored => state == ArchiveMirrorState::Mirrored,
            Self::Partial => state == ArchiveMirrorState::Partial,
            Self::NotMirrored => state == ArchiveMirrorState::NotMirrored,
            Self::TransientFailure => state == ArchiveMirrorState::TransientFailure,
            Self::RateLimited => state == ArchiveMirrorState::RateLimited,
            Self::StructuralFailure => state == ArchiveMirrorState::StructuralFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMatchKind {
    Title,
    LocalTranscript,
}

impl ArchiveMatchKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "TITLE",
            Self::LocalTranscript => "LOCAL TRANSCRIPT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSearchDocument {
    pub catalog_index: usize,
    pub title: String,
    pub state: ArchiveMirrorState,
    /// Already-projected visible user/assistant messages only.
    pub visible_messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSearchResult {
    pub document_index: usize,
    pub catalog_index: usize,
    pub kind: ArchiveMatchKind,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArchiveSearchIndex {
    documents: Vec<ArchiveSearchDocument>,
}

impl LocalArchiveSearchIndex {
    pub fn new(documents: Vec<ArchiveSearchDocument>) -> Self {
        Self { documents }
    }

    pub fn catalog_entries(&self) -> usize {
        self.documents.len()
    }

    pub fn mirrored_transcripts(&self) -> usize {
        self.documents
            .iter()
            .filter(|document| {
                matches!(
                    document.state,
                    ArchiveMirrorState::Mirrored | ArchiveMirrorState::Partial
                )
            })
            .count()
    }

    pub fn visible_messages(&self) -> usize {
        self.documents
            .iter()
            .map(|document| document.visible_messages.len())
            .sum()
    }

    pub fn documents(&self) -> &[ArchiveSearchDocument] {
        &self.documents
    }

    pub fn search(
        &self,
        query: &str,
        mode: ArchiveSearchMode,
        filter: ArchiveStateFilter,
    ) -> Vec<ArchiveSearchResult> {
        let query = query.trim();
        let folded_query = query.to_lowercase();
        let mut results = Vec::new();

        for (document_index, document) in self.documents.iter().enumerate() {
            if !filter.accepts(document.state) {
                continue;
            }

            let title_match =
                !folded_query.is_empty() && document.title.to_lowercase().contains(&folded_query);
            if query.is_empty()
                || (title_match && mode != ArchiveSearchMode::MirroredTranscriptText)
            {
                results.push(ArchiveSearchResult {
                    document_index,
                    catalog_index: document.catalog_index,
                    kind: ArchiveMatchKind::Title,
                    snippet: None,
                });
            }

            if mode == ArchiveSearchMode::Titles || query.is_empty() {
                continue;
            }
            if !matches!(
                document.state,
                ArchiveMirrorState::Mirrored | ArchiveMirrorState::Partial
            ) {
                continue;
            }
            for message in &document.visible_messages {
                if let Some(snippet) = local_snippet(message, &folded_query) {
                    results.push(ArchiveSearchResult {
                        document_index,
                        catalog_index: document.catalog_index,
                        kind: ArchiveMatchKind::LocalTranscript,
                        snippet: Some(snippet),
                    });
                    break;
                }
            }
        }

        results
    }
}

pub fn move_selection(current: Option<usize>, result_count: usize, delta: isize) -> Option<usize> {
    if result_count == 0 {
        return None;
    }
    let current = current.unwrap_or(0).min(result_count - 1) as isize;
    Some((current + delta).rem_euclid(result_count as isize) as usize)
}

pub fn activate_selection(
    results: &[ArchiveSearchResult],
    selection: Option<usize>,
) -> Option<ArchiveSearchResult> {
    results
        .get(selection.unwrap_or(0))
        .cloned()
        .or_else(|| results.first().cloned())
}

/// Extract a bounded local excerpt around a case-folded match.
///
/// Unicode lowercasing may expand or contextualize characters, so folded
/// offsets must never be used to slice the original UTF-8 text directly.
pub fn local_snippet(text: &str, folded_query: &str) -> Option<String> {
    if folded_query.is_empty() {
        return None;
    }
    let folded_text = text.to_lowercase();
    let match_start = folded_text.find(folded_query)?;
    let match_end = match_start + folded_query.len();

    let mut folded_offset = 0;
    let mut original_start = None;
    let mut original_end = None;
    for (byte_start, ch) in text.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        for lower in ch.to_lowercase() {
            let folded_end = folded_offset + lower.len_utf8();
            if original_start.is_none() && match_start < folded_end {
                original_start = Some(byte_start);
            }
            if folded_offset < match_end {
                original_end = Some(byte_end);
            }
            folded_offset = folded_end;
        }
        if folded_offset >= match_end {
            break;
        }
    }
    let (Some(original_start), Some(original_end)) = (original_start, original_end) else {
        return None;
    };
    let start = text[..original_start]
        .char_indices()
        .rev()
        .nth(48)
        .map(|(offset, _)| offset)
        .unwrap_or(0);
    let end = text[original_end..]
        .char_indices()
        .nth(96)
        .map(|(offset, _)| original_end + offset)
        .unwrap_or(text.len());
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < text.len() { "…" } else { "" };
    let excerpt = text[start..end]
        .replace('\r', "")
        .replace('\n', " ↵ ");
    Some(format!("{prefix}{excerpt}{suffix}"))
}

impl fmt::Display for ArchiveSearchMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> LocalArchiveSearchIndex {
        LocalArchiveSearchIndex::new(vec![
            ArchiveSearchDocument {
                catalog_index: 0,
                title: "Linux notes".to_owned(),
                state: ArchiveMirrorState::Partial,
                visible_messages: vec!["Visible Linux transcript".to_owned()],
            },
            ArchiveSearchDocument {
                catalog_index: 1,
                title: "Unmirrored title".to_owned(),
                state: ArchiveMirrorState::NotMirrored,
                visible_messages: vec![],
            },
            ArchiveSearchDocument {
                catalog_index: 2,
                title: "Other".to_owned(),
                state: ArchiveMirrorState::RateLimited,
                visible_messages: vec!["Linux must not be searchable here".to_owned()],
            },
        ])
    }

    #[test]
    fn title_and_case_insensitive_search_cover_catalog_only() {
        let results = index().search("LINUX", ArchiveSearchMode::Titles, ArchiveStateFilter::All);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].catalog_index, 0);
        assert_eq!(results[0].kind, ArchiveMatchKind::Title);
    }

    #[test]
    fn transcript_search_excludes_unmirrored_and_non_mirrored_states() {
        let results = index().search(
            "linux",
            ArchiveSearchMode::MirroredTranscriptText,
            ArchiveStateFilter::All,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, ArchiveMatchKind::LocalTranscript);
        assert_eq!(results[0].catalog_index, 0);
    }

    #[test]
    fn all_local_data_composes_with_partial_filter() {
        let results = index().search(
            "linux",
            ArchiveSearchMode::AllLocalData,
            ArchiveStateFilter::Partial,
        );
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.catalog_index == 0));
    }

    #[test]
    fn every_state_filter_is_distinct() {
        for (filter, expected) in [
            (ArchiveStateFilter::Mirrored, ArchiveMirrorState::Mirrored),
            (ArchiveStateFilter::Partial, ArchiveMirrorState::Partial),
            (
                ArchiveStateFilter::NotMirrored,
                ArchiveMirrorState::NotMirrored,
            ),
            (
                ArchiveStateFilter::TransientFailure,
                ArchiveMirrorState::TransientFailure,
            ),
            (
                ArchiveStateFilter::RateLimited,
                ArchiveMirrorState::RateLimited,
            ),
            (
                ArchiveStateFilter::StructuralFailure,
                ArchiveMirrorState::StructuralFailure,
            ),
        ] {
            let document = ArchiveSearchDocument {
                catalog_index: 7,
                title: "match".to_owned(),
                state: expected,
                visible_messages: vec![],
            };
            let results = LocalArchiveSearchIndex::new(vec![document]).search(
                "match",
                ArchiveSearchMode::Titles,
                filter,
            );
            assert_eq!(results.len(), 1);
        }
    }

    #[test]
    fn keyboard_selection_wraps_and_enter_can_target_result() {
        assert_eq!(move_selection(None, 3, 1), Some(1));
        assert_eq!(move_selection(Some(2), 3, 1), Some(0));
        assert_eq!(move_selection(Some(0), 3, -1), Some(2));
        assert_eq!(move_selection(Some(0), 0, 1), None);
        let results = index().search("linux", ArchiveSearchMode::Titles, ArchiveStateFilter::All);
        assert_eq!(
            activate_selection(&results, Some(0)),
            Some(results[0].clone())
        );
    }

    #[test]
    fn rebuilding_from_the_same_durable_projection_is_structurally_stable() {
        let original = index();
        let rebuilt = LocalArchiveSearchIndex::new(original.documents().to_vec());
        assert_eq!(original, rebuilt);
        assert_eq!(
            original.search(
                "linux",
                ArchiveSearchMode::AllLocalData,
                ArchiveStateFilter::All
            ),
            rebuilt.search(
                "linux",
                ArchiveSearchMode::AllLocalData,
                ArchiveStateFilter::All
            )
        );
    }

    #[test]
    fn unicode_expanding_lowercase_never_slices_interior_utf8() {
        assert_eq!(
            local_snippet("İstanbul is busy", "i"),
            Some("İstanbul is busy".to_owned())
        );
        assert_eq!(
            local_snippet("EL NIÑO\nsueña", "niño"),
            Some("EL NIÑO ↵ sueña".to_owned())
        );
        assert!(local_snippet("México", "other").is_none());
    }

    #[test]
    fn long_snippets_omit_remote_neighbors_and_bound_context() {
        let text = format!("{}MIDDLE{}!", "a".repeat(150), "z".repeat(300));
        let excerpt = local_snippet(&text, "middle").unwrap();
        assert!(excerpt.contains("MIDDLE"));
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() < 180);
    }

    #[test]
    fn snippets_are_compact_and_local() {
        let snippet = local_snippet("prefix visible target suffix", "target").unwrap();
        assert!(snippet.contains("target"));
        assert!(!snippet.contains("hidden"));
    }
}
