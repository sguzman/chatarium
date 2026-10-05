//! Durable, replayable scheduling state for catalog conversation mirroring.
//!
//! Queue events contain only the opaque remote identity, catalog position, and structural
//! outcome. Private conversation bodies remain exclusively in the existing snapshot events.

use crate::EventEnvelope;
use crate::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use crate::remote_mirror_transcript::project_remote_active_transcript;
use chatarium_core::EventKind;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io;

const QUEUE_SCHEMA: &str = "chatarium-remote-mirror-queue";
const QUEUE_VERSION: u64 = 1;

/// Current durable state for one discovered catalog item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteMirrorQueueStatus {
    /// Discovered but not yet attempted.
    Discovered,
    /// Explicitly queued for a capture run.
    Queued,
    /// Capture started but no terminal queue outcome was recorded.
    Capturing,
    /// Snapshot replay proves a complete visible transcript.
    MirroredFully,
    /// Snapshot replay proves a durable but incomplete visible transcript.
    MirroredPartial,
    /// A terminal HTTP 429 stopped this item and the current batch.
    RateLimited,
    /// A non-structural transient attempt failure was recorded.
    TransientFailure,
    /// A structural or permanent failure was recorded.
    StructuralFailure,
}

/// Replayable state for one catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMirrorQueueItem {
    /// Zero-based position in the durable discovered catalog.
    pub catalog_index: usize,
    /// Opaque remote conversation identity.
    pub remote_conversation_id: String,
    /// Current durable queue state.
    pub status: RemoteMirrorQueueStatus,
    /// Latest journal sequence affecting this item.
    pub last_sequence: u64,
}

/// Derived queue view used by status and batch scheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMirrorQueueSummary {
    /// All discovered catalog items represented in this view.
    pub items: Vec<RemoteMirrorQueueItem>,
}

impl RemoteMirrorQueueSummary {
    /// Number of items currently classified as full mirrors.
    #[must_use]
    pub fn full_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == RemoteMirrorQueueStatus::MirroredFully)
            .count()
    }

    /// Number of items currently classified as partial mirrors.
    #[must_use]
    pub fn partial_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == RemoteMirrorQueueStatus::MirroredPartial)
            .count()
    }

    /// Number of items eligible for a later bounded batch.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    RemoteMirrorQueueStatus::Discovered
                        | RemoteMirrorQueueStatus::Queued
                        | RemoteMirrorQueueStatus::Capturing
                        | RemoteMirrorQueueStatus::TransientFailure
                )
            })
            .count()
    }

    /// Select deterministic work without selecting successful or rate-limited items.
    #[must_use]
    pub fn eligible_items(&self, limit: usize) -> Vec<RemoteMirrorQueueItem> {
        self.items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    RemoteMirrorQueueStatus::Discovered
                        | RemoteMirrorQueueStatus::Queued
                        | RemoteMirrorQueueStatus::Capturing
                        | RemoteMirrorQueueStatus::TransientFailure
                )
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Count items in a particular state.
    #[must_use]
    pub fn count(&self, status: RemoteMirrorQueueStatus) -> usize {
        self.items.iter().filter(|item| item.status == status).count()
    }
}

