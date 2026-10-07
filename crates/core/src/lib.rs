//! Domain model for Chatarium reliability state.

pub mod authenticated_session;
pub mod chat_container;
pub mod control;
pub mod control_provenance;
pub mod control_route;
pub mod coordination_suggestion;
pub mod local_memory;
pub mod orchestration;
pub mod remote;
pub mod routing;
pub mod session;
pub mod supervision;

use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! local_id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// Generate a new local identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Return the opaque UUID backing this local identifier.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

local_id_type!(
    LocalConversationId,
    "Opaque local conversation identity independent of any remote conversation identifier."
);
local_id_type!(
    LocalTurnId,
    "Opaque local turn identity independent of any remote turn or request identifier."
);
local_id_type!(
    LocalMessageId,
    "Opaque local message identity independent of any remote message identifier."
);
local_id_type!(
    RemoteReadObservationId,
    "Opaque local identity for one recorded remote read observation."
);

/// One locally authored user message with identities independent of the remote service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoredUserMessage {
    /// Local conversation containing the message.
    pub conversation_id: LocalConversationId,
    /// Local turn containing the message.
    pub turn_id: LocalTurnId,
    /// Local message identity.
    pub message_id: LocalMessageId,
    /// Exact user-authored text committed before any remote mutation.
    pub text: String,
}

impl AuthoredUserMessage {
    /// Construct one typed authored user message without contacting any remote service.
    #[must_use]
    pub fn new(
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
        message_id: LocalMessageId,
        text: impl Into<String>,
    ) -> Self {
        Self {
            conversation_id,
            turn_id,
            message_id,
            text: text.into(),
        }
    }
}

/// Error encountered while replaying durable event kinds into turn evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnReplayError {
    /// A durable dispatch event appeared before a durable local message commit.
    DispatchBeforeCommit,
}

impl fmt::Display for TurnReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DispatchBeforeCommit => {
                write!(
                    formatter,
                    "dispatch evidence appeared before local message commit"
                )
            }
        }
    }
}

impl std::error::Error for TurnReplayError {}

/// Durable evidence Chatarium has about the locally authored side of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalEvidence {
    /// No immutable outgoing message has been committed yet.
    #[default]
    DraftOnly,
    /// The exact outgoing user message is durably committed locally.
    MessageCommitted,
}

/// Evidence Chatarium has about the remote handling of a local turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteEvidence {
    /// No remote dispatch has been observed or initiated.
    #[default]
    NotAttempted,
    /// Dispatch has begun but no acceptance/failure evidence has been observed yet.
    Dispatching,
    /// Transport ended without enough evidence to decide whether the remote accepted the turn.
    OutcomeUnknown,
    /// Remote acceptance or identity was positively observed.
    AcceptedObserved,
    /// A remote failure was positively observed.
    FailedObserved,
}

/// Evidence Chatarium has about assistant output for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssistantEvidence {
    /// No assistant output has been observed.
    #[default]
    None,
    /// Output is currently being observed incrementally.
    Streaming,
    /// Some output was observed before an interruption without observed completion.
    PartialInterrupted,
    /// Completion was positively observed.
    CompletedObserved,
}

/// Combined evidence state for a local turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnEvidence {
    /// Durable local-message evidence.
    pub local: LocalEvidence,
    /// Remote mutation evidence.
    pub remote: RemoteEvidence,
    /// Assistant-output evidence.
    pub assistant: AssistantEvidence,
}

impl TurnEvidence {
    /// Record that the exact outgoing user message is durably committed locally.
    pub fn commit_local_message(&mut self) {
        self.local = LocalEvidence::MessageCommitted;
    }

    /// Mark the start of a remote dispatch attempt.
    ///
    /// A dispatch may only begin after the outgoing message is durably committed. Returning
    /// `false` leaves the state unchanged and lets callers surface an invariant violation rather
    /// than allowing transient UI/network state to outrun local durability.
    pub fn begin_dispatch(&mut self) -> bool {
        if self.local != LocalEvidence::MessageCommitted {
            return false;
        }
        self.remote = RemoteEvidence::Dispatching;
        true
    }

