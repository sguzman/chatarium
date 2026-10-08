//! Search semantics for native Chatarium conversations (not imported remote mirrors).
//! Search only locally projected user/assistant text and the local workspace title.
//! The journal and metadata catalog remain authoritative; the query is ephemeral.

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
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || title.to_lowercase().contains(&needle) {
        return Some(NativeSearchMatch::Title);
    }
    visible_messages
        .into_iter()
        .any(|message| message.to_lowercase().contains(&needle))
        .then_some(NativeSearchMatch::Message)
}

#[cfg(test)]
mod tests {
    use super::*;

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
