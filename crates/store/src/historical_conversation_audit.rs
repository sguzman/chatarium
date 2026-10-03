//! Durable historical conversation snapshots imported from ChatGPT account exports.
//!
//! Account-export evidence is archival, not live protocol authority. A preserved remote
//! conversation identifier lets later synchronization correlate the same conversation, but this
//! module deliberately does not create a live remote binding or imply read/write capability.

use crate::{EventEnvelope, EventStore};
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const HISTORICAL_SNAPSHOT_SCHEMA: &str = "chatarium-historical-conversation-snapshot";
const HISTORICAL_SNAPSHOT_VERSION: u64 = 1;

/// Metadata for one content-addressed historical conversation snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalConversationSnapshot {
    /// Stable Chatarium-local identity for this imported remote conversation lineage.
    pub local_conversation_id: LocalConversationId,
    /// Exact conversation identity present in the account export.
    pub remote_conversation_id: String,
    /// SHA-256 of the exact source JSON file.
    pub source_sha256: String,
    /// Relative local archive path of the exact source JSON file.
    pub source_archive: String,
    /// SHA-256 of the canonicalized raw conversation object.
    pub conversation_sha256: String,
    /// Relative local archive path of the preserved raw conversation object.
    pub conversation_archive: String,
    /// Zero-based position of the conversation in its source JSON array.
    pub source_index: u64,
    /// Exported title when present.
    pub title: Option<String>,
    /// Exported creation timestamp when present.
    pub create_time: Option<f64>,
    /// Exported update timestamp when present.
    pub update_time: Option<f64>,
    /// Exported current-node identity when present.
    pub current_node: Option<String>,
}

/// Restart-replayable historical snapshot plus its durable sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalConversationSnapshotAuditRecord {
    /// Snapshot metadata and archive provenance.
    pub snapshot: HistoricalConversationSnapshot,
    /// Durable journal sequence where this snapshot was recorded.
    pub imported_sequence: u64,
}

/// Append one historical conversation snapshot.
///
/// This event does not create a live remote binding. The remote identifier remains archival
/// provenance until a separately evidenced live compatibility path binds it.
pub fn record_historical_conversation_snapshot(
    store: &mut impl EventStore,
    snapshot: &HistoricalConversationSnapshot,
) -> std::io::Result<u64> {
    let payload = serde_json::to_string(&json!({
        "schema": HISTORICAL_SNAPSHOT_SCHEMA,
        "version": HISTORICAL_SNAPSHOT_VERSION,
        "record": "historical_conversation_snapshot_imported",
        "local_conversation_id": snapshot.local_conversation_id.to_string(),
        "remote_conversation_id": snapshot.remote_conversation_id,
        "source_sha256": snapshot.source_sha256,
        "source_archive": snapshot.source_archive,
        "conversation_sha256": snapshot.conversation_sha256,
        "conversation_archive": snapshot.conversation_archive,
        "source_index": snapshot.source_index,
        "title": snapshot.title,
        "create_time": snapshot.create_time,
        "update_time": snapshot.update_time,
        "current_node": snapshot.current_node,
    }))
    .map_err(invalid_data)?;

    store.append_scoped(
        Some(historical_conversation_scope(snapshot.local_conversation_id)),
        EventKind::HistoricalConversationSnapshotImported,
        payload,
    )
}

