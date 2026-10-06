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
    TopLevelInstructions,
    ConversationDeveloperContext,
    DurableTranscript { sequence: u64 },
    CurrentDraft,
}

impl ContextSource {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::TopLevelInstructions => "top-level instructions".to_owned(),
            Self::ConversationDeveloperContext => "conversation developer context".to_owned(),
            Self::DurableTranscript { sequence } => {
                format!("durable transcript · event #{sequence}")
            }
            Self::CurrentDraft => "current draft preview".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InclusionDecision {
    Included,
    OmittedEmpty,
    ExcludedByPolicy,
}

impl InclusionDecision {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::OmittedEmpty => "omitted · empty",
            Self::ExcludedByPolicy => "excluded · policy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPolicy {
    pub include_instructions: bool,
    pub include_developer_context: bool,
    pub include_durable_transcript: bool,
    pub include_current_draft: bool,
}

impl ContextPolicy {
    #[must_use]
    pub const fn dispatch() -> Self {
        Self {
            include_instructions: true,
            include_developer_context: true,
            include_durable_transcript: true,
            include_current_draft: false,
        }
    }

    #[must_use]
    pub const fn preview() -> Self {
        Self {
            include_current_draft: true,
            ..Self::dispatch()
        }
    }

    const fn includes_source(self, source: &ContextSource) -> bool {
        match source {
            ContextSource::TopLevelInstructions => self.include_instructions,
            ContextSource::ConversationDeveloperContext => self.include_developer_context,
            ContextSource::DurableTranscript { .. } => self.include_durable_transcript,
            ContextSource::CurrentDraft => self.include_current_draft,
        }
    }
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
pub struct ContextInventoryItem {
    pub source: ContextSource,
    pub role: Option<String>,
    pub decision: InclusionDecision,
    pub utf8_bytes: usize,
    pub unicode_scalars: usize,
    pub lines: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextSizeLedger {
    pub included_items: usize,
    pub omitted_items: usize,
    pub utf8_bytes: usize,
    pub unicode_scalars: usize,
    pub lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPlan {
    pub instructions: Option<String>,
    pub messages: Vec<ComposedMessage>,
    pub inventory: Vec<ContextInventoryItem>,
    pub size: ContextSizeLedger,
}

impl ContextPlan {
    #[must_use]
    pub fn compose(
        policy: ContextPolicy,
        instructions: &str,
        developer_context: &str,
        transcript: impl IntoIterator<Item = TranscriptMessage>,
    ) -> Self {
        let mut plan = Self {
            instructions: None,
            messages: Vec::new(),
            inventory: Vec::new(),
            size: ContextSizeLedger::default(),
        };

        plan.add_optional_scalar(
            policy,
            ContextSource::TopLevelInstructions,
            None,
            instructions,
            |plan, text| plan.instructions = Some(text.to_owned()),
        );

        plan.add_optional_scalar(
            policy,
            ContextSource::ConversationDeveloperContext,
            Some("developer"),
            developer_context,
            |plan, text| {
                plan.messages.push(ComposedMessage {
                    role: "developer".to_owned(),
                    content: text.to_owned(),
                    source: ContextSource::ConversationDeveloperContext,
                });
            },
        );

        for message in transcript {
            let role = message.role.request_role();
            let decision = if policy.includes_source(&message.source) {
                InclusionDecision::Included
            } else {
                InclusionDecision::ExcludedByPolicy
            };
            plan.record_inventory(&message.source, Some(role), decision, &message.text);
            if decision == InclusionDecision::Included {
                plan.messages.push(ComposedMessage {
                    role: role.to_owned(),
                    content: message.text,
                    source: message.source,
                });
            }
        }

        plan
    }

    fn add_optional_scalar(
        &mut self,
        policy: ContextPolicy,
        source: ContextSource,
        role: Option<&str>,
        text: &str,
        include: impl FnOnce(&mut Self, &str),
    ) {
        let decision = if text.trim().is_empty() {
            InclusionDecision::OmittedEmpty
        } else if policy.includes_source(&source) {
            InclusionDecision::Included
        } else {
            InclusionDecision::ExcludedByPolicy
        };
        self.record_inventory(&source, role, decision, text);
        if decision == InclusionDecision::Included {
            include(self, text);
        }
    }

    fn record_inventory(
        &mut self,
        source: &ContextSource,
        role: Option<&str>,
        decision: InclusionDecision,
        text: &str,
    ) {
        let utf8_bytes = text.len();
        let unicode_scalars = text.chars().count();
        let lines = content_lines(text);

        if decision == InclusionDecision::Included {
            self.size.included_items = self.size.included_items.saturating_add(1);
            self.size.utf8_bytes = self.size.utf8_bytes.saturating_add(utf8_bytes);
            self.size.unicode_scalars = self.size.unicode_scalars.saturating_add(unicode_scalars);
            self.size.lines = self.size.lines.saturating_add(lines);
        } else {
            self.size.omitted_items = self.size.omitted_items.saturating_add(1);
        }

        self.inventory.push(ContextInventoryItem {
            source: source.clone(),
            role: role.map(ToOwned::to_owned),
            decision,
            utf8_bytes,
            unicode_scalars,
            lines,
        });
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
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::DurableTranscript { .. })
            })
            .count()
    }

