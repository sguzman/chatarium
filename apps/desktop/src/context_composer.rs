//! Typed local context composition for SIWC Responses requests.
//!
//! This module is intentionally transport-agnostic. It owns the ordered local
//! context plan that higher-level behavior/lifecycle code can inspect before the
//! SIWC bridge strips local provenance and sends only the Responses request shape.

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

impl TranscriptRole {
    const fn request_role(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextSource {
    ConversationDeveloperContext,
    DurableTranscript { sequence: u64 },
    CurrentDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub text: String,
    pub source: ContextSource,
}

impl TranscriptMessage {
    #[must_use]
    pub fn durable(role: TranscriptRole, text: impl Into<String>, sequence: u64) -> Self {
        Self {
            role,
            text: text.into(),
            source: ContextSource::DurableTranscript { sequence },
        }
    }

    #[must_use]
    pub fn draft(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::User,
            text: text.into(),
            source: ContextSource::CurrentDraft,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedMessage {
    pub role: String,
    pub content: String,
    pub source: ContextSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPlan {
    pub instructions: Option<String>,
    pub messages: Vec<ComposedMessage>,
}

impl ContextPlan {
    #[must_use]
    pub fn compose(
        instructions: &str,
        developer_context: &str,
        transcript: impl IntoIterator<Item = TranscriptMessage>,
    ) -> Self {
        let instructions = (!instructions.trim().is_empty()).then(|| instructions.to_owned());
        let mut messages = Vec::new();

        if !developer_context.trim().is_empty() {
            messages.push(ComposedMessage {
                role: "developer".to_owned(),
                content: developer_context.to_owned(),
                source: ContextSource::ConversationDeveloperContext,
            });
        }

        messages.extend(transcript.into_iter().map(|message| ComposedMessage {
            role: message.role.request_role().to_owned(),
            content: message.text,
            source: message.source,
        }));

        Self {
            instructions,
            messages,
        }
    }

    #[must_use]
    pub fn input_json(&self) -> Value {
        Value::Array(
            self.messages
                .iter()
                .map(|message| {
                    json!({
                        "role": message.role,
                        "content": message.content,
                    })
                })
                .collect(),
        )
    }

    #[must_use]
    pub fn request_preview(&self, model: Option<&str>) -> Value {
        let mut request = json!({
            "model": model,
            "input": self.input_json(),
            "store": false,
            "stream": true,
        });
        if let Some(instructions) = self.instructions.as_ref() {
            request
                .as_object_mut()
                .expect("request preview is an object")
                .insert(
                    "instructions".to_owned(),
                    Value::String(instructions.clone()),
                );
        }
        request
    }

    #[must_use]
    pub fn durable_transcript_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| {
                matches!(
                    message.source,
                    ContextSource::DurableTranscript { .. }
                )
            })
            .count()
    }

    #[must_use]
    pub fn has_current_draft(&self) -> bool {
        self.messages
            .iter()
            .any(|message| message.source == ContextSource::CurrentDraft)
    }

    #[must_use]
    pub fn has_developer_context(&self) -> bool {
        self.messages
            .iter()
            .any(|message| message.source == ContextSource::ConversationDeveloperContext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_keeps_instructions_separate_and_developer_first() {
        let plan = ContextPlan::compose(
            "answer compactly",
            "local behavior",
            [
                TranscriptMessage::durable(TranscriptRole::User, "one", 1),
                TranscriptMessage::durable(TranscriptRole::Assistant, "two", 2),
                TranscriptMessage::durable(TranscriptRole::User, "three", 3),
            ],
        );

        assert_eq!(plan.instructions.as_deref(), Some("answer compactly"));
        assert_eq!(
            plan.input_json(),
            json!([
                {"role": "developer", "content": "local behavior"},
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"},
            ])
        );
        assert_eq!(plan.durable_transcript_count(), 3);
        assert!(plan.has_developer_context());
        assert!(!plan.has_current_draft());
    }

    #[test]
    fn empty_optional_context_is_omitted_without_rewriting_transcript() {
        let plan = ContextPlan::compose(
            "   ",
            "\n",
            [TranscriptMessage::durable(
                TranscriptRole::User,
                " exact text ",
                9,
            )],
        );

        assert_eq!(plan.instructions, None);
        assert_eq!(
            plan.input_json(),
            json!([{"role": "user", "content": " exact text "}])
        );
        assert!(!plan.has_developer_context());
    }

    #[test]
    fn preview_marks_draft_locally_but_does_not_leak_provenance_to_request() {
        let plan = ContextPlan::compose(
            "",
            "",
            [
                TranscriptMessage::durable(TranscriptRole::Assistant, "prior", 7),
                TranscriptMessage::draft("pending"),
            ],
        );

        assert!(plan.has_current_draft());
        assert_eq!(
            plan.request_preview(Some("gpt-example")),
            json!({
                "model": "gpt-example",
                "input": [
                    {"role": "assistant", "content": "prior"},
                    {"role": "user", "content": "pending"},
                ],
                "store": false,
                "stream": true,
            })
        );
    }

    #[test]
    fn provenance_retains_durable_sequence_without_entering_wire_input() {
        let plan = ContextPlan::compose(
            "",
            "",
            [TranscriptMessage::durable(
                TranscriptRole::User,
                "hello",
                42,
            )],
        );

        assert_eq!(
            plan.messages[0].source,
            ContextSource::DurableTranscript { sequence: 42 }
        );
        assert_eq!(
            plan.input_json(),
            json!([{"role": "user", "content": "hello"}])
        );
    }
}
