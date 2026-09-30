//! Logical chat containers and local session rollover semantics.
//!
//! A Chatarium chat container outlives any one local/remote ChatGPT session.
//! Session saturation is therefore modeled as an expected continuity transition,
//! not as a conversation-level failure.

use crate::session::SessionId;
use std::fmt;

/// Opaque local identity for one logical Chatarium chat spanning session rollovers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatContainerId(u64);

impl ChatContainerId {
    /// Construct a caller-owned logical chat-container identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value for persistence/diagnostics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ChatContainerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Opaque local identity for context material carried across one session rollover.
///
/// The identity does not define the representation or delivery mechanism. A later
/// adapter may realize it as a transcript attachment, continuity capsule, local
/// retrieval source, or another empirically supported mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContextHandoffId(u64);

impl ContextHandoffId {
    /// Construct a caller-owned context-handoff identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the opaque local value for persistence/diagnostics.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ContextHandoffId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Local lifecycle of one physical ChatGPT/Chatarium session inside a chat container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLifecyclePhase {
    /// Session is the active leaf and has no known continuity pressure.
    Healthy,
    /// Session remains usable but continuity pressure has been observed.
    Aging,
    /// Session must not receive new ordinary work; a successor is required.
    Saturated,
    /// Continuity has moved to a successor. This is terminal, but not a failure.
    Retired,
}

impl SessionLifecyclePhase {
    /// Stable persisted spelling.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Aging => "aging",
            Self::Saturated => "saturated",
            Self::Retired => "retired",
        }
    }

    /// Parse a stable persisted spelling.
    #[must_use]
    pub fn from_stable_name(value: &str) -> Option<Self> {
        match value {
            "healthy" => Some(Self::Healthy),
            "aging" => Some(Self::Aging),
            "saturated" => Some(Self::Saturated),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    /// Whether ordinary conversational turns may still target this session.
    #[must_use]
    pub const fn accepts_ordinary_turns(self) -> bool {
        matches!(self, Self::Healthy | Self::Aging)
    }

    /// Whether the lineage needs a successor before ordinary work can continue.
    #[must_use]
    pub const fn successor_required(self) -> bool {
        matches!(self, Self::Saturated)
    }

    /// Whether this phase is terminal for the physical session.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired)
    }

    /// Whether an explicit phase-change event may move from this phase to the next phase.
    ///
    /// Retired is intentionally excluded here: retirement is produced only by
    /// a validated successor binding, so retired always has continuity provenance.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Healthy, Self::Aging | Self::Saturated)
                | (Self::Aging, Self::Saturated)
        )
    }
}

/// Invalid explicit session lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifecycleTransitionError {
    pub from: SessionLifecyclePhase,
    pub to: SessionLifecyclePhase,
}

impl fmt::Display for SessionLifecycleTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid session lifecycle transition {} -> {}",
            self.from.stable_name(),
            self.to.stable_name()
        )
    }
}

impl std::error::Error for SessionLifecycleTransitionError {}

/// One explicit non-retirement session lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLifecycleTransition {
    session_id: SessionId,
    from: SessionLifecyclePhase,
    to: SessionLifecyclePhase,
}

impl SessionLifecycleTransition {
    /// Construct a validated explicit phase transition.
    pub fn new(
        session_id: SessionId,
        from: SessionLifecyclePhase,
        to: SessionLifecyclePhase,
    ) -> Result<Self, SessionLifecycleTransitionError> {
        if !from.can_transition_to(to) {
            return Err(SessionLifecycleTransitionError { from, to });
        }
        Ok(Self {
            session_id,
            from,
            to,
        })
    }

    /// Session whose phase changes.
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    /// Required prior phase.
    #[must_use]
    pub const fn from(self) -> SessionLifecyclePhase {
        self.from
    }

    /// Resulting phase.
    #[must_use]
    pub const fn to(self) -> SessionLifecyclePhase {
        self.to
    }
}

