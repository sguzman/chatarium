//! Ephemeral authenticated-session authority for remote reads.
//!
//! Authentication material is deliberately owned by a runtime provider and never
//! represented by Chatarium's durable domain types. This module does not define
//! cookies, tokens, headers, browser storage, login flows, or endpoint behavior.

/// Mechanism-agnostic evidence about a user's live authenticated session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthenticationEvidence {
    /// The provider has positive runtime evidence that the user session is authenticated.
    Authenticated,
    /// The provider has positive runtime evidence that the session is not authenticated.
    Unauthenticated,
    /// The provider cannot currently establish whether the session is authenticated.
    Unknown,
}

/// Runtime boundary that owns all concrete session/authentication material.
///
/// Implementations are responsible for consulting the user's own session and must
/// not expose reusable credentials through this trait. The core intentionally
/// cannot infer how authentication is represented or verified.
pub trait UserAuthenticatedSessionProvider {
    /// Provider-specific runtime error.
    type Error;

    /// Return current authentication evidence without exposing credential material.
    fn authentication_evidence(
        &mut self,
    ) -> Result<SessionAuthenticationEvidence, Self::Error>;
}

/// Failure to obtain a borrow-scoped authenticated-session lease.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionLeaseError<E> {
    /// The concrete runtime provider failed while checking authentication evidence.
    Provider(E),
    /// The provider positively established that the session is not authenticated.
    Unauthenticated,
    /// The provider could not establish authentication either way.
    Unknown,
}

/// Borrow-scoped authority proving that a provider reported positive authentication.
///
/// The lease deliberately has no durable identifier, credential fields, cloning,
/// copying, or serialization support. Dropping the lease ends this authority.
/// Restart cannot reconstruct it from Chatarium's journal.
pub struct AuthenticatedSessionLease<'a, P: UserAuthenticatedSessionProvider + ?Sized> {
    provider: &'a mut P,
}

impl<'a, P: UserAuthenticatedSessionProvider + ?Sized> AuthenticatedSessionLease<'a, P> {
    /// Acquire a transient lease only from positive runtime authentication evidence.
    pub fn acquire(provider: &'a mut P) -> Result<Self, SessionLeaseError<P::Error>> {
        match provider
            .authentication_evidence()
            .map_err(SessionLeaseError::Provider)?
        {
            SessionAuthenticationEvidence::Authenticated => Ok(Self { provider }),
            SessionAuthenticationEvidence::Unauthenticated => {
                Err(SessionLeaseError::Unauthenticated)
            }
            SessionAuthenticationEvidence::Unknown => Err(SessionLeaseError::Unknown),
        }
    }

    /// Let adapter code use the exact provider covered by this authenticated borrow.
    ///
    /// Chatarium's generic boundary still does not know or copy any concrete
    /// credential representation; provider-specific code remains responsible for
    /// keeping reusable authentication material private.
    pub fn with_provider<R>(&mut self, operation: impl FnOnce(&mut P) -> R) -> R {
        operation(self.provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider {
        evidence: Result<SessionAuthenticationEvidence, &'static str>,
        probe_count: usize,
        private_material: &'static str,
    }

    impl UserAuthenticatedSessionProvider for FakeProvider {
        type Error = &'static str;

        fn authentication_evidence(
            &mut self,
        ) -> Result<SessionAuthenticationEvidence, Self::Error> {
            self.probe_count += 1;
            self.evidence
        }
    }

    #[test]
    fn authenticated_evidence_acquires_borrow_scoped_lease() {
        let mut provider = FakeProvider {
            evidence: Ok(SessionAuthenticationEvidence::Authenticated),
            probe_count: 0,
            private_material: "not-part-of-the-lease",
        };

        {
            let mut lease = AuthenticatedSessionLease::acquire(&mut provider).unwrap();
            let marker = lease.with_provider(|session| session.private_material.len());
            assert_eq!(marker, "not-part-of-the-lease".len());
        }

        assert_eq!(provider.probe_count, 1);
        assert_eq!(provider.private_material, "not-part-of-the-lease");
    }

    #[test]
    fn unauthenticated_evidence_fails_closed() {
        let mut provider = FakeProvider {
            evidence: Ok(SessionAuthenticationEvidence::Unauthenticated),
            probe_count: 0,
            private_material: "private",
        };

        assert_eq!(
            AuthenticatedSessionLease::acquire(&mut provider).err(),
            Some(SessionLeaseError::Unauthenticated)
        );
    }

    #[test]
    fn unknown_evidence_fails_closed() {
        let mut provider = FakeProvider {
            evidence: Ok(SessionAuthenticationEvidence::Unknown),
            probe_count: 0,
            private_material: "private",
        };

        assert_eq!(
            AuthenticatedSessionLease::acquire(&mut provider).err(),
            Some(SessionLeaseError::Unknown)
        );
    }

    #[test]
    fn provider_errors_remain_distinguishable() {
        let mut provider = FakeProvider {
            evidence: Err("session bridge unavailable"),
            probe_count: 0,
            private_material: "private",
        };

        assert_eq!(
            AuthenticatedSessionLease::acquire(&mut provider).err(),
            Some(SessionLeaseError::Provider("session bridge unavailable"))
        );
    }
}
