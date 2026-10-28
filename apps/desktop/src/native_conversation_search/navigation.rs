//! Exact identity/position selection, copy and reader navigation.
use super::*;

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
/// Enter without a selection uses the first row, matching conversation-open
/// navigation. An explicitly selected but stale/out-of-range row MUST NOT
/// silently copy the first result: it could belong to another conversation.
/// A selected title-only hit likewise cannot copy another row's text.
pub fn activate_copy_selection(
    matches: &[(LocalConversationId, Option<usize>)],
    selection: Option<usize>,
) -> Option<(LocalConversationId, usize)> {
    let &(conversation_id, message_index) = match selection {
        Some(index) => matches.get(index)?,
        None => matches.first()?,
    };
    message_index.map(|index| (conversation_id, index))
}

/// Select a native conversation by identity, not a sidebar row number.
/// A row reordering must keep the same owner highlighted and actionable.
pub fn move_identity_selection(
    matches: &[LocalConversationId],
    current: Option<LocalConversationId>,
    delta: isize,
) -> Option<LocalConversationId> {
    if matches.is_empty() {
        return None;
    }
    let position = current.and_then(|id| matches.iter().position(|candidate| *candidate == id));
    // Preserve ordinary arrow navigation when nothing was highlighted.
    // A remembered owner that vanished after archiving/filtering is different:
    // pressing an arrow is an explicit reselection, and must land on the first
    // or last visible row rather than accidentally skipping the first.
    if current.is_some() && position.is_none() {
        return if delta < 0 {
            matches.last().copied()
        } else {
            matches.first().copied()
        };
    }
    crate::local_archive_search::move_selection(position, matches.len(), delta)
        .and_then(|index| matches.get(index).copied())
}

/// Missing explicit identities fail closed instead of selecting the first row.
/// The first row remains the intentional default only when nothing is selected.
pub fn activate_identity_selection(
    matches: &[LocalConversationId],
    selected: Option<LocalConversationId>,
) -> Option<LocalConversationId> {
    match selected {
        Some(id) => matches.iter().find(|candidate| **candidate == id).copied(),
        None => matches.first().copied(),
    }
}

/// Return the currently highlighted sidebar row by conversation ID.
/// An explicit ID that disappeared must not fall back to another owner's row.
/// Only an unset selection intentionally defaults to the first visible row.
pub fn selected_identity_row(
    matches: &[LocalConversationId],
    selected: Option<LocalConversationId>,
) -> Option<usize> {
    match selected {
        Some(id) => matches.iter().position(|candidate| *candidate == id),
        None => (!matches.is_empty()).then_some(0),
    }
}

/// The same identity gate for copy, stage-as-memory, and reader navigation.
pub fn activate_identity_copy_selection(
    matches: &[(LocalConversationId, Option<usize>)],
    selected: Option<LocalConversationId>,
) -> Option<(LocalConversationId, usize)> {
    let &(owner, message_index) = match selected {
        Some(id) => matches.iter().find(|(owner, _)| *owner == id)?,
        None => matches.first()?,
    };
    message_index.map(|index| (owner, index))
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
    let text = crate::native_transcript_export::markdown(
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

/// A selected message is remembered by its durable source event sequence.
/// If an index projection reorders after an activity or assistant update,
/// resolve that exact event rather than the old hit ordinal.
pub fn selected_hit_ordinal(
    row: &NativeSearchRow,
    messages: &[DisplayMessage],
    source_sequence: Option<u64>,
) -> usize {
    source_sequence
        .and_then(|sequence| {
            row.matching_message_indices.iter().position(|index| {
                messages
                    .get(*index)
                    .is_some_and(|message| message.sequence == sequence)
            })
        })
        .unwrap_or(0)
}

/// A remembered event sequence must still exist in the currently matching
/// visible projection before it can be copied, staged, or opened. A stale
/// event must not be silently replaced by the first new matching message.
pub fn confirmed_hit_index(
    row: &NativeSearchRow,
    messages: &[DisplayMessage],
    source_sequence: Option<u64>,
) -> Option<usize> {
    match source_sequence {
        Some(sequence) => row.matching_message_indices.iter().copied().find(|index| {
            messages
                .get(*index)
                .is_some_and(|message| message.sequence == sequence)
        }),
        None => selected_message_index(row, 0),
    }
}

pub fn selected_hit_sequence(
    row: &NativeSearchRow,
    messages: &[DisplayMessage],
    ordinal: usize,
) -> Option<u64> {
    selected_message_index(row, ordinal)
        .and_then(|index| messages.get(index))
        .map(|message| message.sequence)
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
    if crate::offline_reader::search_hits(&message.text, query).is_empty() {
        return None;
    }
    Some(
        messages[..message_index]
            .iter()
            .map(|previous| crate::offline_reader::search_hits(&previous.text, query).len())
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
        .find_map(|message| crate::local_archive_search::local_snippet(message, &folded))
}

