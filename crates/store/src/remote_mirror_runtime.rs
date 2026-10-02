//! Runtime composition for one authenticated, read-only C02 mirror fetch.
//!
//! This module deliberately stops short of defining HTTP, browser, cookie, token,
//! header, or query-value behavior. A runtime provider owns those details and
//! receives only the exact durable remote identity plus the validated protocol
//! revision. Durable import independently rechecks journal-backed readiness.

use crate::EventStore;
use crate::remote_mirror_execution::{
    RemoteMirrorAuthorizationError, RemoteMirrorExecutionPlan, authorize_remote_mirror_read,
    derive_remote_mirror_execution_plan,
};
use crate::remote_mirror_readiness::RemoteMirrorBlocked;
use crate::remote_mirror_snapshot_audit::{
    RemoteConversationSnapshotImportError, RemoteConversationSnapshotImportResult,
    import_validated_remote_conversation_snapshot,
};
use chatarium_core::LocalConversationId;
use chatarium_core::authenticated_session::{
    AuthenticatedSessionLease, SessionLeaseError, UserAuthenticatedSessionProvider,
};
use chatarium_core::remote::{ProtocolObservationRevision, RemoteConversationId};
use serde_json::Value;

/// Mechanism-agnostic runtime provider for one remote conversation read.
///
/// Implementations may use the user's own official authenticated session, but
/// this contract does not define or expose reusable authentication material.
/// It also does not prescribe endpoint query values or any concrete transport.
/// The provider may fetch only the remote identity supplied by the durable
/// readiness plan.
pub trait RemoteConversationFetchProvider: UserAuthenticatedSessionProvider {
    /// Provider-specific failure from the read operation itself.
    type FetchError;

    /// Fetch one private conversation body for the exact validated semantic flow.
    fn fetch_conversation(
        &mut self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<Value, Self::FetchError>;
}

/// Distinct fail-closed outcomes for authenticated fetch plus durable import.
#[derive(Debug)]
pub enum RemoteMirrorFetchImportError<AuthError, FetchError> {
    /// Durable journal history could not be replayed consistently.
    InvalidJournal(String),
    /// The user has not selected this conversation for mirroring.
    Unselected,
    /// Durable protocol/evidence prerequisites do not permit a fetch.
    ProtocolBlocked(RemoteMirrorBlocked),
    /// Authentication acquisition or immediate pre-use revalidation failed.
    Session(SessionLeaseError<AuthError>),
    /// A ready plan unexpectedly failed the authorization composition boundary.
    Authorization(RemoteMirrorAuthorizationError),
    /// The provider failed while performing the read.
    Provider(FetchError),
    /// The returned body failed the independent durable import boundary.
    Import(RemoteConversationSnapshotImportError),
}

/// Fetch one selected/ready remote conversation through transient authenticated
/// authority, then import the returned body into durable local mirror state.
///
/// Unselected or protocol-blocked state returns before the provider is probed.
/// The authenticated lease revalidates immediately before provider use. No
/// failure path retries automatically, and no provider failure appends a mirror
/// snapshot.
pub fn fetch_and_import_selected_remote_conversation<S, P>(
    store: &mut S,
    local_conversation_id: LocalConversationId,
    provider: &mut P,
) -> Result<
    RemoteConversationSnapshotImportResult,
    RemoteMirrorFetchImportError<P::Error, P::FetchError>,
>
where
    S: EventStore,
    P: RemoteConversationFetchProvider,
{
    let plan = derive_remote_mirror_execution_plan(store.events(), local_conversation_id)
        .map_err(RemoteMirrorFetchImportError::InvalidJournal)?;

    let ready = match plan {
        RemoteMirrorExecutionPlan::Unselected => {
            return Err(RemoteMirrorFetchImportError::Unselected);
        }
        RemoteMirrorExecutionPlan::ProtocolBlocked(blocked) => {
            return Err(RemoteMirrorFetchImportError::ProtocolBlocked(blocked));
        }
        RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready) => ready,
    };

    let lease = AuthenticatedSessionLease::acquire(provider)
        .map_err(RemoteMirrorFetchImportError::Session)?;
    let mut authorized = authorize_remote_mirror_read(
        RemoteMirrorExecutionPlan::RequiresAuthenticatedSession(ready),
        lease,
    )
    .map_err(RemoteMirrorFetchImportError::Authorization)?;

    let remote_conversation_id = authorized.ready().remote_conversation_id.clone();
    let protocol_revision = authorized.ready().protocol_revision.clone();
    let fetched = authorized
        .with_authenticated_session_provider(|provider| {
            provider.fetch_conversation(&remote_conversation_id, &protocol_revision)
        })
        .map_err(RemoteMirrorFetchImportError::Session)?;
    let body = fetched.map_err(RemoteMirrorFetchImportError::Provider)?;

