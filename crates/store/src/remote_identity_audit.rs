//! Durable local-to-remote conversation identity provenance.
//!
//! Remote conversation identifiers remain opaque exact strings. This audit owns
//! only one-to-one local/remote correlation and the empirical protocol revision
//! that justified it; it does not imply connectivity or synchronization.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::LocalConversationId;
use chatarium_core::remote::{
    ProtocolObservationRevision, RemoteConversationBinding, RemoteConversationId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const REMOTE_IDENTITY_SCHEMA: &str = "chatarium-remote-conversation-binding";
const REMOTE_IDENTITY_VERSION: u64 = 1;

/// Restart-replayable local/remote conversation correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConversationBindingAuditRecord {
    /// Typed local/remote binding and its protocol-observation provenance.
    pub binding: RemoteConversationBinding,
    /// Durable sequence where the binding was recorded.
    pub bound_sequence: u64,
}

/// Append one exact local-to-remote conversation binding.
pub fn record_remote_conversation_bound(
    store: &mut impl EventStore,
    binding: &RemoteConversationBinding,
) -> std::io::Result<u64> {
    let payload = serde_json::to_string(&json!({
        "schema": REMOTE_IDENTITY_SCHEMA,
        "version": REMOTE_IDENTITY_VERSION,
        "record": "remote_conversation_bound",
        "local_conversation_id": binding.local_conversation_id().to_string(),
        "remote_conversation_id": binding.remote_conversation_id().as_str(),
        "protocol_revision": binding.protocol_revision().as_str(),
    }))
    .map_err(invalid_data)?;

    store.append_scoped(
        Some(remote_conversation_binding_scope(
            binding.local_conversation_id(),
        )),
        EventKind::RemoteConversationBound,
        payload,
    )
}