    /// Mark a transport interruption before acceptance/failure was established.
    pub fn mark_transport_ambiguous(&mut self) {
        if self.remote == RemoteEvidence::Dispatching {
            self.remote = RemoteEvidence::OutcomeUnknown;
        }
        if self.assistant == AssistantEvidence::Streaming {
            self.assistant = AssistantEvidence::PartialInterrupted;
        }
    }

    /// Record positive evidence that the remote accepted the turn.
    pub fn observe_acceptance(&mut self) {
        self.remote = RemoteEvidence::AcceptedObserved;
    }

    /// Record positive evidence that the remote rejected/failed the turn.
    pub fn observe_remote_failure(&mut self) {
        self.remote = RemoteEvidence::FailedObserved;
    }

    /// Record the first observed assistant output.
    pub fn observe_assistant_output(&mut self) {
        if self.assistant != AssistantEvidence::CompletedObserved {
            self.assistant = AssistantEvidence::Streaming;
        }
    }

    /// Record positive evidence that assistant generation completed.
    pub fn observe_completion(&mut self) {
        self.assistant = AssistantEvidence::CompletedObserved;
    }

    /// Apply one durable semantic event kind to this evidence projection.
    ///
    /// Payload-dependent reconciliation events are intentionally ignored here; this method only
    /// derives facts that are positively encoded by the event kind itself.
    pub fn apply_event_kind(&mut self, kind: EventKind) -> Result<(), TurnReplayError> {
        match kind {
            EventKind::UserMessageCommitted => self.commit_local_message(),
            EventKind::DispatchAttempted => {
                if !self.begin_dispatch() {
                    return Err(TurnReplayError::DispatchBeforeCommit);
                }
            }
            EventKind::RemoteAcceptanceObserved => self.observe_acceptance(),
            EventKind::RemoteFailureObserved => self.observe_remote_failure(),
            EventKind::AssistantStreamStarted
            | EventKind::AssistantDeltaObserved
            | EventKind::AssistantSnapshotObserved => self.observe_assistant_output(),
            EventKind::AssistantCompletionObserved => self.observe_completion(),
            EventKind::TransportInterrupted => self.mark_transport_ambiguous(),
            EventKind::DraftChanged
            | EventKind::AssistantStatusObserved
            | EventKind::TranscriptUserMessageObserved
            | EventKind::ClientErrorObserved
            | EventKind::ReconciliationAttempted
            | EventKind::ReconciliationObserved
            | EventKind::ImportStarted
            | EventKind::ImportCompleted
            | EventKind::HistoricalConversationSnapshotImported
            | EventKind::RouteProposed
            | EventKind::RoutePayloadAttached
            | EventKind::RouteUserDecisionRecorded
            | EventKind::RouteDispatched
            | EventKind::LocalRouteDelivered
            | EventKind::LocalRouteContextDecisionRecorded
            | EventKind::LocalMemoryArtifactRecorded
            | EventKind::LocalMemoryContextDecisionRecorded
            | EventKind::RouteResultObserved
            | EventKind::LocalConversationWorkerBound
            | EventKind::LocalConversationChatContainerBound
            | EventKind::WorkerGoalAssigned
            | EventKind::WorkerLifecycleTransitionRecorded
            | EventKind::WorkerControlAdmitted
            | EventKind::ControlRouteBound
            | EventKind::WorkerControlDelivered
            | EventKind::WorkerControlAcknowledged
            | EventKind::WorkerControlStatusResultRecorded
            | EventKind::WorkerControlActionStarted
            | EventKind::WorkerControlActionResultRecorded
            | EventKind::LocalSessionRegistered
            | EventKind::SessionEndpointBound
            | EventKind::WorkerSessionBound
            | EventKind::WorkerSessionSuccessorBound
            | EventKind::ControllerSessionDesignated
            | EventKind::ControllerWorkerBound
            | EventKind::WorkerControlIssuerBound
            | EventKind::WorkerContinuationExecutionStarted
            | EventKind::WorkerContinuationExecutionResultRecorded
            | EventKind::ControllerWorkerResultContextDecisionRecorded
            | EventKind::ControllerCoordinationTurnStarted
            | EventKind::ControllerCoordinationTurnResultRecorded
            | EventKind::ControllerCoordinationResultContextDecisionRecorded
            | EventKind::ControllerCoordinationSuggestionRecorded
            | EventKind::ControllerCoordinationSuggestionPromoted
            | EventKind::ContinuationLeaseCreated
            | EventKind::ContinuationPermitIssued
            | EventKind::RemoteConversationBound
            | EventKind::RemoteReadObservationRecorded
            | EventKind::RemoteMirrorSelectionChanged
            | EventKind::RemoteConversationSnapshotImported
            | EventKind::RemoteMirrorQueueItemQueued
            | EventKind::RemoteMirrorQueueCaptureStarted
            | EventKind::RemoteMirrorQueueCompleted
            | EventKind::RemoteMirrorQueueRateLimited
            | EventKind::RemoteMirrorQueueFailed
            | EventKind::RemoteHealthObserved
            | EventKind::ChatContainerCreated
            | EventKind::ChatSessionLifecycleTransitionRecorded
            | EventKind::ChatSessionSuccessorBound => {}
        }
        Ok(())
    }