    import_validated_remote_conversation_snapshot(store, local_conversation_id, &body)
        .map_err(RemoteMirrorFetchImportError::Import)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_identity_audit::record_remote_conversation_bound;
    use crate::remote_mirror_selection_audit::record_remote_mirror_selection_changed;
    use crate::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
    use crate::remote_read_audit::record_remote_read_observation;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::RemoteReadObservationId;
    use chatarium_core::authenticated_session::SessionAuthenticationEvidence;
    use chatarium_core::remote::RemoteConversationBinding;
    use chatarium_protocol::conversation_fetch::ConversationFetchParseError;
    use chatarium_protocol::read::{JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const REVISION: &str = "2026-10-01.001";
    const REMOTE: &str = "fixture-id-1";

    struct FakeProvider {
        evidence: VecDeque<Result<SessionAuthenticationEvidence, &'static str>>,
        last_evidence: Result<SessionAuthenticationEvidence, &'static str>,
        body: Result<Value, &'static str>,
        authentication_probes: usize,
        fetches: usize,
        observed_remote_ids: Vec<String>,
        observed_revisions: Vec<String>,
    }

    impl FakeProvider {
        fn new(
            evidence: impl IntoIterator<Item = SessionAuthenticationEvidence>,
            body: Result<Value, &'static str>,
        ) -> Self {
            let evidence = evidence.into_iter().map(Ok).collect::<VecDeque<_>>();
            Self {
                evidence,
                last_evidence: Ok(SessionAuthenticationEvidence::Unknown),
                body,
                authentication_probes: 0,
                fetches: 0,
                observed_remote_ids: Vec::new(),
                observed_revisions: Vec::new(),
            }
        }
    }

    impl UserAuthenticatedSessionProvider for FakeProvider {
        type Error = &'static str;

        fn authentication_evidence(
            &mut self,
        ) -> Result<SessionAuthenticationEvidence, Self::Error> {
            self.authentication_probes += 1;
            if let Some(next) = self.evidence.pop_front() {
                self.last_evidence = next;
            }
            self.last_evidence
        }
    }

    impl RemoteConversationFetchProvider for FakeProvider {
        type FetchError = &'static str;

        fn fetch_conversation(
            &mut self,
            remote_conversation_id: &RemoteConversationId,
            protocol_revision: &ProtocolObservationRevision,
        ) -> Result<Value, Self::FetchError> {
            self.fetches += 1;
            self.observed_remote_ids
                .push(remote_conversation_id.as_str().to_owned());
            self.observed_revisions
                .push(protocol_revision.as_str().to_owned());
            self.body.clone()
        }
    }

    fn materialized_fixture() -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../protocol/fixtures/2026-10-01.001/c02-open-conversation.json"
        ))
        .expect("fixture JSON");
        let body = fixture
            .pointer("/read_responses/0/body")
            .expect("fixture body")
            .clone();

        fn materialize(value: Value) -> Value {
            match value {
                Value::Object(map) => Value::Object(
                    map.into_iter()
                        .map(|(key, value)| (key, materialize(value)))
                        .collect(),
                ),
                Value::Array(items) => Value::Array(items.into_iter().map(materialize).collect()),
                Value::String(value) if value == "<empty-string>" => Value::String(String::new()),
                Value::String(value) if value == "<redacted-text>" => {
                    Value::String("fixture-redacted-text".to_owned())
                }
                Value::String(value) if value == "<string>" => {
                    Value::String("fixture-string".to_owned())
                }
                Value::String(value) if value == "<number>" => json!(1.0),
                Value::String(value) if value == "<bool>" => json!(true),
                Value::String(value) if value == "<url>" => {
                    Value::String("https://example.invalid/".to_owned())
                }
                Value::String(value) if value.starts_with("<id:") && value.ends_with('>') => {
                    let id = value.trim_start_matches("<id:").trim_end_matches('>');
                    Value::String(format!("fixture-id-{id}"))
                }
                other => other,
            }
        }

        materialize(body)
    }

    fn ready_store(
        selected: bool,
        revision: &str,
        remote: &str,
    ) -> (MemoryEventStore, LocalConversationId) {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let binding = RemoteConversationBinding::new(
            local,
            RemoteConversationId::new(remote).unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        );
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        if selected {
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        }
        let observation = ReadObservation::new(
            revision,
            ReadExperiment::OpenConversation,
            ReadMethod::Get,
            "/backend-api/conversations/<id>",
            vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
            200,
            "application/json",
            false,
            true,
            Some(JsonTopLevelType::Object),
        )
        .unwrap();
        record_remote_read_observation(&mut store, RemoteReadObservationId::new(), &observation)
            .unwrap();
        (store, local)
    }

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-mirror-runtime-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    #[test]
    fn unselected_state_never_probes_or_calls_provider() {
        let (mut store, local) = ready_store(false, REVISION, REMOTE);
        let mut provider = FakeProvider::new(
            [SessionAuthenticationEvidence::Authenticated],
            Ok(materialized_fixture()),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::Unselected)
        ));
        assert_eq!(provider.authentication_probes, 0);
        assert_eq!(provider.fetches, 0);
    }

    #[test]
    fn protocol_blocked_state_never_probes_or_calls_provider() {
        let (mut store, local) = ready_store(true, "future-c02-observation", REMOTE);
        let mut provider = FakeProvider::new(
            [SessionAuthenticationEvidence::Authenticated],
            Ok(materialized_fixture()),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::ProtocolBlocked(_))
        ));
        assert_eq!(provider.authentication_probes, 0);
        assert_eq!(provider.fetches, 0);
    }

    #[test]
    fn authenticated_fetch_imports_only_the_bound_remote_identity() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ],
            Ok(materialized_fixture()),
        );

        let result =
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider)
                .expect("fetch/import");
        assert!(result.appended);
        assert_eq!(provider.authentication_probes, 2);
        assert_eq!(provider.fetches, 1);
        assert_eq!(provider.observed_remote_ids, vec![REMOTE.to_owned()]);
        assert_eq!(provider.observed_revisions, vec![REVISION.to_owned()]);

        let records = replay_remote_conversation_snapshot_audit(store.events()).expect("replay");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].remote_conversation_id.as_str(), REMOTE);
    }

    #[test]
    fn stale_authentication_before_provider_use_fails_without_fetch() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Unknown,
            ],
            Ok(materialized_fixture()),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::Session(
                SessionLeaseError::Unknown
            ))
        ));
        assert_eq!(provider.authentication_probes, 2);
        assert_eq!(provider.fetches, 0);
        assert!(
            replay_remote_conversation_snapshot_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn provider_failure_is_distinct_and_does_not_import() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ],
            Err("fetch failed"),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::Provider("fetch failed"))
        ));
        assert_eq!(provider.fetches, 1);
        assert!(
            replay_remote_conversation_snapshot_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn identity_mismatch_remains_an_import_failure() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let mut body = materialized_fixture();
        body.as_object_mut()
            .unwrap()
            .insert("conversation_id".to_owned(), json!("different-remote"));
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ],
            Ok(body),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::Import(
                RemoteConversationSnapshotImportError::Parse(
                    ConversationFetchParseError::IdentityMismatch { .. }
                )
            ))
        ));
        assert_eq!(provider.fetches, 1);
    }

    #[test]
    fn malformed_body_remains_an_import_failure() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let mut body = materialized_fixture();
        body.as_object_mut().unwrap().remove("messages");
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ],
            Ok(body),
        );

        assert!(matches!(
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider),
            Err(RemoteMirrorFetchImportError::Import(
                RemoteConversationSnapshotImportError::Parse(
                    ConversationFetchParseError::MissingField(field)
                )
            )) if field == "messages"
        ));
        assert_eq!(provider.fetches, 1);
    }

    #[test]
    fn duplicate_fetch_of_identical_snapshot_is_idempotent() {
        let (mut store, local) = ready_store(true, REVISION, REMOTE);
        let body = materialized_fixture();
        let mut provider = FakeProvider::new(
            [
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ],
            Ok(body),
        );

        let first = fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider)
            .unwrap();
        let second =
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider)
                .unwrap();

        assert!(first.appended);
        assert!(!second.appended);
        assert_eq!(first.sequence, second.sequence);
        assert_eq!(provider.fetches, 2);
        assert_eq!(
            store
                .events()
                .iter()
                .filter(|event| event.kind
                    == chatarium_core::EventKind::RemoteConversationSnapshotImported)
                .count(),
            1
        );
    }

    #[test]
    fn successful_runtime_import_replays_after_real_journal_reopen() {
        let path = temp_path("reopen");
        let body = materialized_fixture();
        let local;

        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            local = LocalConversationId::new();
            let binding = RemoteConversationBinding::new(
                local,
                RemoteConversationId::new(REMOTE).unwrap(),
                ProtocolObservationRevision::new(REVISION).unwrap(),
            );
            record_remote_conversation_bound(&mut store, &binding).unwrap();
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
            let observation = ReadObservation::new(
                REVISION,
                ReadExperiment::OpenConversation,
                ReadMethod::Get,
                "/backend-api/conversations/<id>",
                vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
                200,
                "application/json",
                false,
                true,
                Some(JsonTopLevelType::Object),
            )
            .unwrap();
            record_remote_read_observation(
                &mut store,
                RemoteReadObservationId::new(),
                &observation,
            )
            .unwrap();

            let mut provider = FakeProvider::new(
                [
                    SessionAuthenticationEvidence::Authenticated,
                    SessionAuthenticationEvidence::Authenticated,
                ],
                Ok(body.clone()),
            );
            fetch_and_import_selected_remote_conversation(&mut store, local, &mut provider)
                .unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_remote_conversation_snapshot_audit(reopened.events()).expect("replay");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].local_conversation_id, local);
        assert_eq!(records[0].raw_body, body);

        let _ = fs::remove_file(path);
    }
}