    #[must_use]
    pub fn has_current_draft(&self) -> bool {
        self.inventory.iter().any(|item| {
            item.decision == InclusionDecision::Included
                && item.source == ContextSource::CurrentDraft
        })
    }

    #[must_use]
    pub fn has_developer_context(&self) -> bool {
        self.inventory.iter().any(|item| {
            item.decision == InclusionDecision::Included
                && item.source == ContextSource::ConversationDeveloperContext
        })
    }
}

fn content_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.bytes().filter(|byte| *byte == b'\n').count() + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_keeps_instructions_separate_and_developer_first() {
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
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
    fn empty_optional_context_is_visible_as_omitted_without_rewriting_transcript() {
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
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
        assert_eq!(
            plan.inventory[0].decision,
            InclusionDecision::OmittedEmpty
        );
        assert_eq!(
            plan.inventory[1].decision,
            InclusionDecision::OmittedEmpty
        );
        assert!(!plan.has_developer_context());
    }

    #[test]
    fn preview_includes_draft_but_dispatch_policy_mechanically_excludes_it() {
        let transcript = [
            TranscriptMessage::durable(TranscriptRole::Assistant, "prior", 7),
            TranscriptMessage::draft("pending"),
        ];

        let preview = ContextPlan::compose(ContextPolicy::preview(), "", "", transcript.clone());
        assert!(preview.has_current_draft());
        assert_eq!(
            preview.request_preview(Some("gpt-example")),
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

        let dispatch = ContextPlan::compose(ContextPolicy::dispatch(), "", "", transcript);
        assert!(!dispatch.has_current_draft());
        assert_eq!(
            dispatch.input_json(),
            json!([{"role": "assistant", "content": "prior"}])
        );
        assert_eq!(
            dispatch
                .inventory
                .iter()
                .find(|item| item.source == ContextSource::CurrentDraft)
                .unwrap()
                .decision,
            InclusionDecision::ExcludedByPolicy
        );
    }

    #[test]
    fn size_ledger_counts_exact_included_content_units_not_tokens() {
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
            "abc",
            "é",
            [TranscriptMessage::durable(
                TranscriptRole::User,
                "x\ny",
                42,
            )],
        );

        assert_eq!(
            plan.size,
            ContextSizeLedger {
                included_items: 3,
                omitted_items: 0,
                utf8_bytes: 8,
                unicode_scalars: 7,
                lines: 4,
            }
        );
    }

    #[test]
    fn provenance_retains_durable_sequence_without_entering_wire_input() {
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
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