/// Replay strict one-to-one local/remote conversation identity bindings.
pub fn replay_remote_identity_audit(
    events: &[EventEnvelope],
) -> Result<Vec<RemoteConversationBindingAuditRecord>, String> {
    let mut by_local = BTreeMap::<LocalConversationId, RemoteConversationBindingAuditRecord>::new();
    let mut remote_owner = BTreeMap::<RemoteConversationId, LocalConversationId>::new();

    for event in events {
        if event.kind != EventKind::RemoteConversationBound {
            continue;
        }

        let payload = typed_payload(event)?;
        let local = required_string(&payload, "local_conversation_id")?
            .parse::<LocalConversationId>()
            .map_err(|error| {
                format!(
                    "invalid local_conversation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        validate_scope(event, local)?;

        let remote =
            RemoteConversationId::new(required_string(&payload, "remote_conversation_id")?)
                .map_err(|error| {
                    format!(
                        "invalid remote_conversation_id at sequence {}: {error}",
                        event.sequence
                    )
                })?;
        let revision =
            ProtocolObservationRevision::new(required_string(&payload, "protocol_revision")?)
                .map_err(|error| {
                    format!(
                        "invalid protocol_revision at sequence {}: {error}",
                        event.sequence
                    )
                })?;

        if let Some(existing) = by_local.get(&local) {
            return Err(format!(
                "local conversation {} is already bound to remote conversation {:?} at sequence {}; cannot bind {:?} at sequence {}",
                local,
                existing.binding.remote_conversation_id().as_str(),
                existing.bound_sequence,
                remote.as_str(),
                event.sequence
            ));
        }

        if let Some(existing_local) = remote_owner.get(&remote) {
            return Err(format!(
                "remote conversation identity is already bound to local conversation {}; cannot also bind local conversation {} at sequence {}",
                existing_local, local, event.sequence
            ));
        }

        let binding = RemoteConversationBinding::new(local, remote.clone(), revision);
        by_local.insert(
            local,
            RemoteConversationBindingAuditRecord {
                binding,
                bound_sequence: event.sequence,
            },
        );
        remote_owner.insert(remote, local);
    }

    let mut records = by_local.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.bound_sequence);
    Ok(records)
}

/// Stable local-only scope for one remote conversation binding.
///
/// Raw remote identifiers intentionally never appear in the scope.
#[must_use]
pub fn remote_conversation_binding_scope(local: LocalConversationId) -> String {
    format!("remote-conversation-binding:{local}")
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed remote conversation binding payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(REMOTE_IDENTITY_SCHEMA) {
        return Err(format!(
            "remote conversation binding at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "remote conversation binding at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != REMOTE_IDENTITY_VERSION {
        return Err(format!(
            "unsupported remote conversation binding version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "remote_conversation_bound" {
        return Err(format!(
            "remote conversation binding at sequence {} has wrong record kind",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(event: &EventEnvelope, local: LocalConversationId) -> Result<(), String> {
    let expected = remote_conversation_binding_scope(local);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "remote conversation binding at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed remote conversation binding is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::{JsonlEventStore, MemoryEventStore};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn binding(
        local: LocalConversationId,
        remote: &str,
        revision: &str,
    ) -> RemoteConversationBinding {
        RemoteConversationBinding::new(
            local,
            RemoteConversationId::new(remote).unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        )
    }

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-identity-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn binding_survives_real_journal_reopen() {
        let path = temp_path("reopen", "jsonl");
        let local = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_remote_conversation_bound(
                &mut store,
                &binding(local, "opaque/remote:id", "2026-09-29.002"),
            )
            .unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let record = replay_remote_identity_audit(reopened.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.binding.local_conversation_id(), local);
        assert_eq!(
            record.binding.remote_conversation_id().as_str(),
            "opaque/remote:id"
        );
        assert_eq!(
            record.binding.protocol_revision().as_str(),
            "2026-09-29.002"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn duplicate_exact_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let binding = binding(local, "remote-a", "rev-a");
        record_remote_conversation_bound(&mut store, &binding).unwrap();
        record_remote_conversation_bound(&mut store, &binding).unwrap();

        let error = replay_remote_identity_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound"));
    }

    #[test]
    fn one_local_cannot_bind_two_remote_identities() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(local, "remote-a", "rev-a")).unwrap();
        record_remote_conversation_bound(&mut store, &binding(local, "remote-b", "rev-b")).unwrap();

        let error = replay_remote_identity_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound"));
    }

    #[test]
    fn one_remote_identity_cannot_bind_two_local_conversations() {
        let mut store = MemoryEventStore::default();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(first, "remote-a", "rev-a")).unwrap();
        record_remote_conversation_bound(&mut store, &binding(second, "remote-a", "rev-b"))
            .unwrap();

        let error = replay_remote_identity_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to local conversation"));
    }

    #[test]
    fn malformed_typed_payload_is_rejected() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        store
            .append_scoped(
                Some(remote_conversation_binding_scope(local)),
                EventKind::RemoteConversationBound,
                json!({
                    "schema": REMOTE_IDENTITY_SCHEMA,
                    "version": REMOTE_IDENTITY_VERSION,
                    "record": "remote_conversation_bound",
                    "local_conversation_id": "not-a-uuid",
                    "remote_conversation_id": "remote-a",
                    "protocol_revision": "rev-a",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_remote_identity_audit(store.events()).unwrap_err();
        assert!(error.contains("local_conversation_id"));
    }

    #[test]
    fn empty_remote_identity_and_revision_are_rejected_on_replay() {
        for (remote, revision, expected) in [
            ("", "rev-a", "remote_conversation_id"),
            ("remote-a", "", "protocol_revision"),
        ] {
            let mut store = MemoryEventStore::default();
            let local = LocalConversationId::new();
            store
                .append_scoped(
                    Some(remote_conversation_binding_scope(local)),
                    EventKind::RemoteConversationBound,
                    json!({
                        "schema": REMOTE_IDENTITY_SCHEMA,
                        "version": REMOTE_IDENTITY_VERSION,
                        "record": "remote_conversation_bound",
                        "local_conversation_id": local.to_string(),
                        "remote_conversation_id": remote,
                        "protocol_revision": revision,
                    })
                    .to_string(),
                )
                .unwrap();

            let error = replay_remote_identity_audit(store.events()).unwrap_err();
            assert!(error.contains(expected));
        }
    }

    #[test]
    fn scope_mismatch_is_rejected_without_remote_id_in_scope() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let remote = "PRIVATE-REMOTE-IDENTITY";
        store
            .append_scoped(
                Some("remote-conversation-binding:wrong".to_owned()),
                EventKind::RemoteConversationBound,
                json!({
                    "schema": REMOTE_IDENTITY_SCHEMA,
                    "version": REMOTE_IDENTITY_VERSION,
                    "record": "remote_conversation_bound",
                    "local_conversation_id": local.to_string(),
                    "remote_conversation_id": remote,
                    "protocol_revision": "rev-a",
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_remote_identity_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
        assert!(!remote_conversation_binding_scope(local).contains(remote));
    }

    #[test]
    fn torn_tail_cannot_fabricate_binding() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let _store = JsonlEventStore::open(&path).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":1,"kind":"remote_conversation_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(
            replay_remote_identity_audit(reopened.events())
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "draft".to_owned())
            .unwrap();
        assert!(
            replay_remote_identity_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn generic_sqlite_projection_carries_binding_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        record_remote_conversation_bound(
            &mut store,
            &binding(LocalConversationId::new(), "remote-a", "rev-a"),
        )
        .unwrap();

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();
        assert_eq!(
            projection
                .events_of_kind(EventKind::RemoteConversationBound)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }
}
