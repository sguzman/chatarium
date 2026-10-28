//! Native query matching and excerpt detection.
use super::*;

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
pub(super) fn find_match_with_position<'a>(
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
            crate::local_archive_search::local_snippet(message, &needle)
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
