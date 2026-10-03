//! Crash-recoverable upgrade from an imported historical conversation lineage to a live C02 mirror.
//!
//! A historical account-export identity is only a candidate. This workflow first fetches that exact
//! remote identity through transient authenticated authority and parses the returned body with exact
//! identity correlation. Only then may the local lineage gain a durable live binding, C02 evidence,
//! mirror selection, and remote snapshot.

use crate::EventStore;
use crate::historical_transcript::latest_historical_conversation_catalog;
use crate::remote_identity_audit::{
    record_remote_conversation_bound, replay_remote_identity_audit,
};
use crate::remote_mirror_runtime::RemoteConversationFetchProvider;
use crate::remote_mirror_selection_audit::{
    record_remote_mirror_selection_changed, replay_remote_mirror_selection_audit,
};
use crate::remote_mirror_snapshot_audit::{
    RemoteConversationSnapshotImportError, RemoteConversationSnapshotImportResult,
    import_validated_remote_conversation_snapshot,
};
use crate::remote_read_audit::{
    RemoteReadObservationProvenance, record_remote_read_observation_from_fixture,
    replay_remote_read_audit,
};
use chatarium_core::LocalConversationId;
use chatarium_core::RemoteReadObservationId;
use chatarium_core::authenticated_session::{AuthenticatedSessionLease, SessionLeaseError};
use chatarium_core::remote::{
    ProtocolObservationRevision, RemoteConversationBinding, RemoteConversationId,
};
use chatarium_protocol::Compatibility;
use chatarium_protocol::conversation_fetch::{
    ConversationFetchParseError, parse_conversation_fetch_response,
};
use chatarium_protocol::read::{
    JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation, ReadQueryParameterEvidence,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::fmt;

const LIVE_REVISION: &str = "2026-10-03.001";
const C02_FIXTURE: &[u8] =
    include_bytes!("../../../protocol/fixtures/2026-10-03.001/c02-open-conversation.json");

/// Successful promotion/fetch result for one historical lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalLiveMirrorBootstrapResult {
    /// Stable local lineage preserved from the account export.
    pub local_conversation_id: LocalConversationId,
    /// Exact remote identity verified by the live response.
    pub remote_conversation_id: RemoteConversationId,
    /// Durable remote snapshot append/idempotency result.
    pub snapshot: RemoteConversationSnapshotImportResult,
}

/// Fail-closed outcomes for historical-to-live promotion.
#[derive(Debug)]
pub enum HistoricalLiveMirrorBootstrapError<AuthError, FetchError> {
    InvalidJournal(String),
    HistoricalConversationMissing(LocalConversationId),
    InvalidHistoricalRemoteIdentity,
    ExistingBindingConflict(String),
    AmbiguousReadEvidence,
    Session(SessionLeaseError<AuthError>),
    Provider(FetchError),
    Parse(ConversationFetchParseError),
    Persistence(std::io::Error),
    Snapshot(RemoteConversationSnapshotImportError),
}

impl<AuthError: fmt::Debug, FetchError: fmt::Debug> fmt::Display
    for HistoricalLiveMirrorBootstrapError<AuthError, FetchError>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJournal(detail) => write!(formatter, "invalid durable history: {detail}"),
            Self::HistoricalConversationMissing(local) => {
                write!(formatter, "historical conversation {local} is not present")
            }
            Self::InvalidHistoricalRemoteIdentity => {
                write!(
                    formatter,
                    "historical conversation has an invalid remote identity"
                )
            }
            Self::ExistingBindingConflict(detail) => {
                write!(
                    formatter,
                    "existing remote binding conflicts with live bootstrap: {detail}"
                )
            }
            Self::AmbiguousReadEvidence => {
                write!(
                    formatter,
                    "more than one validated live C02 read observation is durable"
                )
            }
            Self::Session(error) => write!(
                formatter,
                "authenticated browser session unavailable: {error:?}"
            ),
            Self::Provider(error) => write!(formatter, "live conversation fetch failed: {error:?}"),
            Self::Parse(error) => write!(
                formatter,
                "live conversation identity/shape validation failed: {error}"
            ),
            Self::Persistence(error) => write!(formatter, "persist live mirror bootstrap: {error}"),
            Self::Snapshot(error) => {
                write!(formatter, "persist live conversation snapshot: {error}")
            }
        }
    }
}

impl<AuthError: fmt::Debug, FetchError: fmt::Debug> std::error::Error
    for HistoricalLiveMirrorBootstrapError<AuthError, FetchError>
{
}