/// Replay historical snapshots and enforce stable local/remote lineage identity.
///
/// Multiple snapshots of the same remote conversation are valid when their content changes, but
/// every snapshot of that remote identity must retain the same local conversation identity.
pub fn replay_historical_conversation_snapshots(
    events: &[EventEnvelope],
) -> Result<Vec<HistoricalConversationSnapshotAuditRecord>, String> {
    let mut remote_owner = BTreeMap::<String, LocalConversationId>::new();
    let mut local_remote = BTreeMap::<LocalConversationId, String>::new();
    let mut records = Vec::new();

    for event in events {
        if event.kind != EventKind::HistoricalConversationSnapshotImported {
            continue;
        }

        let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
            format!(
                "malformed historical conversation snapshot at sequence {}: {error}",
                event.sequence
            )
        })?;
        if value.get("schema").and_then(Value::as_str) != Some(HISTORICAL_SNAPSHOT_SCHEMA) {
            return Err(format!(
                "historical conversation snapshot at sequence {} has missing/unsupported schema",
                event.sequence
            ));
        }
        let version = required_u64(&value, "version")?;
        if version != HISTORICAL_SNAPSHOT_VERSION {
            return Err(format!(
                "unsupported historical conversation snapshot version {version} at sequence {}",
                event.sequence
            ));
        }
        if required_string(&value, "record")? != "historical_conversation_snapshot_imported" {
            return Err(format!(
                "historical conversation snapshot at sequence {} has wrong record kind",
                event.sequence
            ));
        }

        let local_conversation_id = required_string(&value, "local_conversation_id")?
            .parse::<LocalConversationId>()
            .map_err(|error| {
                format!(
                    "invalid historical local_conversation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        let expected_scope = historical_conversation_scope(local_conversation_id);
        if event.scope.as_deref() != Some(expected_scope.as_str()) {
            return Err(format!(
                "historical conversation snapshot at sequence {} has scope {:?}, expected {:?}",
                event.sequence, event.scope, expected_scope
            ));
        }

        let remote_conversation_id = required_non_empty_string(&value, "remote_conversation_id")?;
        let source_sha256 = required_non_empty_string(&value, "source_sha256")?;
        let source_archive = required_non_empty_string(&value, "source_archive")?;
        let conversation_sha256 = required_non_empty_string(&value, "conversation_sha256")?;
        let conversation_archive = required_non_empty_string(&value, "conversation_archive")?;
        let source_index = required_u64(&value, "source_index")?;

        if let Some(existing) = remote_owner.get(&remote_conversation_id) {
            if *existing != local_conversation_id {
                return Err(format!(
                    "historical remote conversation {:?} changed local owner from {} to {} at sequence {}",
                    remote_conversation_id, existing, local_conversation_id, event.sequence
                ));
            }
        }
        if let Some(existing) = local_remote.get(&local_conversation_id) {
            if existing != &remote_conversation_id {
                return Err(format!(
                    "historical local conversation {} changed remote identity from {:?} to {:?} at sequence {}",
                    local_conversation_id, existing, remote_conversation_id, event.sequence
                ));
            }
        }

        remote_owner.insert(remote_conversation_id.clone(), local_conversation_id);
        local_remote.insert(local_conversation_id, remote_conversation_id.clone());

        records.push(HistoricalConversationSnapshotAuditRecord {
            snapshot: HistoricalConversationSnapshot {
                local_conversation_id,
                remote_conversation_id,
                source_sha256,
                source_archive,
                conversation_sha256,
                conversation_archive,
                source_index,
                title: optional_string(&value, "title")?,
                create_time: optional_number(&value, "create_time")?,
                update_time: optional_number(&value, "update_time")?,
                current_node: optional_string(&value, "current_node")?,
            },
            imported_sequence: event.sequence,
        });
    }

    Ok(records)
}

/// Stable local-only scope for one historical conversation lineage.
#[must_use]
pub fn historical_conversation_scope(local_conversation_id: LocalConversationId) -> String {
    format!("historical-conversation:{local_conversation_id}")
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("historical conversation snapshot is missing string field '{field}'"))
}

fn required_non_empty_string(value: &Value, field: &str) -> Result<String, String> {
    let result = required_string(value, field)?;
    if result.is_empty() {
        return Err(format!(
            "historical conversation snapshot field '{field}' must not be empty"
        ));
    }
    Ok(result)
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("historical conversation snapshot is missing integer field '{field}'"))
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "historical conversation snapshot field '{field}' must be string or null"
        )),
    }
}

fn optional_number(value: &Value, field: &str) -> Result<Option<f64>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_f64().map(Some).ok_or_else(|| {
            format!("historical conversation snapshot field '{field}' must be number or null")
        }),
    }
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;

    fn snapshot(local: LocalConversationId, remote: &str, digest: &str) -> HistoricalConversationSnapshot {
        HistoricalConversationSnapshot {
            local_conversation_id: local,
            remote_conversation_id: remote.to_owned(),
            source_sha256: "source".to_owned(),
            source_archive: "imports/openai-account-export/sources/source.json".to_owned(),
            conversation_sha256: digest.to_owned(),
            conversation_archive: format!("imports/openai-account-export/conversations/{digest}.json"),
            source_index: 0,
            title: Some("title".to_owned()),
            create_time: Some(1.0),
            update_time: Some(2.0),
            current_node: Some("leaf".to_owned()),
        }
    }

    #[test]
    fn replay_allows_changed_snapshots_under_same_identity_lineage() {
        let local = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        record_historical_conversation_snapshot(&mut store, &snapshot(local, "remote-a", "one"))
            .unwrap();
        record_historical_conversation_snapshot(&mut store, &snapshot(local, "remote-a", "two"))
            .unwrap();

        let records = replay_historical_conversation_snapshots(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].snapshot.local_conversation_id, local);
        assert_eq!(records[1].snapshot.local_conversation_id, local);
        assert_eq!(records[1].snapshot.remote_conversation_id, "remote-a");
    }

    #[test]
    fn replay_rejects_remote_identity_rebound_to_another_local_conversation() {
        let mut store = MemoryEventStore::default();
        record_historical_conversation_snapshot(
            &mut store,
            &snapshot(LocalConversationId::new(), "remote-a", "one"),
        )
        .unwrap();
        record_historical_conversation_snapshot(
            &mut store,
            &snapshot(LocalConversationId::new(), "remote-a", "two"),
        )
        .unwrap();

        let error = replay_historical_conversation_snapshots(store.events()).unwrap_err();
        assert!(error.contains("changed local owner"));
    }
}