    /// Replay durable semantic event kinds in sequence to reconstruct current turn evidence.
    pub fn replay_event_kinds(
        kinds: impl IntoIterator<Item = EventKind>,
    ) -> Result<Self, TurnReplayError> {
        let mut evidence = Self::default();
        for kind in kinds {
            evidence.apply_event_kind(kind)?;
        }
        Ok(evidence)
    }
}

/// Durable local event kinds. Payload storage is intentionally left to the store layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// The draft changed.
    DraftChanged,
    /// An immutable outgoing user message was committed locally.
    UserMessageCommitted,
    /// A remote dispatch was attempted.
    DispatchAttempted,
    /// Remote acceptance was observed.
    RemoteAcceptanceObserved,
    /// A remote failure was observed.
    RemoteFailureObserved,
    /// Assistant output began.
    AssistantStreamStarted,
    /// Assistant output changed.
    AssistantDeltaObserved,
    /// A complete assistant DOM/text snapshot was observed without implying generation completed.
    AssistantSnapshotObserved,
    /// A transient assistant-side UI/status placeholder was observed.
    AssistantStatusObserved,
    /// A user message was observed in a remote/rendered transcript.
    TranscriptUserMessageObserved,
    /// A visible client-side/site error was observed.
    ClientErrorObserved,
    /// Assistant completion was observed.
    AssistantCompletionObserved,
    /// Transport was interrupted.
    TransportInterrupted,
    /// Reconciliation was attempted.
    ReconciliationAttempted,
    /// Reconciliation produced a new observation.
    ReconciliationObserved,
    /// Import of an external/local capture began.
    ImportStarted,
    /// Import of an external/local capture completed.
    ImportCompleted,
    /// One historical conversation snapshot from an account export was imported locally.
    HistoricalConversationSnapshotImported,
    /// A supervisory route was durably proposed.
    RouteProposed,
    /// One immutable local payload was correlated to a proposed route.
    RoutePayloadAttached,
    /// An explicit user allow/deny decision for a supervisory route was recorded.
    RouteUserDecisionRecorded,
    /// A supervisory route consumed its one-shot dispatch authorization.
    RouteDispatched,
    /// A dispatched local session-message payload was durably delivered.
    LocalRouteDelivered,
    /// An explicit include/exclude decision for delivered routed context was recorded.
    LocalRouteContextDecisionRecorded,
    /// One immutable explicit local memory artifact was recorded.
    LocalMemoryArtifactRecorded,
    /// An explicit include/exclude decision for one local memory artifact was recorded.
    LocalMemoryContextDecisionRecorded,
    /// A generic routing-layer result or error observation was recorded.
    RouteResultObserved,
    /// One local conversation was durably correlated to an orchestration worker identity.
    LocalConversationWorkerBound,
    /// One local conversation was durably correlated to a logical chat container.
    LocalConversationChatContainerBound,
    /// A worker received a new typed goal assignment.
    WorkerGoalAssigned,
    /// A typed worker lifecycle transition was recorded.
    WorkerLifecycleTransitionRecorded,
    /// A typed orchestration control command was admitted locally.
    WorkerControlAdmitted,
    /// An admitted worker control was correlated to its orchestration route.
    ControlRouteBound,
    /// A dispatched worker control was durably delivered to its local worker conversation.
    WorkerControlDelivered,
    /// A delivered worker control was explicitly acknowledged by its local worker conversation.
    WorkerControlAcknowledged,
    /// An acknowledged worker StatusRequest captured a durable worker lifecycle snapshot.
    WorkerControlStatusResultRecorded,
    /// A worker explicitly began applying an acknowledged mutating control.
    WorkerControlActionStarted,
    /// A control-correlated worker lifecycle mutation produced a durable action result.
    WorkerControlActionResultRecorded,
    /// A local Chatarium session identity was registered.
    LocalSessionRegistered,
    /// A local session was correlated to one routing endpoint.
    SessionEndpointBound,
    /// An orchestration worker was correlated to one local session.
    WorkerSessionBound,
    /// A persistent worker identity moved from one session leaf to its explicit successor.
    WorkerSessionSuccessorBound,
    /// A local session was explicitly designated as a controller/coordinator.
    ControllerSessionDesignated,
    /// A controller session was correlated to one worker session.
    ControllerWorkerBound,
    /// Explicit issuer provenance was correlated to one admitted worker control.
    WorkerControlIssuerBound,
    /// Worker-side execution of one acknowledged Continue control began.
    WorkerContinuationExecutionStarted,
    /// Terminal remote outcome for one worker continuation execution was recorded.
    WorkerContinuationExecutionResultRecorded,
    /// An explicit include/exclude decision for one controller-visible worker result was recorded.
    ControllerWorkerResultContextDecisionRecorded,
    /// A deliberate non-authored controller coordination turn was started.
    ControllerCoordinationTurnStarted,
    /// A non-authored controller coordination turn reached a durable terminal outcome.
    ControllerCoordinationTurnResultRecorded,
    /// An explicit include/exclude decision for one terminal controller coordination result was recorded.
    ControllerCoordinationResultContextDecisionRecorded,
    /// A non-authoritative typed next-action suggestion was recorded from controller coordination.
    ControllerCoordinationSuggestionRecorded,
    /// An explicit user promotion correlated a coordination suggestion to a real control proposal.
    ControllerCoordinationSuggestionPromoted,
    /// A bounded continuation lease was durably created.
    ContinuationLeaseCreated,
    /// One continuation permit ordinal was durably issued from a lease.
    ContinuationPermitIssued,
    /// One local conversation was correlated to an observed remote conversation identity.
    RemoteConversationBound,
    /// One safe structural remote read observation was durably recorded.
    RemoteReadObservationRecorded,
    /// User intent to include or exclude one bound conversation from future remote mirroring changed.
    RemoteMirrorSelectionChanged,
    /// One validated remote conversation snapshot was imported into durable local mirror state.
    RemoteConversationSnapshotImported,
    /// One discovered remote conversation entered the durable mirror queue.
    RemoteMirrorQueueItemQueued,
    /// One durable mirror queue item began its exact capture attempt.
    RemoteMirrorQueueCaptureStarted,
    /// One durable mirror queue item completed as full or partial.
    RemoteMirrorQueueCompleted,
    /// One durable mirror queue item was stopped by an HTTP 429 response.
    RemoteMirrorQueueRateLimited,
    /// One durable mirror queue item failed without a successful mirror.
    RemoteMirrorQueueFailed,
    /// One structural remote-health observation changed controller gating state.
    RemoteHealthObserved,
    /// One logical chat container was created around an already-registered root session.
    ChatContainerCreated,
    /// One chat-container session advanced toward saturation.
    ChatSessionLifecycleTransitionRecorded,
    /// One saturated chat session was durably succeeded by a fresh session with handoff provenance.
    ChatSessionSuccessorBound,
}

