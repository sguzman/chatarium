//! Domain model for Chatarium reliability state.

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
    /// A remote failure was observed.
    RemoteFailureObserved,
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
    use super::{AssistantEvidence, LocalEvidence, RemoteEvidence, TurnEvidence};

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
}
