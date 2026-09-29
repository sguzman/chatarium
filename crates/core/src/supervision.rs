//! Pure-domain controller/worker session supervision.
//!
//! Supervision is coordination provenance only. It grants no routing-policy
//! authority, continuation authority, transport ability, or lifecycle mutation.

use crate::session::SessionId;

/// Explicit local designation of one session as a controller/coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerDesignation {
    session_id: SessionId,
}

impl ControllerDesignation {
    /// Designate one already-known local session as a controller.
    #[must_use]
    pub const fn new(session_id: SessionId) -> Self {
        Self { session_id }
    }

    /// Designated controller session.
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        self.session_id
    }
}

/// Directed controller-session -> worker-session supervision relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerWorkerBinding {
    controller_session_id: SessionId,
    worker_session_id: SessionId,
}

impl ControllerWorkerBinding {
    /// Construct a supervision relationship.
    ///
    /// Self-supervision is rejected. Durable replay validates registration,
    /// controller designation, and worker-role facts.
    pub fn new(
        controller_session_id: SessionId,
        worker_session_id: SessionId,
    ) -> Result<Self, SupervisionError> {
        if controller_session_id == worker_session_id {
            return Err(SupervisionError::SelfSupervision {
                session_id: controller_session_id,
            });
        }

        Ok(Self {
            controller_session_id,
            worker_session_id,
        })
    }

    /// Controller/coordinator session.
    #[must_use]
    pub const fn controller_session_id(self) -> SessionId {
        self.controller_session_id
    }

    /// Supervised worker session.
    #[must_use]
    pub const fn worker_session_id(self) -> SessionId {
        self.worker_session_id
    }
}

/// Pure-domain supervision construction error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisionError {
    /// A session cannot supervise itself.
    SelfSupervision {
        /// Session referenced on both sides.
        session_id: SessionId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_designation_preserves_session_identity() {
        let session = SessionId::new(1);
        assert_eq!(ControllerDesignation::new(session).session_id(), session);
    }

    #[test]
    fn controller_worker_binding_preserves_direction() {
        let controller = SessionId::new(1);
        let worker = SessionId::new(2);
        let binding = ControllerWorkerBinding::new(controller, worker).unwrap();

        assert_eq!(binding.controller_session_id(), controller);
        assert_eq!(binding.worker_session_id(), worker);
    }

    #[test]
    fn self_supervision_is_rejected() {
        let session = SessionId::new(1);
        assert_eq!(
            ControllerWorkerBinding::new(session, session),
            Err(SupervisionError::SelfSupervision {
                session_id: session,
            })
        );
    }
}