impl EventKind {
    /// Stable lowercase name used by durable journal formats.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::DraftChanged => "draft_changed",
            Self::UserMessageCommitted => "user_message_committed",
            Self::DispatchAttempted => "dispatch_attempted",
            Self::RemoteAcceptanceObserved => "remote_acceptance_observed",
            Self::RemoteFailureObserved => "remote_failure_observed",
            Self::AssistantStreamStarted => "assistant_stream_started",
            Self::AssistantDeltaObserved => "assistant_delta_observed",
            Self::AssistantSnapshotObserved => "assistant_snapshot_observed",
            Self::AssistantStatusObserved => "assistant_status_observed",
            Self::TranscriptUserMessageObserved => "transcript_user_message_observed",
            Self::ClientErrorObserved => "client_error_observed",
            Self::AssistantCompletionObserved => "assistant_completion_observed",
            Self::TransportInterrupted => "transport_interrupted",
            Self::ReconciliationAttempted => "reconciliation_attempted",
            Self::ReconciliationObserved => "reconciliation_observed",
            Self::ImportStarted => "import_started",
            Self::ImportCompleted => "import_completed",
            Self::HistoricalConversationSnapshotImported => {
                "historical_conversation_snapshot_imported"
            }
            Self::RouteProposed => "route_proposed",
            Self::RoutePayloadAttached => "route_payload_attached",
            Self::RouteUserDecisionRecorded => "route_user_decision_recorded",
            Self::RouteDispatched => "route_dispatched",
            Self::LocalRouteDelivered => "local_route_delivered",
            Self::LocalRouteContextDecisionRecorded => "local_route_context_decision_recorded",
            Self::LocalMemoryArtifactRecorded => "local_memory_artifact_recorded",
            Self::LocalMemoryContextDecisionRecorded => "local_memory_context_decision_recorded",
            Self::RouteResultObserved => "route_result_observed",
            Self::LocalConversationWorkerBound => "local_conversation_worker_bound",
            Self::LocalConversationChatContainerBound => "local_conversation_chat_container_bound",
            Self::WorkerGoalAssigned => "worker_goal_assigned",
            Self::WorkerLifecycleTransitionRecorded => "worker_lifecycle_transition_recorded",
            Self::WorkerControlAdmitted => "worker_control_admitted",
            Self::ControlRouteBound => "control_route_bound",
            Self::WorkerControlDelivered => "worker_control_delivered",
            Self::WorkerControlAcknowledged => "worker_control_acknowledged",
            Self::WorkerControlStatusResultRecorded => "worker_control_status_result_recorded",
            Self::WorkerControlActionStarted => "worker_control_action_started",
            Self::WorkerControlActionResultRecorded => "worker_control_action_result_recorded",
            Self::LocalSessionRegistered => "local_session_registered",
            Self::SessionEndpointBound => "session_endpoint_bound",
            Self::WorkerSessionBound => "worker_session_bound",
            Self::WorkerSessionSuccessorBound => "worker_session_successor_bound",
            Self::ControllerSessionDesignated => "controller_session_designated",
            Self::ControllerWorkerBound => "controller_worker_bound",
            Self::WorkerControlIssuerBound => "worker_control_issuer_bound",
            Self::WorkerContinuationExecutionStarted => "worker_continuation_execution_started",
            Self::WorkerContinuationExecutionResultRecorded => {
                "worker_continuation_execution_result_recorded"
            }
            Self::ControllerWorkerResultContextDecisionRecorded => {
                "controller_worker_result_context_decision_recorded"
            }
            Self::ControllerCoordinationTurnStarted => "controller_coordination_turn_started",
            Self::ControllerCoordinationTurnResultRecorded => {
                "controller_coordination_turn_result_recorded"
            }
            Self::ControllerCoordinationResultContextDecisionRecorded => {
                "controller_coordination_result_context_decision_recorded"
            }
            Self::ControllerCoordinationSuggestionRecorded => {
                "controller_coordination_suggestion_recorded"
            }
            Self::ControllerCoordinationSuggestionPromoted => {
                "controller_coordination_suggestion_promoted"
            }
            Self::ContinuationLeaseCreated => "continuation_lease_created",
            Self::ContinuationPermitIssued => "continuation_permit_issued",
            Self::RemoteConversationBound => "remote_conversation_bound",
            Self::RemoteReadObservationRecorded => "remote_read_observation_recorded",
            Self::RemoteMirrorSelectionChanged => "remote_mirror_selection_changed",
            Self::RemoteConversationSnapshotImported => "remote_conversation_snapshot_imported",
            Self::RemoteMirrorQueueItemQueued => "remote_mirror_queue_item_queued",
            Self::RemoteMirrorQueueCaptureStarted => "remote_mirror_queue_capture_started",
            Self::RemoteMirrorQueueCompleted => "remote_mirror_queue_completed",
            Self::RemoteMirrorQueueRateLimited => "remote_mirror_queue_rate_limited",
            Self::RemoteMirrorQueueFailed => "remote_mirror_queue_failed",
            Self::RemoteHealthObserved => "remote_health_observed",
            Self::ChatContainerCreated => "chat_container_created",
            Self::ChatSessionLifecycleTransitionRecorded => {
                "chat_session_lifecycle_transition_recorded"
            }
            Self::ChatSessionSuccessorBound => "chat_session_successor_bound",
        }
    }

    /// Parse a stable persisted event name.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "draft_changed" => Some(Self::DraftChanged),
            "user_message_committed" => Some(Self::UserMessageCommitted),
            "dispatch_attempted" => Some(Self::DispatchAttempted),
            "remote_acceptance_observed" => Some(Self::RemoteAcceptanceObserved),
            "remote_failure_observed" => Some(Self::RemoteFailureObserved),
            "assistant_stream_started" => Some(Self::AssistantStreamStarted),
            "assistant_delta_observed" => Some(Self::AssistantDeltaObserved),
            "assistant_snapshot_observed" => Some(Self::AssistantSnapshotObserved),
            "assistant_status_observed" => Some(Self::AssistantStatusObserved),
            "transcript_user_message_observed" => Some(Self::TranscriptUserMessageObserved),
            "client_error_observed" => Some(Self::ClientErrorObserved),
            "assistant_completion_observed" => Some(Self::AssistantCompletionObserved),
            "transport_interrupted" => Some(Self::TransportInterrupted),
            "reconciliation_attempted" => Some(Self::ReconciliationAttempted),
            "reconciliation_observed" => Some(Self::ReconciliationObserved),
            "import_started" => Some(Self::ImportStarted),
            "import_completed" => Some(Self::ImportCompleted),
            "historical_conversation_snapshot_imported" => {
                Some(Self::HistoricalConversationSnapshotImported)
            }
            "route_proposed" => Some(Self::RouteProposed),
            "route_payload_attached" => Some(Self::RoutePayloadAttached),
            "route_user_decision_recorded" => Some(Self::RouteUserDecisionRecorded),
            "route_dispatched" => Some(Self::RouteDispatched),
            "local_route_delivered" => Some(Self::LocalRouteDelivered),
            "local_route_context_decision_recorded" => {
                Some(Self::LocalRouteContextDecisionRecorded)
            }
            "local_memory_artifact_recorded" => Some(Self::LocalMemoryArtifactRecorded),
            "local_memory_context_decision_recorded" => {
                Some(Self::LocalMemoryContextDecisionRecorded)
            }
            "route_result_observed" => Some(Self::RouteResultObserved),
            "local_conversation_worker_bound" => Some(Self::LocalConversationWorkerBound),
            "local_conversation_chat_container_bound" => {
                Some(Self::LocalConversationChatContainerBound)
            }
            "worker_goal_assigned" => Some(Self::WorkerGoalAssigned),
            "worker_lifecycle_transition_recorded" => Some(Self::WorkerLifecycleTransitionRecorded),
            "worker_control_admitted" => Some(Self::WorkerControlAdmitted),
            "control_route_bound" => Some(Self::ControlRouteBound),
            "worker_control_delivered" => Some(Self::WorkerControlDelivered),
            "worker_control_acknowledged" => Some(Self::WorkerControlAcknowledged),
            "worker_control_status_result_recorded" => {
                Some(Self::WorkerControlStatusResultRecorded)
            }
            "worker_control_action_started" => Some(Self::WorkerControlActionStarted),
            "worker_control_action_result_recorded" => {
                Some(Self::WorkerControlActionResultRecorded)
            }
            "local_session_registered" => Some(Self::LocalSessionRegistered),
            "session_endpoint_bound" => Some(Self::SessionEndpointBound),
            "worker_session_bound" => Some(Self::WorkerSessionBound),
            "worker_session_successor_bound" => Some(Self::WorkerSessionSuccessorBound),
            "controller_session_designated" => Some(Self::ControllerSessionDesignated),
            "controller_worker_bound" => Some(Self::ControllerWorkerBound),
            "worker_control_issuer_bound" => Some(Self::WorkerControlIssuerBound),
            "worker_continuation_execution_started" => {
                Some(Self::WorkerContinuationExecutionStarted)
            }
            "worker_continuation_execution_result_recorded" => {
                Some(Self::WorkerContinuationExecutionResultRecorded)
            }
            "controller_worker_result_context_decision_recorded" => {
                Some(Self::ControllerWorkerResultContextDecisionRecorded)
            }
            "controller_coordination_turn_started" => Some(Self::ControllerCoordinationTurnStarted),
            "controller_coordination_turn_result_recorded" => {
                Some(Self::ControllerCoordinationTurnResultRecorded)
            }
            "controller_coordination_result_context_decision_recorded" => {
                Some(Self::ControllerCoordinationResultContextDecisionRecorded)
            }
            "controller_coordination_suggestion_recorded" => {
                Some(Self::ControllerCoordinationSuggestionRecorded)
            }
            "controller_coordination_suggestion_promoted" => {
                Some(Self::ControllerCoordinationSuggestionPromoted)
            }
            "continuation_lease_created" => Some(Self::ContinuationLeaseCreated),
            "continuation_permit_issued" => Some(Self::ContinuationPermitIssued),
            "remote_conversation_bound" => Some(Self::RemoteConversationBound),
            "remote_read_observation_recorded" => Some(Self::RemoteReadObservationRecorded),
            "remote_mirror_selection_changed" => Some(Self::RemoteMirrorSelectionChanged),
            "remote_conversation_snapshot_imported" => {
                Some(Self::RemoteConversationSnapshotImported)
            }
            "remote_mirror_queue_item_queued" => Some(Self::RemoteMirrorQueueItemQueued),
            "remote_mirror_queue_capture_started" => Some(Self::RemoteMirrorQueueCaptureStarted),
            "remote_mirror_queue_completed" => Some(Self::RemoteMirrorQueueCompleted),
            "remote_mirror_queue_rate_limited" => Some(Self::RemoteMirrorQueueRateLimited),
            "remote_mirror_queue_failed" => Some(Self::RemoteMirrorQueueFailed),
            "remote_health_observed" => Some(Self::RemoteHealthObserved),
            "chat_container_created" => Some(Self::ChatContainerCreated),
            "chat_session_lifecycle_transition_recorded" => {
                Some(Self::ChatSessionLifecycleTransitionRecorded)
            }
            "chat_session_successor_bound" => Some(Self::ChatSessionSuccessorBound),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalEvidence,
        LocalMessageId, LocalTurnId, RemoteEvidence, TurnEvidence, TurnReplayError,
    };
    use std::str::FromStr;

    #[test]
    fn local_ids_round_trip_without_cross_type_conflation() {
        let conversation = LocalConversationId::new();
        let turn = LocalTurnId::new();
        let message = LocalMessageId::new();

        assert_ne!(conversation.to_string(), turn.to_string());
        assert_ne!(conversation.to_string(), message.to_string());
        assert_ne!(turn.to_string(), message.to_string());

        assert_eq!(
            LocalConversationId::from_str(&conversation.to_string()).unwrap(),
            conversation
        );
        assert_eq!(LocalTurnId::from_str(&turn.to_string()).unwrap(), turn);
        assert_eq!(
            LocalMessageId::from_str(&message.to_string()).unwrap(),
            message
        );
    }

    #[test]
    fn local_ids_are_uuid_v7_but_not_used_as_order_authority() {
        let conversation = LocalConversationId::new();
        assert_eq!(conversation.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn authored_user_message_preserves_typed_identity_and_exact_text() {
        let conversation_id = LocalConversationId::new();
        let turn_id = LocalTurnId::new();
        let message_id = LocalMessageId::new();
        let text = " exact text\nwith spacing ";
        let authored = AuthoredUserMessage::new(conversation_id, turn_id, message_id, text);

        assert_eq!(authored.conversation_id, conversation_id);
        assert_eq!(authored.turn_id, turn_id);
        assert_eq!(authored.message_id, message_id);
        assert_eq!(authored.text, text);
    }

    #[test]
    fn replay_rejects_dispatch_before_commit() {
        let error = TurnEvidence::replay_event_kinds([EventKind::DispatchAttempted])
            .expect_err("dispatch before commit must fail");
        assert_eq!(error, TurnReplayError::DispatchBeforeCommit);
    }

    #[test]
    fn replay_preserves_ambiguous_remote_outcome() {
        let evidence = TurnEvidence::replay_event_kinds([
            EventKind::UserMessageCommitted,
            EventKind::DispatchAttempted,
            EventKind::TransportInterrupted,
        ])
        .expect("replay");

        assert_eq!(evidence.local, LocalEvidence::MessageCommitted);
        assert_eq!(evidence.remote, RemoteEvidence::OutcomeUnknown);
        assert_eq!(evidence.assistant, AssistantEvidence::None);
    }

    #[test]
    fn replay_preserves_acceptance_and_partial_output_across_interruption() {
        let evidence = TurnEvidence::replay_event_kinds([
            EventKind::UserMessageCommitted,
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::AssistantStreamStarted,
            EventKind::AssistantDeltaObserved,
            EventKind::TransportInterrupted,
        ])
        .expect("replay");

        assert_eq!(evidence.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(evidence.assistant, AssistantEvidence::PartialInterrupted);
    }

    #[test]
    fn later_completion_advances_interrupted_assistant_without_remote_inference() {
        let evidence = TurnEvidence::replay_event_kinds([
            EventKind::UserMessageCommitted,
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::AssistantStreamStarted,
            EventKind::TransportInterrupted,
            EventKind::AssistantCompletionObserved,
            EventKind::AssistantSnapshotObserved,
        ])
        .expect("replay");

        assert_eq!(evidence.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(evidence.assistant, AssistantEvidence::CompletedObserved);
    }

    #[test]
    fn replay_does_not_invent_remote_acceptance_or_failure() {
        let evidence = TurnEvidence::replay_event_kinds([
            EventKind::UserMessageCommitted,
            EventKind::AssistantSnapshotObserved,
        ])
        .expect("replay");

        assert_eq!(evidence.remote, RemoteEvidence::NotAttempted);
        assert_eq!(evidence.assistant, AssistantEvidence::Streaming);
    }

    #[test]
    fn dispatch_cannot_outrun_local_durability() {
        let mut turn = TurnEvidence::default();
        assert!(!turn.begin_dispatch());
        assert_eq!(turn.local, LocalEvidence::DraftOnly);
        assert_eq!(turn.remote, RemoteEvidence::NotAttempted);
    }

    #[test]
    fn disconnect_after_dispatch_preserves_uncertainty() {
        let mut turn = TurnEvidence::default();
        turn.commit_local_message();
        assert!(turn.begin_dispatch());
        turn.mark_transport_ambiguous();
        assert_eq!(turn.local, LocalEvidence::MessageCommitted);
        assert_eq!(turn.remote, RemoteEvidence::OutcomeUnknown);
    }

    #[test]
    fn disconnect_during_stream_preserves_partial_output_state() {
        let mut turn = TurnEvidence::default();
        turn.commit_local_message();
        assert!(turn.begin_dispatch());
        turn.observe_acceptance();
        turn.observe_assistant_output();
        turn.mark_transport_ambiguous();
        assert_eq!(turn.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(turn.assistant, AssistantEvidence::PartialInterrupted);
    }

    #[test]
    fn observed_failure_is_not_conflated_with_ambiguity() {
        let mut turn = TurnEvidence::default();
        turn.commit_local_message();
        assert!(turn.begin_dispatch());
        turn.observe_remote_failure();
        turn.mark_transport_ambiguous();
        assert_eq!(turn.remote, RemoteEvidence::FailedObserved);
    }

    #[test]
    fn stable_event_names_round_trip() {
        let kinds = [
            EventKind::DraftChanged,
            EventKind::UserMessageCommitted,
            EventKind::DispatchAttempted,
            EventKind::RemoteAcceptanceObserved,
            EventKind::RemoteFailureObserved,
            EventKind::AssistantStreamStarted,
            EventKind::AssistantDeltaObserved,
            EventKind::AssistantSnapshotObserved,
            EventKind::AssistantStatusObserved,
            EventKind::TranscriptUserMessageObserved,
            EventKind::ClientErrorObserved,
            EventKind::AssistantCompletionObserved,
            EventKind::TransportInterrupted,
            EventKind::ReconciliationAttempted,
            EventKind::ReconciliationObserved,
            EventKind::ImportStarted,
            EventKind::ImportCompleted,
            EventKind::RemoteMirrorSelectionChanged,
        ];

        for kind in kinds {
            assert_eq!(EventKind::from_stable_name(kind.stable_name()), Some(kind));
        }
        assert_eq!(EventKind::from_stable_name("future_event"), None);
    }
}
