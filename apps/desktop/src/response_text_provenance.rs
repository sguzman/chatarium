//! Evidence-only classification of unexpected protocol-like assistant text.
//!
//! This intentionally never rewrites user-visible output. A source-level
//! anomaly must be distinguished from a native reader/rendering anomaly before
//! attempting repair. Diagnostics contain only classes, not private text.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspectedTextMarker {
    ExplicitWireDelimiter,
    AdjacentRoleChannel,
}

impl SuspectedTextMarker {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ExplicitWireDelimiter => "explicit_wire_delimiter",
            Self::AdjacentRoleChannel => "adjacent_role_channel",
        }
    }
}

/// Inspect a bounded tail of already observed streaming text. Evaluating
/// the cumulative tail also detects markers split across source deltas.
#[must_use]
pub fn suspect_in_stream_tail(text: &str) -> Option<SuspectedTextMarker> {
    let tail = text
        .char_indices()
        .rev()
        .nth(511)
        .map_or(text, |(start, _)| &text[start..]);
    classify(tail)
}

fn classify(text: &str) -> Option<SuspectedTextMarker> {
    if ["<|im_start|>", "<|im_end|>", "<|im_sep|>", "<|meta_sep|>"]
        .iter()
        .any(|marker| text.contains(marker))
    {
        return Some(SuspectedTextMarker::ExplicitWireDelimiter);
    }
    // A role immediately followed by a channel with a short separator is
    // unusual in ordinary prose. The decision is deliberately diagnostic,
    // not a claim that the model or parser leaked a hidden channel.
    for role in ["system", "assistant", "developer"] {
        for channel in ["analysis", "commentary", "final"] {
            let mut remaining = text;
            while let Some(index) = remaining.find(role) {
                let after_role = &remaining[index + role.len()..];
                let prefix = after_role
                    .chars()
                    .take(4)
                    .take_while(|c| c.is_whitespace() || !c.is_alphanumeric())
                    .collect::<String>();
                if prefix.chars().any(|character| !character.is_whitespace())
                    && after_role[prefix.len()..].starts_with(channel)
                {
                    return Some(SuspectedTextMarker::AdjacentRoleChannel);
                }
                remaining = after_role;
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalSourceRelation {
    FinalMatchesStream,
    FinalEmptyUsesStream,
    FinalDiffersFromStream,
    NoStream,
}

/// The bridge supplies deltas and an optional final result independently.
/// Preserve disagreement as an observable fact without choosing a culprit.
#[must_use]
pub fn final_source_relation(streamed: &str, final_text: &str) -> FinalSourceRelation {
    if final_text.is_empty() && !streamed.is_empty() {
        FinalSourceRelation::FinalEmptyUsesStream
    } else if streamed.is_empty() {
        FinalSourceRelation::NoStream
    } else if streamed == final_text {
        FinalSourceRelation::FinalMatchesStream
    } else {
        FinalSourceRelation::FinalDiffersFromStream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_delimiters_split_between_deltas_without_changing_text() {
        let mut stream = String::from("normal answer <|im_st");
        assert_eq!(suspect_in_stream_tail(&stream), None);
        stream.push_str("art|> more answer");
        assert_eq!(
            suspect_in_stream_tail(&stream),
            Some(SuspectedTextMarker::ExplicitWireDelimiter)
        );
        assert!(stream.ends_with("more answer"));
    }

    #[test]
    fn adjacent_channel_markers_are_suspicious_but_ordinary_words_are_not() {
        assert_eq!(
            suspect_in_stream_tail("system commentary was discussed"),
            None
        );
        assert_eq!(
            suspect_in_stream_tail("text system::commentary token"),
            Some(SuspectedTextMarker::AdjacentRoleChannel)
        );
        assert_eq!(suspect_in_stream_tail("a final commentary"), None);
    }

    #[test]
    fn final_source_relation_preserves_uncertainty() {
        assert_eq!(
            final_source_relation("abc", "abc"),
            FinalSourceRelation::FinalMatchesStream
        );
        assert_eq!(
            final_source_relation("abc", ""),
            FinalSourceRelation::FinalEmptyUsesStream
        );
        assert_eq!(
            final_source_relation("abc", "abd"),
            FinalSourceRelation::FinalDiffersFromStream
        );
        assert_eq!(
            final_source_relation("", "server"),
            FinalSourceRelation::NoStream
        );
    }

    #[test]
    fn source_diagnostics_do_not_include_message_bodies() {
        let text = "secret content <|im_end|> secret suffix";
        let category = suspect_in_stream_tail(text).unwrap().label();
        assert_eq!(category, "explicit_wire_delimiter");
        assert!(!category.contains("secret"));
    }
}
