//! Issuer provenance for typed worker controls.
//!
//! Provenance identifies who issued a control without granting any additional
//! routing, dispatch, continuation, or lifecycle authority.

use crate::control::ControlId;
use crate::session::SessionId;

/// Explicit issuer of one admitted worker control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlIssuer {
    /// Direct operator/user action.
    User,
    /// A designated controller session issued the control.
    ControllerSession(SessionId),
}

/// One admitted control correlated to exactly one issuer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlProvenance {
    control_id: ControlId,
    issuer: ControlIssuer,
}

impl ControlProvenance {
    /// Construct explicit issuer provenance for one admitted control.
    #[must_use]
    pub const fn new(control_id: ControlId, issuer: ControlIssuer) -> Self {
        Self { control_id, issuer }
    }

    /// Admitted control identity.
    #[must_use]
    pub const fn control_id(self) -> ControlId {
        self.control_id
    }

    /// Explicit issuer.
    #[must_use]
    pub const fn issuer(self) -> ControlIssuer {
        self.issuer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_provenance_preserves_control_identity() {
        let provenance = ControlProvenance::new(ControlId::new(1), ControlIssuer::User);
        assert_eq!(provenance.control_id(), ControlId::new(1));
        assert_eq!(provenance.issuer(), ControlIssuer::User);
    }

    #[test]
    fn controller_provenance_preserves_session_identity() {
        let session = SessionId::new(7);
        let provenance =
            ControlProvenance::new(ControlId::new(1), ControlIssuer::ControllerSession(session));
        assert_eq!(
            provenance.issuer(),
            ControlIssuer::ControllerSession(session)
        );
    }
}
