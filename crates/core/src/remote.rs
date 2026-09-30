//! Endpoint-agnostic local/remote conversation identity provenance.
//!
//! Remote ChatGPT identifiers are treated as exact opaque strings. This module
//! deliberately does not assume UUID syntax, endpoint shape, authentication,
//! synchronization state, or remote mutability.

use crate::LocalConversationId;
use std::fmt;

/// Invalid remote-identity provenance input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteIdentityError {
    /// Remote conversation identity was empty.
    EmptyRemoteConversationId,
    /// Protocol observation revision was empty.
    EmptyProtocolObservationRevision,
}

impl fmt::Display for RemoteIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRemoteConversationId => {
                write!(formatter, "remote conversation identity must not be empty")
            }
            Self::EmptyProtocolObservationRevision => {
                write!(formatter, "protocol observation revision must not be empty")
            }
        }
    }
}

impl std::error::Error for RemoteIdentityError {}

/// Exact opaque remote ChatGPT conversation identity.
///
/// No textual normalization or format interpretation is performed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemoteConversationId(String);

impl RemoteConversationId {
    /// Construct an exact opaque remote conversation identity.
    pub fn new(value: impl Into<String>) -> Result<Self, RemoteIdentityError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RemoteIdentityError::EmptyRemoteConversationId);
        }
        Ok(Self(value))
    }

    /// Exact remote identity as observed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteConversationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Named empirical protocol observation supporting one remote identity binding.
///
/// Core intentionally treats revisions as opaque labels rather than parsing their
/// timestamp-oriented repository convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtocolObservationRevision(String);

impl ProtocolObservationRevision {
    /// Construct an exact non-empty observation revision label.
    pub fn new(value: impl Into<String>) -> Result<Self, RemoteIdentityError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RemoteIdentityError::EmptyProtocolObservationRevision);
        }
        Ok(Self(value))
    }

    /// Exact empirical observation revision label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProtocolObservationRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One provenance-preserving local-to-remote conversation correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConversationBinding {
    local_conversation_id: LocalConversationId,
    remote_conversation_id: RemoteConversationId,
    protocol_revision: ProtocolObservationRevision,
}

impl RemoteConversationBinding {
    /// Correlate one local conversation to one positively observed remote identity.
    #[must_use]
    pub fn new(
        local_conversation_id: LocalConversationId,
        remote_conversation_id: RemoteConversationId,
        protocol_revision: ProtocolObservationRevision,
    ) -> Self {
        Self {
            local_conversation_id,
            remote_conversation_id,
            protocol_revision,
        }
    }

    /// Local Chatarium conversation identity.
    #[must_use]
    pub const fn local_conversation_id(&self) -> LocalConversationId {
        self.local_conversation_id
    }

    /// Exact opaque remote conversation identity.
    #[must_use]
    pub fn remote_conversation_id(&self) -> &RemoteConversationId {
        &self.remote_conversation_id
    }

    /// Empirical protocol observation supporting the binding.
    #[must_use]
    pub fn protocol_revision(&self) -> &ProtocolObservationRevision {
        &self.protocol_revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_identity_preserves_non_uuid_exact_text() {
        let raw = "opaque/remote:id_ABC-123";
        let id = RemoteConversationId::new(raw).unwrap();
        assert_eq!(id.as_str(), raw);
        assert_eq!(id.to_string(), raw);
    }

    #[test]
    fn remote_identity_rejects_only_empty_input_without_normalizing() {
        assert_eq!(
            RemoteConversationId::new(""),
            Err(RemoteIdentityError::EmptyRemoteConversationId)
        );
        assert_eq!(
            RemoteConversationId::new("  ").unwrap().as_str(),
            "  ",
            "core must not silently trim opaque remote identity"
        );
    }

    #[test]
    fn protocol_revision_is_exact_and_non_empty() {
        let revision = ProtocolObservationRevision::new("2026-09-29.002").unwrap();
        assert_eq!(revision.as_str(), "2026-09-29.002");
        assert_eq!(
            ProtocolObservationRevision::new(""),
            Err(RemoteIdentityError::EmptyProtocolObservationRevision)
        );
    }

    #[test]
    fn remote_binding_event_does_not_mutate_turn_evidence() {
        let mut evidence = crate::TurnEvidence::default();
        evidence
            .apply_event_kind(crate::EventKind::RemoteConversationBound)
            .unwrap();
        assert_eq!(evidence, crate::TurnEvidence::default());
    }

    #[test]
    fn binding_preserves_all_identity_domains() {
        let local = LocalConversationId::new();
        let remote = RemoteConversationId::new("remote-conversation").unwrap();
        let revision = ProtocolObservationRevision::new("observation-x").unwrap();
        let binding = RemoteConversationBinding::new(local, remote.clone(), revision.clone());

        assert_eq!(binding.local_conversation_id(), local);
        assert_eq!(binding.remote_conversation_id(), &remote);
        assert_eq!(binding.protocol_revision(), &revision);
    }
}
