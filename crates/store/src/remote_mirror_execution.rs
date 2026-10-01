//! Final pre-network P3 gate for selected remote conversation mirroring.
//!
//! Durable selection/readiness and live authenticated authority are intentionally
//! independent. This module still performs no network I/O and appends no events.

use crate::EventEnvelope;
use crate::remote_mirror_readiness::{RemoteMirrorBlocked, RemoteMirrorReady};
use crate::remote_mirror_selection_audit::{
    SelectedRemoteMirrorPlan, derive_selected_remote_mirror_plan,
};
use chatarium_core::LocalConversationId;
use chatarium_core::authenticated_session::{
    AuthenticatedSessionLease, SessionLeaseError, UserAuthenticatedSessionProvider,
};

/// Persistent P3 mirror state immediately before transient authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMirrorExecutionPlan {
    /// The user has not selected this conversation for mirroring.
    Unselected,
    /// Durable protocol/evidence prerequisites block mirroring.
    ProtocolBlocked(RemoteMirrorBlocked),
    /// Durable intent and protocol evidence are ready; a live authenticated session is still required.
    RequiresAuthenticatedSession(RemoteMirrorReady),
}

/// Failure to authorize a remote mirror read from persistent plan state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteMirrorAuthorizationError {
    /// The conversation is not selected for mirroring.
    Unselected,
    /// Durable protocol/evidence prerequisites are not satisfied.
    ProtocolBlocked(RemoteMirrorBlocked),
}

/// Borrow-scoped authority for a future read-only remote mirror operation.
///
/// This value combines durable readiness evidence with a live authenticated-session
/// lease. It is not durable, not clonable, and does not itself perform a request.
pub struct AuthorizedRemoteMirrorRead<'a, P: UserAuthenticatedSessionProvider + ?Sized> {
    ready: RemoteMirrorReady,
    session: AuthenticatedSessionLease<'a, P>,
}

impl<'a, P: UserAuthenticatedSessionProvider + ?Sized> AuthorizedRemoteMirrorRead<'a, P> {
    /// Durable evidence that passed the mirror-readiness gate.
    #[must_use]
    pub const fn ready(&self) -> &RemoteMirrorReady {
        &self.ready
    }

    /// Run adapter-specific logic only after revalidating the live session authority.
    ///
    /// This boundary still does not expose or copy any reusable authentication material,
    /// and stale/uncertain authentication fails closed before the operation runs.
    pub fn with_authenticated_session_provider<R>(
        &mut self,
        operation: impl FnOnce(&mut P) -> R,
    ) -> Result<R, SessionLeaseError<P::Error>> {
        self.session.with_authenticated_provider(operation)
    }
}

/// Derive the persistent pre-authentication execution plan for one local conversation.
pub fn derive_remote_mirror_execution_plan(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Result<RemoteMirrorExecutionPlan, String> {
    let selected = derive_selected_remote_mirror_plan(events, local_conversation_id)?;
    Ok(match selected {
        SelectedRemoteMirrorPlan::Unselected => RemoteMirrorExecutionPlan::Unselected,
        SelectedRemoteMirrorPlan::SelectedButBlocked(blocked) => {
            RemoteMirrorExecutionPlan::ProtocolBlocked(blocked)
        }
        SelectedRemoteMirrorPlan::SelectedAndReady(ready) => {
            RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready)
        }
    })
}

