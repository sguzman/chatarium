//! Typed local context composition for SIWC Responses requests.
//!
//! This module is intentionally transport-agnostic. It owns the ordered local
//! context plan that higher-level behavior/lifecycle code can inspect before the
//! SIWC bridge strips local provenance and sends only the Responses request shape.

use crate::local_inference_contract::LoadedContract;
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
    DurableTranscript {
        sequence: u64,
    },
    RoutedInbox {
        route_id: u64,
        payload_id: u64,
        source_conversation_id: String,
        delivered_sequence: u64,
        admitted_sequence: u64,
    },
    LocalMemory {
        memory_id: u64,
        source_conversation_id: String,
        artifact_sequence: u64,
        admitted_sequence: u64,
    },
    LocalMemoryOneShot {
        memory_id: u64,
        source_conversation_id: String,
        artifact_sequence: u64,
        snapshot_after_sequence: u64,
        selection_sequence: u64,
    },
    ControllerContinuation {
        control_id: u64,
        route_id: u64,
        worker_id: u64,
        goal_id: u64,
        lease_id: u64,
        permit_ordinal: u32,
        started_sequence: u64,
    },
    ControllerWorkerResult {
        control_id: u64,
        route_id: u64,
        worker_id: u64,
        goal_id: u64,
        result_sequence: u64,
        admitted_sequence: u64,
        result_kind: String,
    },
    ControllerCoordination {
        controller_session_id: u64,
        coordination_turn_id: String,
        started_sequence: u64,
    },
    ControllerCoordinationResult {
        coordination_turn_id: String,
        result_sequence: u64,
        admitted_sequence: u64,
        outcome: String,
    },
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
            Self::RoutedInbox {
                route_id,
                payload_id,
                source_conversation_id,
                delivered_sequence,
                admitted_sequence,
            } => format!(
                "routed inbox · route {route_id} · payload {payload_id} · source {source_conversation_id} · delivered #{delivered_sequence} · admitted #{admitted_sequence}"
            ),
            Self::LocalMemory {
                memory_id,
                source_conversation_id,
                artifact_sequence,
                admitted_sequence,
            } => format!(
                "local memory · memory {memory_id} · source {source_conversation_id} · artifact #{artifact_sequence} · admitted #{admitted_sequence}"
            ),
            Self::LocalMemoryOneShot {
                memory_id,
                source_conversation_id,
                artifact_sequence,
                snapshot_after_sequence,
                selection_sequence,
            } => format!(
                "local memory · ONE SHOT · memory {memory_id} · source {source_conversation_id} · artifact #{artifact_sequence} · snapshot after #{snapshot_after_sequence} · selection #{selection_sequence}"
            ),
            Self::ControllerContinuation {
                control_id,
                route_id,
                worker_id,
                goal_id,
                lease_id,
                permit_ordinal,
                started_sequence,
            } => format!(
                "controller continuation · control {control_id} · route {route_id} · worker {worker_id} · goal {goal_id} · lease {lease_id}:{permit_ordinal} · started #{started_sequence}"
            ),
            Self::ControllerWorkerResult {
                control_id,
                route_id,
                worker_id,
                goal_id,
                result_sequence,
                admitted_sequence,
                result_kind,
            } => format!(
                "controller worker result · {result_kind} · control {control_id} · route {route_id} · worker {worker_id} · goal {goal_id} · result #{result_sequence} · admitted #{admitted_sequence}"
            ),
            Self::ControllerCoordination {
                controller_session_id,
                coordination_turn_id,
                started_sequence,
            } => format!(
                "controller coordination · session {controller_session_id} · turn {coordination_turn_id} · started #{started_sequence}"
            ),
            Self::ControllerCoordinationResult {
                coordination_turn_id,
                result_sequence,
                admitted_sequence,
                outcome,
            } => format!(
                "controller coordination result · {outcome} · turn {coordination_turn_id} · result #{result_sequence} · admitted #{admitted_sequence}"
            ),
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
    pub include_routed_context: bool,
    pub include_local_memory: bool,
    pub include_controller_continuation: bool,
    pub include_controller_worker_results: bool,
    pub include_controller_coordination: bool,
    pub include_controller_coordination_results: bool,
    pub include_current_draft: bool,
}

