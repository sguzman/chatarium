//! Context source provenance and inclusion policy.
use super::*;

/// Conservative UTF-8 proxy ceiling, not a model-token limit. The raw journal
/// is never pruned; only the input assembled for this inference is bounded.
pub const DEFAULT_CONTEXT_MAX_UTF8_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

impl TranscriptRole {
    pub(super) const fn request_role(self) -> &'static str {
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
    ToolResult {
        call_id: u64,
        route_id: u64,
        provider_id: u64,
        source_session_id: u64,
        outcome_sequence: u64,
        admitted_sequence: u64,
        outcome_kind: String,
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
        selection_sequence: Option<u64>,
    },
    OpenVikingCheckpoint {
        source_conversation_id: String,
        source_turn_id: String,
        source_journal_sequence: u64,
        uri: String,
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
    CoordinationActivation {
        started_sequence: u64,
        activation_turn_id: String,
        inbox_ids: Vec<u64>,
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
            Self::ToolResult {
                call_id,
                route_id,
                provider_id,
                source_session_id,
                outcome_sequence,
                admitted_sequence,
                outcome_kind,
            } => format!(
                "tool result · {outcome_kind} · call {call_id} · route {route_id} · provider {provider_id} · session {source_session_id} · outcome #{outcome_sequence} · admitted #{admitted_sequence}"
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
            } => {
                let selection = selection_sequence
                    .map(|sequence| format!("#{sequence}"))
                    .unwrap_or_else(|| "pending durable commit".to_owned());
                format!(
                    "local memory · ONE SHOT · memory {memory_id} · source {source_conversation_id} · artifact #{artifact_sequence} · snapshot after #{snapshot_after_sequence} · selection {selection}"
                )
            }
            Self::OpenVikingCheckpoint {
                source_conversation_id,
                source_turn_id,
                source_journal_sequence,
                uri,
            } => format!(
                "OpenViking checkpoint · source {source_conversation_id} · turn {source_turn_id} · journal #{source_journal_sequence} · URI {uri}"
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
            Self::CoordinationActivation {
                started_sequence,
                activation_turn_id,
                inbox_ids,
            } => format!(
                "verified coordination activation · turn {activation_turn_id} · inbox {inbox_ids:?} · started #{started_sequence}"
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
    ExcludedByBudget,
}

impl InclusionDecision {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::OmittedEmpty => "omitted · empty",
            Self::ExcludedByPolicy => "excluded · policy",
            Self::ExcludedByBudget => "excluded · context budget",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPolicy {
    pub include_instructions: bool,
    pub include_developer_context: bool,
    pub include_durable_transcript: bool,
    pub include_routed_context: bool,
    pub include_tool_results: bool,
    pub include_local_memory: bool,
    pub include_openviking_checkpoints: bool,
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
            include_tool_results: true,
            include_local_memory: true,
            include_openviking_checkpoints: true,
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

    pub(super) const fn includes_source(self, source: &ContextSource) -> bool {
        match source {
            ContextSource::TopLevelInstructions => self.include_instructions,
            ContextSource::ConversationDeveloperContext => self.include_developer_context,
            ContextSource::DurableTranscript { .. } => self.include_durable_transcript,
            ContextSource::RoutedInbox { .. } => self.include_routed_context,
            ContextSource::ToolResult { .. } => self.include_tool_results,
            ContextSource::LocalMemory { .. } | ContextSource::LocalMemoryOneShot { .. } => {
                self.include_local_memory
            }
            ContextSource::OpenVikingCheckpoint { .. } => self.include_openviking_checkpoints,
            ContextSource::ControllerContinuation { .. } => self.include_controller_continuation,
            ContextSource::ControllerWorkerResult { .. } => self.include_controller_worker_results,
            ContextSource::ControllerCoordination { .. } => self.include_controller_coordination,
            ContextSource::ControllerCoordinationResult { .. } => {
                self.include_controller_coordination_results
            }
            ContextSource::CoordinationActivation { .. } => self.include_durable_transcript,
            ContextSource::CurrentDraft => self.include_current_draft,
        }
    }
}