/// Combine a persistent execution plan with positive live authenticated authority.
///
/// The caller must acquire the lease separately from a runtime provider. Therefore
/// authentication cannot make an unselected or protocol-blocked conversation executable.
pub fn authorize_remote_mirror_read<'a, P: UserAuthenticatedSessionProvider + ?Sized>(
    plan: RemoteMirrorExecutionPlan,
    session: AuthenticatedSessionLease<'a, P>,
) -> Result<AuthorizedRemoteMirrorRead<'a, P>, RemoteMirrorAuthorizationError> {
    match plan {
        RemoteMirrorExecutionPlan::Unselected => Err(RemoteMirrorAuthorizationError::Unselected),
        RemoteMirrorExecutionPlan::ProtocolBlocked(blocked) => {
            Err(RemoteMirrorAuthorizationError::ProtocolBlocked(blocked))
        }
        RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready) => {
            Ok(AuthorizedRemoteMirrorRead { ready, session })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_identity_audit::record_remote_conversation_bound;
    use crate::remote_mirror_selection_audit::record_remote_mirror_selection_changed;
    use crate::remote_read_audit::record_remote_read_observation;
    use crate::{EventStore, MemoryEventStore};
    use chatarium_core::RemoteReadObservationId;
    use chatarium_core::authenticated_session::{
        AuthenticatedSessionLease, SessionAuthenticationEvidence, UserAuthenticatedSessionProvider,
    };
    use chatarium_core::remote::{
        ProtocolObservationRevision, RemoteConversationBinding, RemoteConversationId,
    };
    use chatarium_protocol::read::{JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation};

    struct FakeProvider {
        evidence: SessionAuthenticationEvidence,
        uses: usize,
    }

    impl UserAuthenticatedSessionProvider for FakeProvider {
        type Error = &'static str;

        fn authentication_evidence(
            &mut self,
        ) -> Result<SessionAuthenticationEvidence, Self::Error> {
            Ok(self.evidence)
        }
    }

    fn synthetic_ready(local: LocalConversationId) -> RemoteMirrorReady {
        RemoteMirrorReady {
            local_conversation_id: local,
            remote_conversation_id: RemoteConversationId::new("opaque-remote").unwrap(),
            protocol_revision: ProtocolObservationRevision::new("rev-a").unwrap(),
            read_observation_id: RemoteReadObservationId::new(),
            binding_sequence: 1,
            read_observation_sequence: 2,
        }
    }

    fn production_c02(revision: &str) -> ReadObservation {
        ReadObservation::new(
            revision,
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/observed",
            vec![],
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap()
    }

    #[test]
    fn unselected_plan_cannot_authorize() {
        let mut provider = FakeProvider {
            evidence: SessionAuthenticationEvidence::Authenticated,
            uses: 0,
        };
        let lease = AuthenticatedSessionLease::acquire(&mut provider).unwrap();

        assert!(matches!(
            authorize_remote_mirror_read(RemoteMirrorExecutionPlan::Unselected, lease),
            Err(RemoteMirrorAuthorizationError::Unselected)
        ));
    }

    #[test]
    fn protocol_blocked_plan_cannot_authorize() {
        let local = LocalConversationId::new();
        let blocked = RemoteMirrorBlocked::MissingConversationFetchObservation {
            local_conversation_id: local,
            binding_sequence: 1,
        };
        let mut provider = FakeProvider {
            evidence: SessionAuthenticationEvidence::Authenticated,
            uses: 0,
        };
        let lease = AuthenticatedSessionLease::acquire(&mut provider).unwrap();

        assert!(matches!(
            authorize_remote_mirror_read(
                RemoteMirrorExecutionPlan::ProtocolBlocked(blocked),
                lease
            ),
            Err(RemoteMirrorAuthorizationError::ProtocolBlocked(_))
        ));
    }

    #[test]
    fn ready_plan_plus_live_authenticated_lease_authorizes() {
        let local = LocalConversationId::new();
        let ready = synthetic_ready(local);
        let mut provider = FakeProvider {
            evidence: SessionAuthenticationEvidence::Authenticated,
            uses: 0,
        };
        let lease = AuthenticatedSessionLease::acquire(&mut provider).unwrap();

        {
            let mut authorized = authorize_remote_mirror_read(
                RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready.clone()),
                lease,
            )
            .unwrap();

            assert_eq!(authorized.ready(), &ready);
            authorized
                .with_authenticated_session_provider(|session| session.uses += 1)
                .unwrap();
        }

        assert_eq!(provider.uses, 1);
    }

    #[test]
    fn current_production_no_baseline_remains_protocol_blocked() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let revision = "future-c02-observation";
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new("opaque-remote").unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        record_remote_read_observation(
            &mut store,
            RemoteReadObservationId::new(),
            &production_c02(revision),
        )
        .unwrap();

        assert!(matches!(
            derive_remote_mirror_execution_plan(store.events(), local).unwrap(),
            RemoteMirrorExecutionPlan::ProtocolBlocked(RemoteMirrorBlocked::NoBaseline { .. })
        ));
    }

    #[test]
    fn current_validated_c02_reaches_authenticated_session_gate() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let revision = "2026-10-01.001";
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new("opaque-remote").unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        record_remote_read_observation(
            &mut store,
            RemoteReadObservationId::new(),
            &production_c02(revision),
        )
        .unwrap();

        assert!(matches!(
            derive_remote_mirror_execution_plan(store.events(), local).unwrap(),
            RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(_)
        ));
    }

    #[test]
    fn execution_plan_derivation_is_read_only() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let before = store.events().to_vec();

        assert_eq!(
            derive_remote_mirror_execution_plan(store.events(), local).unwrap(),
            RemoteMirrorExecutionPlan::Unselected
        );
        assert_eq!(store.events(), before.as_slice());
    }
}