/// Store-only promotion error used after a live body was fetched off the durability worker.
pub type HistoricalLiveMirrorPromotionError =
    HistoricalLiveMirrorBootstrapError<Infallible, Infallible>;

/// Validate and durably promote one already-fetched C02 body.
///
/// This performs no network I/O. The exact remote identity is rechecked against the durable
/// historical lineage before any live-binding event is appended, so a stale UI selection or
/// compromised bridge result cannot redirect one local lineage to another remote conversation.
pub fn promote_historical_live_mirror_body<S>(
    store: &mut S,
    local_conversation_id: LocalConversationId,
    expected_remote_conversation_id: &str,
    fetched: &Value,
) -> Result<HistoricalLiveMirrorBootstrapResult, HistoricalLiveMirrorPromotionError>
where
    S: EventStore,
{
    promote_historical_live_mirror_body_typed::<S, Infallible, Infallible>(
        store,
        local_conversation_id,
        expected_remote_conversation_id,
        fetched,
    )
}

/// Fetch and promote one imported historical lineage into a live remote mirror.
///
/// No durable live claim is appended until the fetched body parses against the current C02
/// revision and reports the exact historical remote identity.
pub fn bootstrap_historical_live_mirror<S, P>(
    store: &mut S,
    local_conversation_id: LocalConversationId,
    provider: &mut P,
) -> Result<
    HistoricalLiveMirrorBootstrapResult,
    HistoricalLiveMirrorBootstrapError<P::Error, P::FetchError>,
>
where
    S: EventStore,
    P: RemoteConversationFetchProvider,
{
    let catalog = latest_historical_conversation_catalog(store.events())
        .map_err(HistoricalLiveMirrorBootstrapError::InvalidJournal)?;
    let historical = catalog
        .into_iter()
        .find(|entry| entry.local_conversation_id == local_conversation_id)
        .ok_or(
            HistoricalLiveMirrorBootstrapError::HistoricalConversationMissing(
                local_conversation_id,
            ),
        )?;

    let remote_conversation_id = RemoteConversationId::new(historical.remote_conversation_id)
        .map_err(|_| HistoricalLiveMirrorBootstrapError::InvalidHistoricalRemoteIdentity)?;
    let protocol_revision = ProtocolObservationRevision::new(LIVE_REVISION)
        .expect("hard-coded live protocol revision is non-empty");

    let lease = AuthenticatedSessionLease::acquire(provider)
        .map_err(HistoricalLiveMirrorBootstrapError::Session)?;
    let mut lease = lease;
    let fetched = lease
        .with_authenticated_provider(|provider| {
            provider.fetch_conversation(&remote_conversation_id, &protocol_revision)
        })
        .map_err(HistoricalLiveMirrorBootstrapError::Session)?
        .map_err(HistoricalLiveMirrorBootstrapError::Provider)?;

    promote_historical_live_mirror_body_typed::<S, P::Error, P::FetchError>(
        store,
        local_conversation_id,
        remote_conversation_id.as_str(),
        &fetched,
    )
}

fn promote_historical_live_mirror_body_typed<S, AuthError, FetchError>(
    store: &mut S,
    local_conversation_id: LocalConversationId,
    expected_remote_conversation_id: &str,
    fetched: &Value,
) -> Result<
    HistoricalLiveMirrorBootstrapResult,
    HistoricalLiveMirrorBootstrapError<AuthError, FetchError>,
>
where
    S: EventStore,
{
    let catalog = latest_historical_conversation_catalog(store.events())
        .map_err(HistoricalLiveMirrorBootstrapError::InvalidJournal)?;
    let historical = catalog
        .into_iter()
        .find(|entry| entry.local_conversation_id == local_conversation_id)
        .ok_or(
            HistoricalLiveMirrorBootstrapError::HistoricalConversationMissing(
                local_conversation_id,
            ),
        )?;

    if historical.remote_conversation_id != expected_remote_conversation_id {
        return Err(HistoricalLiveMirrorBootstrapError::ExistingBindingConflict(
            "requested remote identity no longer matches the durable historical lineage".to_owned(),
        ));
    }

    let remote_conversation_id = RemoteConversationId::new(historical.remote_conversation_id)
        .map_err(|_| HistoricalLiveMirrorBootstrapError::InvalidHistoricalRemoteIdentity)?;
    let protocol_revision = ProtocolObservationRevision::new(LIVE_REVISION)
        .expect("hard-coded live protocol revision is non-empty");

    // This is intentionally before *any* live-binding append. The historical ID remains only a
    // candidate unless the live response positively echoes the exact same remote identity.
    parse_conversation_fetch_response(
        LIVE_REVISION,
        fetched,
        Some(remote_conversation_id.as_str()),
    )
    .map_err(HistoricalLiveMirrorBootstrapError::Parse)?;

    ensure_binding(
        store,
        local_conversation_id,
        &remote_conversation_id,
        &protocol_revision,
    )?;
    ensure_live_read_evidence(store)?;
    ensure_selected(store, local_conversation_id)?;

    let snapshot =
        import_validated_remote_conversation_snapshot(store, local_conversation_id, fetched)
            .map_err(HistoricalLiveMirrorBootstrapError::Snapshot)?;

    Ok(HistoricalLiveMirrorBootstrapResult {
        local_conversation_id,
        remote_conversation_id,
        snapshot,
    })
}

