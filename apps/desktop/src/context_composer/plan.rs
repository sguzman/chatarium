//! Bounded context plans, inventories and request assembly.
use super::*;

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

    /// Compose one request under a strict byte ceiling. All durable history
    /// remains in the journal; excluded content is reported in the inventory.
    /// Any essential current message that does not fit fails closed, not cut.
    pub fn compose_bounded(
        policy: ContextPolicy,
        instructions: &str,
        developer_context: &str,
        transcript: impl IntoIterator<Item = TranscriptMessage>,
    ) -> Result<Self, String> {
        Self::compose(policy, instructions, developer_context, transcript)
            .within_utf8_budget(DEFAULT_CONTEXT_MAX_UTF8_BYTES)
    }

    /// Bound only the outgoing composed input, without changing its sources.
    /// UTF-8 is an auditable conservative size proxy, not a tokenizer.
    pub fn within_utf8_budget(mut self, max_bytes: usize) -> Result<Self, String> {
        if max_bytes == 0 {
            return Err("context budget must be positive".to_owned());
        }
        if self.size.utf8_bytes <= max_bytes {
            return Ok(self);
        }
        // The included non-instruction inventory has the same order as
        // messages. Preserve it to correlate omissions exactly, even if two
        // events happened to have equal content.
        let inventory_ids = self
            .inventory
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.decision == InclusionDecision::Included
                    && item.source != ContextSource::TopLevelInstructions
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if inventory_ids.len() != self.messages.len() {
            return Err("context message-to-inventory correspondence failed".to_owned());
        }

        let mut chosen = vec![false; self.messages.len()];
        let mut used = self.instructions.as_ref().map_or(0, String::len);
        // Developer context, one-shot choices, and active coordination
        // instructions must not vanish silently during a long conversation.
        for (index, message) in self.messages.iter().enumerate() {
            let mandatory = matches!(
                message.source,
                ContextSource::ConversationDeveloperContext
                    | ContextSource::LocalMemoryOneShot { .. }
                    | ContextSource::ControllerContinuation { .. }
                    | ContextSource::ControllerCoordination { .. }
                    | ContextSource::CoordinationActivation { .. }
                    | ContextSource::CurrentDraft
            );
            if mandatory {
                chosen[index] = true;
                used = used.saturating_add(message.content.len());
            }
        }
        // Preserve the latest authored user message and its immediate durable
        // predecessor. A request must never become a reply to the wrong turn.
        let recent_durable = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| {
                matches!(message.source, ContextSource::DurableTranscript { .. })
            })
            .map(|(index, _)| index)
            .take(2)
            .collect::<Vec<_>>();
        for index in recent_durable {
            if !chosen[index] {
                chosen[index] = true;
                used = used.saturating_add(self.messages[index].content.len());
            }
        }
        if used > max_bytes {
            return Err(format!(
                "current turn, explicit context, or instructions exceed {max_bytes}-byte context budget; refusing silent truncation"
            ));
        }

        // Preserve a short contiguous recent conversation window BEFORE
        // selecting verified historical memory. Otherwise sixteen large recent
        // messages can consume the full budget and silently crowd out the
        // rolling checkpoint needed to recover older decisions. After memory
        // selection, fill the remaining budget with up to twelve more recent
        // turns, then other evidence and older turns. Without checkpoints this
        // preserves the existing 16-message recency ordering.
        // All evidence remains whole with its original source provenance.
        let recent = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| {
                matches!(message.source, ContextSource::DurableTranscript { .. })
            })
            .map(|(index, _)| index)
            .take(16)
            .collect::<Vec<_>>();
        let checkpoints = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| {
                matches!(message.source, ContextSource::OpenVikingCheckpoint { .. })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let other = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| {
                !matches!(
                    message.source,
                    ContextSource::DurableTranscript { .. }
                        | ContextSource::OpenVikingCheckpoint { .. }
                )
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let older = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, message)| {
                matches!(message.source, ContextSource::DurableTranscript { .. })
            })
            .map(|(index, _)| index)
            .skip(16)
            .collect::<Vec<_>>();
        for index in recent
            .iter()
            .take(4)
            .copied()
            .chain(checkpoints)
            .chain(recent.iter().skip(4).copied())
            .chain(other)
            .chain(older)
        {
            if chosen[index] {
                continue;
            }
            let size = self.messages[index].content.len();
            if size <= max_bytes - used {
                chosen[index] = true;
                used += size;
            }
        }

        for (index, inventory_id) in inventory_ids.iter().copied().enumerate() {
            if !chosen[index] {
                self.inventory[inventory_id].decision = InclusionDecision::ExcludedByBudget;
            }
        }
        self.messages = self
            .messages
            .into_iter()
            .enumerate()
            .filter_map(|(index, message)| chosen[index].then_some(message))
            .collect();
        self.size = ContextSizeLedger::default();
        for item in &self.inventory {
            if item.decision == InclusionDecision::Included {
                self.size.included_items += 1;
                self.size.utf8_bytes += item.utf8_bytes;
                self.size.unicode_scalars += item.unicode_scalars;
                self.size.lines += item.lines;
            } else {
                self.size.omitted_items += 1;
            }
        }
        debug_assert_eq!(self.size.utf8_bytes, used);
        Ok(self)
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

    #[must_use]
    pub fn routed_context_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::RoutedInbox { .. })
            })
            .count()
    }

    #[must_use]
    pub fn tool_result_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::ToolResult { .. })
            })
            .count()
    }

    #[must_use]
    pub fn local_memory_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::LocalMemory { .. })
            })
            .count()
    }

    #[must_use]
    pub fn local_memory_one_shot_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::LocalMemoryOneShot { .. })
            })
            .count()
    }

    #[must_use]
    pub fn openviking_checkpoint_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::OpenVikingCheckpoint { .. })
            })
            .count()
    }

    #[must_use]
    pub fn controller_worker_result_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::ControllerWorkerResult { .. })
            })
            .count()
    }

    #[must_use]
    pub fn controller_coordination_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(item.source, ContextSource::ControllerCoordination { .. })
            })
            .count()
    }

    #[must_use]
    pub fn controller_coordination_result_count(&self) -> usize {
        self.inventory
            .iter()
            .filter(|item| {
                item.decision == InclusionDecision::Included
                    && matches!(
                        item.source,
                        ContextSource::ControllerCoordinationResult { .. }
                    )
            })
            .count()
    }
}

fn content_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.bytes().filter(|byte| *byte == b'\n').count() + 1
    }
}