/// Invalid successor binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSuccessorBindingError {
    SameSession,
}

impl fmt::Display for SessionSuccessorBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SameSession => write!(formatter, "a session cannot succeed itself"),
        }
    }
}

impl std::error::Error for SessionSuccessorBindingError {}

/// Durable local continuity edge from one saturated session to its successor.
///
/// The context-handoff identity establishes provenance/correlation only. It does
/// not claim that a remote conversation has already been created or that context
/// has already been delivered to ChatGPT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSuccessorBinding {
    container_id: ChatContainerId,
    predecessor_session_id: SessionId,
    successor_session_id: SessionId,
    context_handoff_id: ContextHandoffId,
}

impl SessionSuccessorBinding {
    /// Construct one successor edge.
    pub fn new(
        container_id: ChatContainerId,
        predecessor_session_id: SessionId,
        successor_session_id: SessionId,
        context_handoff_id: ContextHandoffId,
    ) -> Result<Self, SessionSuccessorBindingError> {
        if predecessor_session_id == successor_session_id {
            return Err(SessionSuccessorBindingError::SameSession);
        }
        Ok(Self {
            container_id,
            predecessor_session_id,
            successor_session_id,
            context_handoff_id,
        })
    }

    #[must_use]
    pub const fn container_id(self) -> ChatContainerId {
        self.container_id
    }

    #[must_use]
    pub const fn predecessor_session_id(self) -> SessionId {
        self.predecessor_session_id
    }

    #[must_use]
    pub const fn successor_session_id(self) -> SessionId {
        self.successor_session_id
    }

    #[must_use]
    pub const fn context_handoff_id(self) -> ContextHandoffId {
        self.context_handoff_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturated_is_expected_rollover_state_not_failure() {
        assert!(SessionLifecyclePhase::Healthy.accepts_ordinary_turns());
        assert!(SessionLifecyclePhase::Aging.accepts_ordinary_turns());
        assert!(!SessionLifecyclePhase::Saturated.accepts_ordinary_turns());
        assert!(SessionLifecyclePhase::Saturated.successor_required());
        assert!(SessionLifecyclePhase::Retired.is_terminal());
        assert!(!SessionLifecyclePhase::Retired.successor_required());
    }

    #[test]
    fn explicit_transitions_are_monotonic_and_cannot_fake_retirement() {
        let session = SessionId::new(1);
        assert!(
            SessionLifecycleTransition::new(
                session,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Aging,
            )
            .is_ok()
        );
        assert!(
            SessionLifecycleTransition::new(
                session,
                SessionLifecyclePhase::Healthy,
                SessionLifecyclePhase::Saturated,
            )
            .is_ok()
        );
        assert!(
            SessionLifecycleTransition::new(
                session,
                SessionLifecyclePhase::Aging,
                SessionLifecyclePhase::Saturated,
            )
            .is_ok()
        );
        assert!(
            SessionLifecycleTransition::new(
                session,
                SessionLifecyclePhase::Saturated,
                SessionLifecyclePhase::Retired,
            )
            .is_err()
        );
        assert!(
            SessionLifecycleTransition::new(
                session,
                SessionLifecyclePhase::Aging,
                SessionLifecyclePhase::Healthy,
            )
            .is_err()
        );
    }

    #[test]
    fn successor_binding_preserves_all_identity_domains() {
        let binding = SessionSuccessorBinding::new(
            ChatContainerId::new(7),
            SessionId::new(1),
            SessionId::new(2),
            ContextHandoffId::new(9),
        )
        .unwrap();

        assert_eq!(binding.container_id(), ChatContainerId::new(7));
        assert_eq!(binding.predecessor_session_id(), SessionId::new(1));
        assert_eq!(binding.successor_session_id(), SessionId::new(2));
        assert_eq!(binding.context_handoff_id(), ContextHandoffId::new(9));
    }
}