fn ensure_binding<S, AuthError, FetchError>(
    store: &mut S,
    local: LocalConversationId,
    remote: &RemoteConversationId,
    revision: &ProtocolObservationRevision,
) -> Result<(), HistoricalLiveMirrorBootstrapError<AuthError, FetchError>>
where
    S: EventStore,
{
    let bindings = replay_remote_identity_audit(store.events())
        .map_err(HistoricalLiveMirrorBootstrapError::InvalidJournal)?;

    if let Some(record) = bindings
        .iter()
        .find(|record| record.binding.local_conversation_id() == local)
    {
        if record.binding.remote_conversation_id() != remote
            || record.binding.protocol_revision() != revision
        {
            return Err(HistoricalLiveMirrorBootstrapError::ExistingBindingConflict(
                "local lineage is already bound to a different remote identity or protocol revision"
                    .to_owned(),
            ));
        }
        return Ok(());
    }

    if bindings
        .iter()
        .any(|record| record.binding.remote_conversation_id() == remote)
    {
        return Err(HistoricalLiveMirrorBootstrapError::ExistingBindingConflict(
            "remote identity is already owned by another local lineage".to_owned(),
        ));
    }

    let binding = RemoteConversationBinding::new(local, remote.clone(), revision.clone());
    record_remote_conversation_bound(store, &binding)
        .map(|_| ())
        .map_err(HistoricalLiveMirrorBootstrapError::Persistence)
}

fn ensure_live_read_evidence<S, AuthError, FetchError>(
    store: &mut S,
) -> Result<(), HistoricalLiveMirrorBootstrapError<AuthError, FetchError>>
where
    S: EventStore,
{
    let reads = replay_remote_read_audit(store.events())
        .map_err(HistoricalLiveMirrorBootstrapError::InvalidJournal)?;
    let matching = reads
        .iter()
        .filter(|record| {
            record.observation.experiment() == ReadExperiment::OpenConversation
                && record.observation.protocol_revision() == LIVE_REVISION
                && matches!(
                    &record.compatibility,
                    Compatibility::ValidatedAgainst(revision) if revision == LIVE_REVISION
                )
        })
        .collect::<Vec<_>>();

    match matching.len() {
        1 => return Ok(()),
        count if count > 1 => {
            return Err(HistoricalLiveMirrorBootstrapError::AmbiguousReadEvidence);
        }
        _ => {}
    }

    let observation = ReadObservation::new_with_query_parameters(
        LIVE_REVISION,
        ReadExperiment::OpenConversation,
        ReadMethod::Get,
        "/backend-api/conversations/<id>",
        vec!["include_has_versions".to_owned(), "num_turns".to_owned()],
        Some(vec![
            ReadQueryParameterEvidence::known("num_turns", "10")
                .expect("hard-coded safe C02 query literal"),
            ReadQueryParameterEvidence::known("include_has_versions", "true")
                .expect("hard-coded safe C02 query literal"),
        ]),
        200,
        "application/json",
        false,
        true,
        Some(JsonTopLevelType::Object),
    )
    .expect("hard-coded validated C02 observation is structurally valid");

    let provenance =
        RemoteReadObservationProvenance::sanitized_read_fixture(sha256_hex(C02_FIXTURE), 0)
            .expect("committed fixture SHA-256 is valid");

    record_remote_read_observation_from_fixture(
        store,
        RemoteReadObservationId::new(),
        &observation,
        &provenance,
    )
    .map(|_| ())
    .map_err(HistoricalLiveMirrorBootstrapError::Persistence)
}