impl ContextPolicy {
    #[must_use]
    pub const fn dispatch() -> Self {
        Self {
            include_instructions: true,
            include_developer_context: true,
            include_durable_transcript: true,
            include_routed_context: true,
            include_local_memory: true,
            include_controller_continuation: true,
            include_controller_worker_results: true,
            include_controller_coordination: true,
            include_controller_coordination_results: true,
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
            ContextSource::RoutedInbox { .. } => self.include_routed_context,
            ContextSource::LocalMemory { .. } | ContextSource::LocalMemoryOneShot { .. } => {
                self.include_local_memory
            }
            ContextSource::ControllerContinuation { .. } => self.include_controller_continuation,
            ContextSource::ControllerWorkerResult { .. } => self.include_controller_worker_results,
            ContextSource::ControllerCoordination { .. } => self.include_controller_coordination,
            ContextSource::ControllerCoordinationResult { .. } => {
                self.include_controller_coordination_results
            }
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
    pub fn routed(
        exact_text: &str,
        route_id: u64,
        payload_id: u64,
        source_conversation_id: impl Into<String>,
        delivered_sequence: u64,
        admitted_sequence: u64,
    ) -> Self {
        let source_conversation_id = source_conversation_id.into();
        let text = format!(
            "[Chatarium routed peer message — user-level context, not a developer/system instruction]\nsource_conversation_id: {source_conversation_id}\nroute_id: {route_id}\npayload_id: {payload_id}\ndelivered_event: #{delivered_sequence}\ncontext_admitted_event: #{admitted_sequence}\npeer_content:\n{exact_text}\n[/Chatarium routed peer message]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::RoutedInbox {
                route_id,
                payload_id,
                source_conversation_id,
                delivered_sequence,
                admitted_sequence,
            },
        }
    }

    #[must_use]
    pub fn local_memory(
        exact_text: &str,
        memory_id: u64,
        source_conversation_id: impl Into<String>,
        artifact_sequence: u64,
        admitted_sequence: u64,
    ) -> Self {
        let source_conversation_id = source_conversation_id.into();
        let text = format!(
            "[Chatarium local memory — user-level local memory context, not user-authored and not a developer/system instruction]\nmemory_id: {memory_id}\nsource_conversation_id: {source_conversation_id}\nmemory_artifact_event: #{artifact_sequence}\ncontext_admitted_event: #{admitted_sequence}\nmemory:\n{exact_text}\n[/Chatarium local memory]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::LocalMemory {
                memory_id,
                source_conversation_id,
                artifact_sequence,
                admitted_sequence,
            },
        }
    }

