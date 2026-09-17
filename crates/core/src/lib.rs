//! Domain model for Chatarium reliability state.

/// Evidence Chatarium has about the remote handling of a local turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteEvidence {
    /// No remote mutation has been attempted.
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
    /// Remote mutation evidence.
    pub remote: RemoteEvidence,
    /// Assistant-output evidence.
    pub assistant: AssistantEvidence,
}

impl TurnEvidence {
    /// Mark the start of a remote dispatch attempt.
    pub fn begin_dispatch(&mut self) {
        self.remote = RemoteEvidence::Dispatching;
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

    /// Record the first observed assistant output.
    pub fn observe_assistant_output(&mut self) {
        self.assistant = AssistantEvidence::Streaming;
    }

    /// Record positive evidence that assistant generation completed.
    pub fn observe_completion(&mut self) {
        self.assistant = AssistantEvidence::CompletedObserved;
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
    /// Assistant output began.
    AssistantStreamStarted,
    /// Assistant output changed.
    AssistantDeltaObserved,
    /// Assistant completion was observed.
    AssistantCompletionObserved,
    /// Transport was interrupted.
    TransportInterrupted,
    /// Reconciliation was attempted.
    ReconciliationAttempted,
    /// Reconciliation produced a new observation.
    ReconciliationObserved,
}

#[cfg(test)]
mod tests {
    use super::{AssistantEvidence, RemoteEvidence, TurnEvidence};

    #[test]
    fn disconnect_after_dispatch_preserves_uncertainty() {
        let mut turn = TurnEvidence::default();
        turn.begin_dispatch();
        turn.mark_transport_ambiguous();
        assert_eq!(turn.remote, RemoteEvidence::OutcomeUnknown);
    }

    #[test]
    fn disconnect_during_stream_preserves_partial_output_state() {
        let mut turn = TurnEvidence::default();
        turn.begin_dispatch();
        turn.observe_acceptance();
        turn.observe_assistant_output();
        turn.mark_transport_ambiguous();
        assert_eq!(turn.remote, RemoteEvidence::AcceptedObserved);
        assert_eq!(turn.assistant, AssistantEvidence::PartialInterrupted);
    }
}