fn ensure_selected<S, AuthError, FetchError>(
    store: &mut S,
    local: LocalConversationId,
) -> Result<(), HistoricalLiveMirrorBootstrapError<AuthError, FetchError>>
where
    S: EventStore,
{
    let selections = replay_remote_mirror_selection_audit(store.events())
        .map_err(HistoricalLiveMirrorBootstrapError::InvalidJournal)?;
    if selections
        .iter()
        .find(|record| record.local_conversation_id == local)
        .is_some_and(|record| record.selected)
    {
        return Ok(());
    }

    record_remote_mirror_selection_changed(store, local, true)
        .map(|_| ())
        .map_err(HistoricalLiveMirrorBootstrapError::Persistence)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::historical_conversation_audit::{
        HistoricalConversationSnapshot, record_historical_conversation_snapshot,
    };
    use crate::remote_identity_audit::replay_remote_identity_audit;
    use crate::remote_mirror_selection_audit::replay_remote_mirror_selection_audit;
    use crate::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
    use crate::remote_read_audit::replay_remote_read_audit;
    use chatarium_core::authenticated_session::{
        SessionAuthenticationEvidence, UserAuthenticatedSessionProvider,
    };
    use serde_json::json;
    use std::collections::VecDeque;

    struct FakeProvider {
        evidence: VecDeque<SessionAuthenticationEvidence>,
        body: Result<Value, &'static str>,
        probes: usize,
        fetches: usize,
        remote_ids: Vec<String>,
        revisions: Vec<String>,
    }

    impl FakeProvider {
        fn authenticated(body: Result<Value, &'static str>) -> Self {
            Self {
                evidence: VecDeque::from([
                    SessionAuthenticationEvidence::Authenticated,
                    SessionAuthenticationEvidence::Authenticated,
                ]),
                body,
                probes: 0,
                fetches: 0,
                remote_ids: Vec::new(),
                revisions: Vec::new(),
            }
        }
    }

    impl UserAuthenticatedSessionProvider for FakeProvider {
        type Error = &'static str;

        fn authentication_evidence(
            &mut self,
        ) -> Result<SessionAuthenticationEvidence, Self::Error> {
            self.probes += 1;
            Ok(self
                .evidence
                .pop_front()
                .unwrap_or(SessionAuthenticationEvidence::Authenticated))
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
            self.remote_ids
                .push(remote_conversation_id.as_str().to_owned());
            self.revisions.push(protocol_revision.as_str().to_owned());
            self.body.clone()
        }
    }

    fn materialized_fixture() -> Value {
        let fixture: Value = serde_json::from_slice(C02_FIXTURE).expect("fixture JSON");
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

    fn historical_store() -> (MemoryEventStore, LocalConversationId) {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_historical_conversation_snapshot(
            &mut store,
            &HistoricalConversationSnapshot {
                local_conversation_id: local,
                remote_conversation_id: "fixture-id-7".to_owned(),
                source_sha256: "source".to_owned(),
                source_archive: "imports/source.json".to_owned(),
                conversation_sha256: "conversation".to_owned(),
                conversation_archive: "imports/conversation.json".to_owned(),
                source_index: 0,
                title: Some("Imported".to_owned()),
                create_time: None,
                update_time: None,
                current_node: Some("fixture-id-6".to_owned()),
            },
        )
        .unwrap();
        (store, local)
    }

    #[test]
    fn live_response_identity_is_verified_before_any_binding_is_appended() {
        let (mut store, local) = historical_store();
        let mut body = materialized_fixture();
        body["conversation_id"] = json!("different-remote");
        let mut provider = FakeProvider::authenticated(Ok(body));

        assert!(matches!(
            bootstrap_historical_live_mirror(&mut store, local, &mut provider),
            Err(HistoricalLiveMirrorBootstrapError::Parse(
                ConversationFetchParseError::IdentityMismatch { .. }
            ))
        ));
        assert!(
            replay_remote_identity_audit(store.events())
                .unwrap()
                .is_empty()
        );
        assert!(replay_remote_read_audit(store.events()).unwrap().is_empty());
        assert!(
            replay_remote_mirror_selection_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_historical_lineage_never_probes_provider() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let mut provider = FakeProvider::authenticated(Ok(materialized_fixture()));

        assert!(matches!(
            bootstrap_historical_live_mirror(&mut store, local, &mut provider),
            Err(HistoricalLiveMirrorBootstrapError::HistoricalConversationMissing(id)) if id == local
        ));
        assert_eq!(provider.probes, 0);
        assert_eq!(provider.fetches, 0);
    }

    #[test]
    fn successful_bootstrap_preserves_local_lineage_and_imports_live_snapshot() {
        let (mut store, local) = historical_store();
        let mut provider = FakeProvider::authenticated(Ok(materialized_fixture()));

        let result =
            bootstrap_historical_live_mirror(&mut store, local, &mut provider).expect("bootstrap");
        assert_eq!(result.local_conversation_id, local);
        assert_eq!(result.remote_conversation_id.as_str(), "fixture-id-7");
        assert!(result.snapshot.appended);
        assert_eq!(provider.probes, 2);
        assert_eq!(provider.fetches, 1);
        assert_eq!(provider.remote_ids, vec!["fixture-id-7".to_owned()]);
        assert_eq!(provider.revisions, vec![LIVE_REVISION.to_owned()]);

        let bindings = replay_remote_identity_audit(store.events()).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].binding.local_conversation_id(), local);

        let reads = replay_remote_read_audit(store.events()).unwrap();
        let live_reads = reads
            .iter()
            .filter(|record| record.observation.protocol_revision() == LIVE_REVISION)
            .collect::<Vec<_>>();
        assert_eq!(live_reads.len(), 1);
        assert!(matches!(
            live_reads[0].compatibility,
            Compatibility::ValidatedAgainst(ref revision) if revision == LIVE_REVISION
        ));
        let parameters = live_reads[0]
            .observation
            .query_parameters()
            .expect("safe exact C02 query evidence");
        assert_eq!(
            parameters
                .iter()
                .map(|parameter| (
                    parameter.key(),
                    parameter.literal_evidence().map(|value| value.literal())
                ))
                .collect::<Vec<_>>(),
            vec![
                ("num_turns", Some("10")),
                ("include_has_versions", Some("true")),
            ]
        );

        assert!(
            replay_remote_mirror_selection_audit(store.events())
                .unwrap()
                .iter()
                .any(|record| record.local_conversation_id == local && record.selected)
        );
        let snapshots = replay_remote_conversation_snapshot_audit(store.events()).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].local_conversation_id, local);
        assert_eq!(snapshots[0].remote_conversation_id.as_str(), "fixture-id-7");
    }

    #[test]
    fn already_fetched_body_promotes_without_provider_or_network_authority() {
        let (mut store, local) = historical_store();
        let body = materialized_fixture();

        let result = promote_historical_live_mirror_body(
            &mut store,
            local,
            "fixture-id-7",
            &body,
        )
        .expect("store-only promotion");

        assert_eq!(result.local_conversation_id, local);
        assert_eq!(result.remote_conversation_id.as_str(), "fixture-id-7");
        assert!(result.snapshot.appended);
        assert_eq!(replay_remote_identity_audit(store.events()).unwrap().len(), 1);
        assert_eq!(
            replay_remote_conversation_snapshot_audit(store.events())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn already_fetched_body_rechecks_durable_historical_identity_before_appending() {
        let (mut store, local) = historical_store();
        let before = store.events().len();

        assert!(matches!(
            promote_historical_live_mirror_body(
                &mut store,
                local,
                "stale-ui-remote-id",
                &materialized_fixture(),
            ),
            Err(HistoricalLiveMirrorBootstrapError::ExistingBindingConflict(_))
        ));
        assert_eq!(store.events().len(), before);
        assert!(replay_remote_identity_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn repeated_bootstrap_is_durable_and_snapshot_idempotent() {
        let (mut store, local) = historical_store();
        let body = materialized_fixture();
        let mut provider = FakeProvider {
            evidence: VecDeque::from([
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
                SessionAuthenticationEvidence::Authenticated,
            ]),
            body: Ok(body),
            probes: 0,
            fetches: 0,
            remote_ids: Vec::new(),
            revisions: Vec::new(),
        };

        let first = bootstrap_historical_live_mirror(&mut store, local, &mut provider).unwrap();
        let second = bootstrap_historical_live_mirror(&mut store, local, &mut provider).unwrap();
        assert!(first.snapshot.appended);
        assert!(!second.snapshot.appended);
        assert_eq!(first.snapshot.sequence, second.snapshot.sequence);
        assert_eq!(
            replay_remote_identity_audit(store.events()).unwrap().len(),
            1
        );
        assert_eq!(
            replay_remote_read_audit(store.events())
                .unwrap()
                .iter()
                .filter(|record| record.observation.protocol_revision() == LIVE_REVISION)
                .count(),
            1
        );
    }
}