/// Derive queue state from the discovered catalog and the append-only journal.
///
/// Existing snapshot events are authoritative for full/partial mirror state. Queue lifecycle
/// events provide resumable scheduling state for items without inventing account-wide coverage.
pub fn derive_remote_mirror_queue(
    catalog: &[(usize, String)],
    events: &[EventEnvelope],
) -> Result<RemoteMirrorQueueSummary, String> {
    let mut items = catalog
        .iter()
        .map(|(catalog_index, remote_conversation_id)| {
            (
                remote_conversation_id.clone(),
                RemoteMirrorQueueItem {
                    catalog_index: *catalog_index,
                    remote_conversation_id: remote_conversation_id.clone(),
                    status: RemoteMirrorQueueStatus::Discovered,
                    last_sequence: 0,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let snapshots = replay_remote_conversation_snapshot_audit(events)?;
    let snapshots_by_sequence = snapshots
        .iter()
        .map(|record| (record.imported_sequence, record))
        .collect::<BTreeMap<_, _>>();

    for event in events {
        if let Some(snapshot) = snapshots_by_sequence.get(&event.sequence) {
            if let Some(item) = items.get_mut(snapshot.remote_conversation_id.as_str()) {
                item.status = project_remote_active_transcript(&snapshot.envelope)
                    .map(|projection| {
                        if projection.truncated_before {
                            RemoteMirrorQueueStatus::MirroredPartial
                        } else {
                            RemoteMirrorQueueStatus::MirroredFully
                        }
                    })
                    .unwrap_or(RemoteMirrorQueueStatus::MirroredPartial);
                item.last_sequence = event.sequence;
            }
            continue;
        }
        let Some(kind) = queue_event_kind(event.kind) else {
            continue;
        };
        let payload: Value = serde_json::from_str(&event.payload).map_err(|error| {
            format!("invalid remote mirror queue payload at sequence {}: {error}", event.sequence)
        })?;
        let remote_id = payload
            .get("remote_conversation_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "remote mirror queue event at sequence {} is missing remote identity",
                    event.sequence
                )
            })?;
        let Some(item) = items.get_mut(remote_id) else {
            continue;
        };
        item.status = match kind {
            QueueEventKind::Queued => RemoteMirrorQueueStatus::Queued,
            QueueEventKind::Capturing => RemoteMirrorQueueStatus::Capturing,
            QueueEventKind::Completed => match payload
                .get("mirror_state")
                .and_then(Value::as_str)
            {
                Some("partial") => RemoteMirrorQueueStatus::MirroredPartial,
                Some("fully_mirrored") => RemoteMirrorQueueStatus::MirroredFully,
                _ => {
                    return Err(format!(
                        "remote mirror queue completion at sequence {} has invalid mirror state",
                        event.sequence
                    ));
                }
            },
            QueueEventKind::RateLimited => RemoteMirrorQueueStatus::RateLimited,
            QueueEventKind::Failed => match payload.get("failure_class").and_then(Value::as_str) {
                Some("structural") => RemoteMirrorQueueStatus::StructuralFailure,
                Some("transient") => RemoteMirrorQueueStatus::TransientFailure,
                _ => {
                    return Err(format!(
                        "remote mirror queue failure at sequence {} has invalid failure class",
                        event.sequence
                    ));
                }
            },
        };
        item.last_sequence = event.sequence;
    }

    Ok(RemoteMirrorQueueSummary {
        items: items.into_values().collect(),
    })
}

/// Persist that an item entered the current bounded batch.
pub fn record_remote_mirror_queue_item_queued(
    store: &mut impl crate::EventStore,
    remote_conversation_id: &str,
    catalog_index: usize,
) -> io::Result<u64> {
    append_queue_event(
        store,
        EventKind::RemoteMirrorQueueItemQueued,
        remote_conversation_id,
        json!({"catalog_index": catalog_index}),
    )
}

/// Persist that an item began its production capture attempt.
pub fn record_remote_mirror_queue_capture_started(
    store: &mut impl crate::EventStore,
    remote_conversation_id: &str,
) -> io::Result<u64> {
    append_queue_event(
        store,
        EventKind::RemoteMirrorQueueCaptureStarted,
        remote_conversation_id,
        json!({}),
    )
}

/// Persist the structural result of one successful durable promotion and projection.
pub fn record_remote_mirror_queue_completed(
    store: &mut impl crate::EventStore,
    remote_conversation_id: &str,
    mirror_state: &str,
) -> io::Result<u64> {
    append_queue_event(
        store,
        EventKind::RemoteMirrorQueueCompleted,
        remote_conversation_id,
        json!({"mirror_state": mirror_state}),
    )
}

/// Persist a terminal rate-limit outcome for the current batch.
pub fn record_remote_mirror_queue_rate_limited(
    store: &mut impl crate::EventStore,
    remote_conversation_id: &str,
) -> io::Result<u64> {
    append_queue_event(
        store,
        EventKind::RemoteMirrorQueueRateLimited,
        remote_conversation_id,
        json!({"http_status": 429}),
    )
}

/// Persist a non-rate-limit failed attempt.
pub fn record_remote_mirror_queue_failed(
    store: &mut impl crate::EventStore,
    remote_conversation_id: &str,
    failure_class: &str,
) -> io::Result<u64> {
    append_queue_event(
        store,
        EventKind::RemoteMirrorQueueFailed,
        remote_conversation_id,
        json!({"failure_class": failure_class}),
    )
}

#[derive(Debug, Clone, Copy)]
enum QueueEventKind {
    Queued,
    Capturing,
    Completed,
    RateLimited,
    Failed,
}

fn queue_event_kind(kind: EventKind) -> Option<QueueEventKind> {
    Some(match kind {
        EventKind::RemoteMirrorQueueItemQueued => QueueEventKind::Queued,
        EventKind::RemoteMirrorQueueCaptureStarted => QueueEventKind::Capturing,
        EventKind::RemoteMirrorQueueCompleted => QueueEventKind::Completed,
        EventKind::RemoteMirrorQueueRateLimited => QueueEventKind::RateLimited,
        EventKind::RemoteMirrorQueueFailed => QueueEventKind::Failed,
        _ => return None,
    })
}

fn append_queue_event(
    store: &mut impl crate::EventStore,
    kind: EventKind,
    remote_conversation_id: &str,
    details: Value,
) -> io::Result<u64> {
    let mut payload = serde_json::Map::new();
    payload.insert("schema".to_owned(), json!(QUEUE_SCHEMA));
    payload.insert("version".to_owned(), json!(QUEUE_VERSION));
    payload.insert("remote_conversation_id".to_owned(), json!(remote_conversation_id));
    if let Value::Object(details) = details {
        payload.extend(details);
    }
    store.append_scoped(
        Some(format!("remote-mirror-queue:{remote_conversation_id}")),
        kind,
        Value::Object(payload).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, MemoryEventStore};

    fn catalog() -> Vec<(usize, String)> {
        vec![
            (0, "remote-a".to_owned()),
            (1, "remote-b".to_owned()),
            (2, "remote-c".to_owned()),
        ]
    }

    #[test]
    fn empty_catalog_has_no_pending_work() {
        let summary = derive_remote_mirror_queue(&[], &[]).unwrap();
        assert!(summary.items.is_empty());
        assert_eq!(summary.pending_count(), 0);
    }

    #[test]
    fn all_discovered_items_are_pending_deterministically() {
        let summary = derive_remote_mirror_queue(&catalog(), &[]).unwrap();
        assert_eq!(summary.pending_count(), 3);
        assert_eq!(summary.eligible_items(2)[0].catalog_index, 0);
        assert_eq!(summary.eligible_items(2)[1].catalog_index, 1);
    }

    #[test]
    fn successful_partial_and_rate_limited_states_survive_replay() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_completed(&mut store, "remote-a", "fully_mirrored").unwrap();
        record_remote_mirror_queue_completed(&mut store, "remote-b", "partial").unwrap();
        record_remote_mirror_queue_rate_limited(&mut store, "remote-c").unwrap();

        let summary = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();

        assert_eq!(summary.full_count(), 1);
        assert_eq!(summary.partial_count(), 1);
        assert_eq!(summary.count(RemoteMirrorQueueStatus::RateLimited), 1);
        assert_eq!(summary.pending_count(), 0);
    }

    #[test]
    fn transient_failure_remains_resumable_but_rate_limit_is_not_selected() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_failed(&mut store, "remote-a", "transient").unwrap();
        record_remote_mirror_queue_rate_limited(&mut store, "remote-b").unwrap();

        let summary = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();
        let selected = summary.eligible_items(3);

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].remote_conversation_id, "remote-a");
        assert_eq!(selected[1].remote_conversation_id, "remote-c");
    }

    #[test]
    fn queued_and_capturing_items_are_reconstructed_as_resumable_after_restart() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_item_queued(&mut store, "remote-a", 0).unwrap();
        record_remote_mirror_queue_capture_started(&mut store, "remote-b").unwrap();

        let summary = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();

        assert_eq!(summary.items[0].status, RemoteMirrorQueueStatus::Queued);
        assert_eq!(summary.items[1].status, RemoteMirrorQueueStatus::Capturing);
        assert_eq!(summary.pending_count(), 3);
        assert_eq!(
            summary
                .eligible_items(3)
                .into_iter()
                .map(|item| item.remote_conversation_id)
                .collect::<Vec<_>>(),
            vec!["remote-a", "remote-b", "remote-c"]
        );
    }

    #[test]
    fn already_mirrored_items_are_skipped_while_later_work_remains_eligible() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_completed(&mut store, "remote-a", "fully_mirrored").unwrap();
        record_remote_mirror_queue_completed(&mut store, "remote-b", "partial").unwrap();

        let summary = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();
        let selected = summary.eligible_items(3);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].remote_conversation_id, "remote-c");
    }

    #[test]
    fn invalid_structural_failure_does_not_hide_later_items() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_failed(&mut store, "remote-a", "structural").unwrap();
        record_remote_mirror_queue_item_queued(&mut store, "remote-b", 1).unwrap();

        let summary = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();

        assert_eq!(summary.count(RemoteMirrorQueueStatus::StructuralFailure), 1);
        assert_eq!(summary.items[1].status, RemoteMirrorQueueStatus::Queued);
        assert_eq!(summary.pending_count(), 2);
    }

    #[test]
    fn rerunning_identical_queue_events_is_replay_idempotent() {
        let mut store = MemoryEventStore::default();
        record_remote_mirror_queue_completed(&mut store, "remote-a", "partial").unwrap();
        let first = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();
        let second = derive_remote_mirror_queue(&catalog(), store.events()).unwrap();
        assert_eq!(first, second);
    }
}
