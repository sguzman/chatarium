//! Local session identity and control-plane bindings.
//!
//! These types distinguish local session identity from worker identity, routing
//! endpoint identity, and future remote ChatGPT conversation/session identity.

use crate::orchestration::WorkerId;
use crate::routing::RouteEndpointId;
use std::fmt;

/// Opaque local identity for one Chatarium session surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(u64);

impl SessionId {
    /// Construct a local session identity from a caller-owned value.
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

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Durable local correlation between a session and its routing endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEndpointBinding {
    session_id: SessionId,
    endpoint_id: RouteEndpointId,
}

impl SessionEndpointBinding {
    /// Construct one local session-to-endpoint identity binding.
    #[must_use]
    pub const fn new(session_id: SessionId, endpoint_id: RouteEndpointId) -> Self {
        Self {
            session_id,
            endpoint_id,
        }
    }

    /// Bound local session.
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }

    /// Bound routing endpoint.
    #[must_use]
    pub const fn endpoint_id(self) -> RouteEndpointId {
        self.endpoint_id
    }
}

/// Durable local correlation between an orchestration worker and a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSessionBinding {
    worker_id: WorkerId,
    session_id: SessionId,
}

impl WorkerSessionBinding {
    /// Construct one worker-to-session identity binding.
    #[must_use]
    pub const fn new(worker_id: WorkerId, session_id: SessionId) -> Self {
        Self {
            worker_id,
            session_id,
        }
    }

    /// Bound worker identity.
    #[must_use]
    pub const fn worker_id(self) -> WorkerId {
        self.worker_id
    }

    /// Bound local session identity.
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }
}

/// Explicit handoff of one persistent worker identity from a predecessor session
/// leaf to a successor session leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSessionSuccessorBinding {
    worker_id: WorkerId,
    predecessor_session_id: SessionId,
    successor_session_id: SessionId,
}

impl WorkerSessionSuccessorBinding {
    /// Construct one worker-session successor handoff.
    pub fn new(
        worker_id: WorkerId,
        predecessor_session_id: SessionId,
        successor_session_id: SessionId,
    ) -> Result<Self, WorkerSessionSuccessorBindingError> {
        if predecessor_session_id == successor_session_id {
            return Err(WorkerSessionSuccessorBindingError::SameSession {
                session_id: predecessor_session_id,
            });
        }
        Ok(Self {
            worker_id,
            predecessor_session_id,
            successor_session_id,
        })
    }

    #[must_use]
    pub const fn worker_id(self) -> WorkerId {
        self.worker_id
    }

    #[must_use]
    pub const fn predecessor_session_id(self) -> SessionId {
        self.predecessor_session_id
    }

    #[must_use]
    pub const fn successor_session_id(self) -> SessionId {
        self.successor_session_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerSessionSuccessorBindingError {
    SameSession { session_id: SessionId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_session_successor_requires_distinct_leaves() {
        let worker = WorkerId::new(11);
        let predecessor = SessionId::new(7);
        let successor = SessionId::new(8);
        let binding = WorkerSessionSuccessorBinding::new(worker, predecessor, successor).unwrap();

        assert_eq!(binding.worker_id(), worker);
        assert_eq!(binding.predecessor_session_id(), predecessor);
        assert_eq!(binding.successor_session_id(), successor);
        assert_eq!(
            WorkerSessionSuccessorBinding::new(worker, predecessor, predecessor),
            Err(WorkerSessionSuccessorBindingError::SameSession {
                session_id: predecessor,
            })
        );
    }

    #[test]
    fn bindings_preserve_distinct_identity_domains() {
        let session = SessionId::new(7);
        let endpoint = RouteEndpointId::new(9);
        let worker = WorkerId::new(11);

        let endpoint_binding = SessionEndpointBinding::new(session, endpoint);
        let worker_binding = WorkerSessionBinding::new(worker, session);

        assert_eq!(endpoint_binding.session_id(), session);
        assert_eq!(endpoint_binding.endpoint_id(), endpoint);
        assert_eq!(worker_binding.worker_id(), worker);
        assert_eq!(worker_binding.session_id(), session);
    }
}