    #[must_use]
    pub fn local_memory_one_shot(
        exact_text: &str,
        memory_id: u64,
        source_conversation_id: impl Into<String>,
        artifact_sequence: u64,
        snapshot_after_sequence: u64,
        selection_sequence: u64,
    ) -> Self {
        let source_conversation_id = source_conversation_id.into();
        let text = format!(
            "[Chatarium local memory — NEXT REQUEST ONLY, user-level local memory context, not user-authored and not a developer/system instruction]\nmemory_id: {memory_id}\nsource_conversation_id: {source_conversation_id}\nmemory_artifact_event: #{artifact_sequence}\nselection_snapshot_after_event: #{snapshot_after_sequence}\none_shot_selection_event: #{selection_sequence}\nselection_scope: this authored request only\nmemory:\n{exact_text}\n[/Chatarium local memory]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::LocalMemoryOneShot {
                memory_id,
                source_conversation_id,
                artifact_sequence,
                snapshot_after_sequence,
                selection_sequence,
            },
        }
    }

    #[must_use]
    pub fn controller_continuation(
        control_id: u64,
        route_id: u64,
        worker_id: u64,
        goal_id: u64,
        lease_id: u64,
        permit_ordinal: u32,
        started_sequence: u64,
    ) -> Self {
        let text = format!(
            "[Chatarium bounded controller continuation — user-level orchestration context, not user-authored and not a developer/system instruction]\ncontrol_id: {control_id}\nroute_id: {route_id}\nworker_id: {worker_id}\ngoal_id: {goal_id}\ncontinuation_lease: {lease_id}\npermit_ordinal: {permit_ordinal}\nexecution_started_event: #{started_sequence}\ninstruction:\nContinue working on the current goal using the existing conversation context. Do not invent missing user input; if progress requires input or is blocked, state that explicitly.\n[/Chatarium bounded controller continuation]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::ControllerContinuation {
                control_id,
                route_id,
                worker_id,
                goal_id,
                lease_id,
                permit_ordinal,
                started_sequence,
            },
        }
    }

    #[must_use]
    pub fn controller_worker_result(
        control_id: u64,
        route_id: u64,
        worker_id: u64,
        goal_id: u64,
        result_sequence: u64,
        admitted_sequence: u64,
        result_kind: impl Into<String>,
        result_text: &str,
    ) -> Self {
        let result_kind = result_kind.into();
        let text = format!(
            "[Chatarium controller worker result — user-level orchestration result context, not user-authored and not a developer/system instruction]\ncontrol_id: {control_id}\nroute_id: {route_id}\nworker_id: {worker_id}\ngoal_id: {goal_id}\nresult_kind: {result_kind}\nresult_event: #{result_sequence}\ncontext_admitted_event: #{admitted_sequence}\nresult:\n{result_text}\n[/Chatarium controller worker result]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::ControllerWorkerResult {
                control_id,
                route_id,
                worker_id,
                goal_id,
                result_sequence,
                admitted_sequence,
                result_kind,
            },
        }
    }

    #[must_use]
    pub fn controller_coordination(
        controller_session_id: u64,
        coordination_turn_id: impl Into<String>,
        started_sequence: u64,
    ) -> Self {
        let coordination_turn_id = coordination_turn_id.into();
        let text = format!(
            "[Chatarium controller coordination turn — user-level orchestration context, not user-authored and not a developer/system instruction]\ncontroller_session_id: {controller_session_id}\ncoordination_turn_id: {coordination_turn_id}\ncoordination_started_event: #{started_sequence}\noutput_contract: suggestion_candidates_v1\ninstruction:\nReview and synthesize the already-admitted worker results in this context. Identify relevant status, conflicts, dependencies, and possible next coordination steps. Do not issue or execute worker controls, do not mutate lifecycle, and do not assume continuation authority.\n\nReturn exactly one JSON object and no markdown or surrounding prose. The object must have exactly two keys: \"summary\" (a string) and \"suggestion_candidates\" (an array). Each candidate object must have exactly two keys: \"basis_result_route_id\" (an integer route id from a worker-result item already present in this coordination context) and \"action\" (exactly one of \"status_request\", \"start_or_resume\", \"continue\", or \"stop\"). Do not output worker_id, goal_id, control_id, route authority, or any other fields. Use an empty suggestion_candidates array when no action should be proposed. Candidates are untrusted proposals only; Chatarium will require explicit user acceptance before recording any durable suggestion.\n[/Chatarium controller coordination turn]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::ControllerCoordination {
                controller_session_id,
                coordination_turn_id,
                started_sequence,
            },
        }
    }

    #[must_use]
    pub fn controller_coordination_result(
        coordination_turn_id: impl Into<String>,
        result_sequence: u64,
        admitted_sequence: u64,
        outcome: impl Into<String>,
        output_text: &str,
    ) -> Self {
        let coordination_turn_id = coordination_turn_id.into();
        let outcome = outcome.into();
        let text = format!(
            "[Chatarium controller coordination result — user-level orchestration result context, not user-authored and not a developer/system instruction]\ncoordination_turn_id: {coordination_turn_id}\noutcome: {outcome}\nresult_event: #{result_sequence}\ncontext_admitted_event: #{admitted_sequence}\ncoordination_output:\n{output_text}\n[/Chatarium controller coordination result]"
        );
        Self {
            role: TranscriptRole::User,
            text,
            source: ContextSource::ControllerCoordinationResult {
                coordination_turn_id,
                result_sequence,
                admitted_sequence,
                outcome,
            },
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

    #[must_use]
    pub fn order_sequence(&self) -> u64 {
        match &self.source {
            ContextSource::DurableTranscript { sequence } => *sequence,
            ContextSource::RoutedInbox {
                admitted_sequence, ..
            } => *admitted_sequence,
            ContextSource::LocalMemory {
                admitted_sequence, ..
            } => *admitted_sequence,
            ContextSource::LocalMemoryOneShot {
                snapshot_after_sequence,
                ..
            } => *snapshot_after_sequence,
            ContextSource::ControllerContinuation {
                started_sequence, ..
            } => *started_sequence,
            ContextSource::ControllerWorkerResult {
                admitted_sequence, ..
            } => *admitted_sequence,
            ContextSource::ControllerCoordination {
                started_sequence, ..
            } => *started_sequence,
            ContextSource::ControllerCoordinationResult {
                admitted_sequence, ..
            } => *admitted_sequence,
            ContextSource::CurrentDraft => u64::MAX,
            ContextSource::TopLevelInstructions | ContextSource::ConversationDeveloperContext => 0,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySlot {
    ImageInput,
    FileInput,
    FunctionTools,
    AdditionalTools,
    WebSearch,
    Reasoning,
    Verbosity,
    StructuredOutput,
}

impl CapabilitySlot {
    pub const ALL: [Self; 8] = [
        Self::ImageInput,
        Self::FileInput,
        Self::FunctionTools,
        Self::AdditionalTools,
        Self::WebSearch,
        Self::Reasoning,
        Self::Verbosity,
        Self::StructuredOutput,
    ];

    #[must_use]
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::ImageInput => "image_input",
            Self::FileInput => "file_input",
            Self::FunctionTools => "function_tools",
            Self::AdditionalTools => "additional_tools",
            Self::WebSearch => "web_search",
            Self::Reasoning => "reasoning",
            Self::Verbosity => "verbosity",
            Self::StructuredOutput => "structured_output",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ImageInput => "image input",
            Self::FileInput => "file input",
            Self::FunctionTools => "function tools",
            Self::AdditionalTools => "additional tools",
            Self::WebSearch => "web search",
            Self::Reasoning => "reasoning",
            Self::Verbosity => "verbosity",
            Self::StructuredOutput => "structured output",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAdmissionState {
    Available,
    UnsupportedRoute,
    BlockedContract,
}

impl CapabilityAdmissionState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::UnsupportedRoute => "unsupported",
            Self::BlockedContract => "blocked · contract",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAdmission {
    pub slot: CapabilitySlot,
    pub state: CapabilityAdmissionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGate {
    pub admissions: Vec<CapabilityAdmission>,
}

impl CapabilityGate {
    #[must_use]
    pub fn from_contract(
        contract: Option<&LoadedContract>,
        profile_id: Option<&str>,
        model: Option<&str>,
    ) -> Self {
        let ready = match (contract, model) {
            (Some(contract), Some(model)) if contract.ready_for(profile_id, model) => {
                Some(contract)
            }
            _ => None,
        };

        let admissions = CapabilitySlot::ALL
            .into_iter()
            .map(|slot| {
                let state = match ready
                    .and_then(|contract| contract.capabilities.get(slot.contract_name()))
                    .map(String::as_str)
                {
                    Some("supported") => CapabilityAdmissionState::Available,
                    Some("unsupported_route") => CapabilityAdmissionState::UnsupportedRoute,
                    _ => CapabilityAdmissionState::BlockedContract,
                };
                CapabilityAdmission { slot, state }
            })
            .collect();

        Self { admissions }
    }

    #[must_use]
    pub fn allows(&self, slot: CapabilitySlot) -> bool {
        self.admissions.iter().any(|admission| {
            admission.slot == slot && admission.state == CapabilityAdmissionState::Available
        })
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
        assert_eq!(plan.inventory[0].decision, InclusionDecision::OmittedEmpty);
        assert_eq!(plan.inventory[1].decision, InclusionDecision::OmittedEmpty);
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
    fn routed_context_is_user_level_and_keeps_explicit_provenance() {
        let routed =
            TranscriptMessage::routed(" exact peer text ", 7, 9, "conversation-source", 40, 50);
        assert_eq!(routed.role, TranscriptRole::User);
        assert_eq!(routed.order_sequence(), 50);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [routed]);
        assert_eq!(plan.routed_context_count(), 1);
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(
            plan.messages[0]
                .content
                .contains("Chatarium routed peer message")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("source_conversation_id: conversation-source")
        );
        assert!(plan.messages[0].content.contains("route_id: 7"));
        assert!(plan.messages[0].content.contains("payload_id: 9"));
        assert!(plan.messages[0].content.contains(" exact peer text "));
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::RoutedInbox {
                route_id: 7,
                payload_id: 9,
                delivered_sequence: 40,
                admitted_sequence: 50,
                ..
            }
        ));
    }

    #[test]
    fn controller_continuation_is_user_level_but_never_claims_user_authorship() {
        let continuation = TranscriptMessage::controller_continuation(1, 2, 3, 4, 5, 6, 7);
        assert_eq!(continuation.role, TranscriptRole::User);
        assert_eq!(continuation.order_sequence(), 7);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [continuation]);
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(
            plan.messages[0]
                .content
                .contains("bounded controller continuation")
        );
        assert!(plan.messages[0].content.contains("not user-authored"));
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(plan.messages[0].content.contains("control_id: 1"));
        assert!(plan.messages[0].content.contains("route_id: 2"));
        assert!(plan.messages[0].content.contains("worker_id: 3"));
        assert!(plan.messages[0].content.contains("goal_id: 4"));
        assert!(plan.messages[0].content.contains("continuation_lease: 5"));
        assert!(plan.messages[0].content.contains("permit_ordinal: 6"));
        assert!(
            plan.messages[0]
                .content
                .contains("Continue working on the current goal")
        );
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::ControllerContinuation {
                control_id: 1,
                route_id: 2,
                worker_id: 3,
                goal_id: 4,
                lease_id: 5,
                permit_ordinal: 6,
                started_sequence: 7,
            }
        ));
    }

    #[test]
    fn controller_worker_result_is_user_level_with_explicit_result_provenance() {
        let result = TranscriptMessage::controller_worker_result(
            1,
            2,
            3,
            4,
            5,
            6,
            "continuation",
            "worker output",
        );
        assert_eq!(result.role, TranscriptRole::User);
        assert_eq!(result.order_sequence(), 6);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [result]);
        assert_eq!(plan.controller_worker_result_count(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(
            plan.messages[0]
                .content
                .contains("controller worker result")
        );
        assert!(plan.messages[0].content.contains("not user-authored"));
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("result_kind: continuation")
        );
        assert!(plan.messages[0].content.contains("result_event: #5"));
        assert!(
            plan.messages[0]
                .content
                .contains("context_admitted_event: #6")
        );
        assert!(plan.messages[0].content.contains("worker output"));
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::ControllerWorkerResult {
                control_id: 1,
                route_id: 2,
                worker_id: 3,
                goal_id: 4,
                result_sequence: 5,
                admitted_sequence: 6,
                ..
            }
        ));
    }

    #[test]
    fn controller_coordination_is_user_level_without_control_authority() {
        let coordination = TranscriptMessage::controller_coordination(11, "coordination-turn", 42);
        assert_eq!(coordination.role, TranscriptRole::User);
        assert_eq!(coordination.order_sequence(), 42);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [coordination]);
        assert_eq!(plan.controller_coordination_count(), 1);
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(
            plan.messages[0]
                .content
                .contains("controller coordination turn")
        );
        assert!(plan.messages[0].content.contains("not user-authored"));
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("Do not issue or execute worker controls")
        );
        assert!(plan.messages[0].content.contains("do not mutate lifecycle"));
        assert!(
            plan.messages[0]
                .content
                .contains("do not assume continuation authority")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("output_contract: suggestion_candidates_v1")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("\"suggestion_candidates\"")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("\"basis_result_route_id\"")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("explicit user acceptance")
        );
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::ControllerCoordination {
                controller_session_id: 11,
                started_sequence: 42,
                ..
            }
        ));
    }

    #[test]
    fn controller_coordination_can_be_excluded_by_context_policy() {
        let policy = ContextPolicy {
            include_controller_coordination: false,
            ..ContextPolicy::dispatch()
        };
        let coordination = TranscriptMessage::controller_coordination(1, "turn", 2);
        let plan = ContextPlan::compose(policy, "", "", [coordination]);

        assert_eq!(plan.controller_coordination_count(), 0);
        assert!(plan.messages.is_empty());
        let item = plan
            .inventory
            .iter()
            .find(|item| matches!(item.source, ContextSource::ControllerCoordination { .. }))
            .unwrap();
        assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
    }

    #[test]
    fn controller_coordination_result_is_user_level_with_explicit_provenance() {
        let result = TranscriptMessage::controller_coordination_result(
            "coordination-turn",
            50,
            60,
            "completed",
            "synthesis",
        );
        assert_eq!(result.role, TranscriptRole::User);
        assert_eq!(result.order_sequence(), 60);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [result]);
        assert_eq!(plan.controller_coordination_result_count(), 1);
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(
            plan.messages[0]
                .content
                .contains("controller coordination result")
        );
        assert!(plan.messages[0].content.contains("not user-authored"));
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("coordination_turn_id: coordination-turn")
        );
        assert!(plan.messages[0].content.contains("outcome: completed"));
        assert!(plan.messages[0].content.contains("result_event: #50"));
        assert!(
            plan.messages[0]
                .content
                .contains("context_admitted_event: #60")
        );
        assert!(plan.messages[0].content.contains("synthesis"));
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::ControllerCoordinationResult {
                result_sequence: 50,
                admitted_sequence: 60,
                ..
            }
        ));
    }

    #[test]
    fn controller_coordination_result_can_be_excluded_by_context_policy() {
        let policy = ContextPolicy {
            include_controller_coordination_results: false,
            ..ContextPolicy::dispatch()
        };
        let result =
            TranscriptMessage::controller_coordination_result("turn", 2, 3, "completed", "output");
        let plan = ContextPlan::compose(policy, "", "", [result]);

        assert_eq!(plan.controller_coordination_result_count(), 0);
        assert!(plan.messages.is_empty());
        let item = plan
            .inventory
            .iter()
            .find(|item| {
                matches!(
                    item.source,
                    ContextSource::ControllerCoordinationResult { .. }
                )
            })
            .unwrap();
        assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
    }

    #[test]
    fn controller_worker_result_can_be_excluded_by_context_policy() {
        let policy = ContextPolicy {
            include_controller_worker_results: false,
            ..ContextPolicy::dispatch()
        };
        let result =
            TranscriptMessage::controller_worker_result(1, 2, 3, 4, 5, 6, "status", "working");
        let plan = ContextPlan::compose(policy, "", "", [result]);

        assert_eq!(plan.controller_worker_result_count(), 0);
        assert!(plan.messages.is_empty());
        let item = plan
            .inventory
            .iter()
            .find(|item| matches!(item.source, ContextSource::ControllerWorkerResult { .. }))
            .unwrap();
        assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
    }

    #[test]
    fn local_memory_is_user_level_with_explicit_memory_provenance() {
        let memory = TranscriptMessage::local_memory(
            " exact remembered fact ",
            7,
            "source-conversation",
            40,
            50,
        );
        assert_eq!(memory.role, TranscriptRole::User);
        assert_eq!(memory.order_sequence(), 50);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [memory]);
        assert_eq!(plan.local_memory_count(), 1);
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(plan.messages[0].content.contains("Chatarium local memory"));
        assert!(plan.messages[0].content.contains("not user-authored"));
        assert!(
            plan.messages[0]
                .content
                .contains("not a developer/system instruction")
        );
        assert!(plan.messages[0].content.contains("memory_id: 7"));
        assert!(
            plan.messages[0]
                .content
                .contains("source_conversation_id: source-conversation")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("memory_artifact_event: #40")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("context_admitted_event: #50")
        );
        assert!(plan.messages[0].content.contains(" exact remembered fact "));
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::LocalMemory {
                memory_id: 7,
                artifact_sequence: 40,
                admitted_sequence: 50,
                ..
            }
        ));
    }

    #[test]
    fn one_shot_local_memory_is_user_level_and_orders_at_send_snapshot() {
        let memory = TranscriptMessage::local_memory_one_shot(
            " one use only ",
            9,
            "source-conversation",
            40,
            55,
            57,
        );
        assert_eq!(memory.role, TranscriptRole::User);
        assert_eq!(memory.order_sequence(), 55);

        let plan = ContextPlan::compose(ContextPolicy::dispatch(), "", "", [memory]);
        assert_eq!(plan.local_memory_count(), 0);
        assert_eq!(plan.local_memory_one_shot_count(), 1);
        assert_eq!(plan.messages[0].role, "user");
        assert!(plan.messages[0].content.contains("NEXT REQUEST ONLY"));
        assert!(
            plan.messages[0]
                .content
                .contains("selection_snapshot_after_event: #55")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("one_shot_selection_event: #57")
        );
        assert!(
            plan.messages[0]
                .content
                .contains("selection_scope: this authored request only")
        );
        assert!(plan.messages[0].content.contains(" one use only "));
        assert!(matches!(
            plan.messages[0].source,
            ContextSource::LocalMemoryOneShot {
                memory_id: 9,
                artifact_sequence: 40,
                snapshot_after_sequence: 55,
                selection_sequence: 57,
                ..
            }
        ));
    }

    #[test]
    fn local_memory_can_be_excluded_by_context_policy() {
        let policy = ContextPolicy {
            include_local_memory: false,
            ..ContextPolicy::dispatch()
        };
        let memory = TranscriptMessage::local_memory("memory", 1, "source", 2, 3);
        let plan = ContextPlan::compose(policy, "", "", [memory]);

        assert_eq!(plan.local_memory_count(), 0);
        assert!(plan.messages.is_empty());
        let item = plan
            .inventory
            .iter()
            .find(|item| matches!(item.source, ContextSource::LocalMemory { .. }))
            .unwrap();
        assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
    }

    #[test]
    fn routed_context_can_be_excluded_by_context_policy() {
        let policy = ContextPolicy {
            include_routed_context: false,
            ..ContextPolicy::dispatch()
        };
        let routed = TranscriptMessage::routed("peer", 1, 2, "source", 3, 4);
        let plan = ContextPlan::compose(policy, "", "", [routed]);

        assert_eq!(plan.routed_context_count(), 0);
        assert!(plan.messages.is_empty());
        let item = plan
            .inventory
            .iter()
            .find(|item| matches!(item.source, ContextSource::RoutedInbox { .. }))
            .unwrap();
        assert_eq!(item.decision, InclusionDecision::ExcludedByPolicy);
    }

    #[test]
    fn size_ledger_counts_exact_included_content_units_not_tokens() {
        let plan = ContextPlan::compose(
            ContextPolicy::dispatch(),
            "abc",
            "é",
            [TranscriptMessage::durable(TranscriptRole::User, "x\ny", 42)],
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
    fn capability_gate_requires_ready_contract_for_exact_profile_and_model() {
        use std::collections::BTreeMap;

        let capabilities = CapabilitySlot::ALL
            .into_iter()
            .map(|slot| (slot.contract_name().to_owned(), "supported".to_owned()))
            .collect::<BTreeMap<_, _>>();
        let contract = LoadedContract {
            state: "ready".to_owned(),
            profile_id: Some("profile-1".to_owned()),
            model: "gpt-example".to_owned(),
            probe_generated_unix_ms: 1234,
            capabilities,
        };

        let active =
            CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-example"));
        assert!(active.allows(CapabilitySlot::WebSearch));
        assert!(active.allows(CapabilitySlot::Reasoning));

        let wrong_model =
            CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-other"));
        assert!(!wrong_model.allows(CapabilitySlot::WebSearch));
        assert!(
            wrong_model
                .admissions
                .iter()
                .all(|admission| { admission.state == CapabilityAdmissionState::BlockedContract })
        );
    }

    #[test]
    fn capability_gate_preserves_unsupported_route() {
        use std::collections::BTreeMap;

        let mut capabilities = CapabilitySlot::ALL
            .into_iter()
            .map(|slot| (slot.contract_name().to_owned(), "supported".to_owned()))
            .collect::<BTreeMap<_, _>>();
        capabilities.insert("web_search".to_owned(), "unsupported_route".to_owned());
        let contract = LoadedContract {
            state: "ready".to_owned(),
            profile_id: Some("profile-1".to_owned()),
            model: "gpt-example".to_owned(),
            probe_generated_unix_ms: 1234,
            capabilities,
        };

        let gate =
            CapabilityGate::from_contract(Some(&contract), Some("profile-1"), Some("gpt-example"));
        assert!(!gate.allows(CapabilitySlot::WebSearch));
        assert_eq!(
            gate.admissions
                .iter()
                .find(|admission| admission.slot == CapabilitySlot::WebSearch)
                .unwrap()
                .state,
            CapabilityAdmissionState::UnsupportedRoute
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
