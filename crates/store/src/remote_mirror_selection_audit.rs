//! Durable user intent for P3 remote conversation mirroring.
//!
//! Selection is distinct from protocol readiness and execution. It records only
//! whether the user wants an already-bound conversation considered for future
//! read-only mirroring when all other gates permit it.

use crate::remote_identity_audit::replay_remote_identity_audit;
use crate::remote_mirror_readiness::{
    RemoteMirrorBlocked, RemoteMirrorReadiness, RemoteMirrorReady, derive_remote_mirror_readiness,
};
use crate::{EventEnvelope, EventStore};
use chatarium_core::{EventKind, LocalConversationId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const REMOTE_MIRROR_SELECTION_SCHEMA: &str = "chatarium-remote-mirror-selection";
const REMOTE_MIRROR_SELECTION_VERSION: u64 = 1;

/// Current restart-replayable mirror-selection state for one local conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMirrorSelectionAuditRecord {
    /// Local conversation whose future mirroring intent changed.
    pub local_conversation_id: LocalConversationId,
    /// Whether the conversation is currently selected for future mirroring.
    pub selected: bool,
    /// Durable sequence of the latest selection-state change.
    pub changed_sequence: u64,
}

/// User selection combined with the evidence-gated P3 readiness result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedRemoteMirrorPlan {
    /// The user has not selected this conversation for future mirroring.
    Unselected,
    /// The user selected it, but protocol/evidence prerequisites are not satisfied.
    SelectedButBlocked(RemoteMirrorBlocked),
    /// The user selected it and the protocol/evidence prerequisites are satisfied.
    SelectedAndReady(RemoteMirrorReady),
}

/// Append one durable mirror-selection intent change.
///
/// Replay remains authoritative for validating ordering and state-transition invariants.
pub fn record_remote_mirror_selection_changed(
    store: &mut impl EventStore,
    local_conversation_id: LocalConversationId,
    selected: bool,
) -> std::io::Result<u64> {
    let payload = serde_json::to_string(&json!({
        "schema": REMOTE_MIRROR_SELECTION_SCHEMA,
        "version": REMOTE_MIRROR_SELECTION_VERSION,
        "record": "remote_mirror_selection_changed",
        "local_conversation_id": local_conversation_id.to_string(),
        "selected": selected,
    }))
    .map_err(invalid_data)?;

    store.append_scoped(
        Some(remote_mirror_selection_scope(local_conversation_id)),
        EventKind::RemoteMirrorSelectionChanged,
        payload,
    )
}

/// Replay current mirror-selection intent for all referenced local conversations.
///
/// A conversation may only become selected after its remote identity binding is
/// already durable. A later binding never retroactively legalizes an earlier selection.
pub fn replay_remote_mirror_selection_audit(
    events: &[EventEnvelope],
) -> Result<Vec<RemoteMirrorSelectionAuditRecord>, String> {
    let bindings = replay_remote_identity_audit(events)?;
    let binding_sequence_by_local = bindings
        .into_iter()
        .map(|record| (record.binding.local_conversation_id(), record.bound_sequence))
        .collect::<BTreeMap<_, _>>();

    let mut current = BTreeMap::<LocalConversationId, RemoteMirrorSelectionAuditRecord>::new();

    for event in events {
        if event.kind != EventKind::RemoteMirrorSelectionChanged {
            continue;
        }

        let payload = typed_payload(event)?;
        let local_conversation_id = required_string(&payload, "local_conversation_id")?
            .parse::<LocalConversationId>()
            .map_err(|error| {
                format!(
                    "invalid remote mirror selection local_conversation_id at sequence {}: {error}",
                    event.sequence
                )
            })?;
        validate_scope(event, local_conversation_id)?;

        let selected = payload
            .get("selected")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                format!(
                    "remote mirror selection at sequence {} is missing bool field 'selected'",
                    event.sequence
                )
            })?;

        let prior = current.get(&local_conversation_id);

        if selected {
            let Some(binding_sequence) = binding_sequence_by_local.get(&local_conversation_id) else {
                return Err(format!(
                    "remote mirror selection at sequence {} selects local conversation {} before any durable remote binding",
                    event.sequence, local_conversation_id
                ));
            };
            if *binding_sequence >= event.sequence {
                return Err(format!(
                    "remote mirror selection at sequence {} selects local conversation {} before its remote binding at sequence {}",
                    event.sequence, local_conversation_id, binding_sequence
                ));
            }
            if prior.is_some_and(|record| record.selected) {
                return Err(format!(
                    "redundant remote mirror select for local conversation {} at sequence {}",
                    local_conversation_id, event.sequence
                ));
            }
        } else if !prior.is_some_and(|record| record.selected) {
            return Err(format!(
                "remote mirror deselection for local conversation {} at sequence {} has no active prior selection",
                local_conversation_id, event.sequence
            ));
        }

        current.insert(
            local_conversation_id,
            RemoteMirrorSelectionAuditRecord {
                local_conversation_id,
                selected,
                changed_sequence: event.sequence,
            },
        );
    }

    let mut records = current.into_values().collect::<Vec<_>>();
    records.sort_by_key(|record| record.changed_sequence);
    Ok(records)
}

/// Derive user selection plus current P3 protocol readiness without appending state.
pub fn derive_selected_remote_mirror_plan(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
) -> Result<SelectedRemoteMirrorPlan, String> {
    let selections = replay_remote_mirror_selection_audit(events)?;
    let selected = selections
        .iter()
        .find(|record| record.local_conversation_id == local_conversation_id)
        .is_some_and(|record| record.selected);

    if !selected {
        return Ok(SelectedRemoteMirrorPlan::Unselected);
    }

    let readiness = derive_remote_mirror_readiness(events, local_conversation_id)?;
    Ok(compose_selection_with_readiness(selected, readiness))
}

fn compose_selection_with_readiness(
    selected: bool,
    readiness: RemoteMirrorReadiness,
) -> SelectedRemoteMirrorPlan {
    if !selected {
        return SelectedRemoteMirrorPlan::Unselected;
    }

    match readiness {
        RemoteMirrorReadiness::Ready(ready) => SelectedRemoteMirrorPlan::SelectedAndReady(ready),
        RemoteMirrorReadiness::Blocked(blocked) => {
            SelectedRemoteMirrorPlan::SelectedButBlocked(blocked)
        }
    }
}

/// Stable local-only scope for one conversation's mirror-selection intent.
#[must_use]
pub fn remote_mirror_selection_scope(local_conversation_id: LocalConversationId) -> String {
    format!("remote-mirror-selection:{local_conversation_id}")
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed remote mirror selection payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(REMOTE_MIRROR_SELECTION_SCHEMA) {
        return Err(format!(
            "remote mirror selection at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "remote mirror selection at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != REMOTE_MIRROR_SELECTION_VERSION {
        return Err(format!(
            "unsupported remote mirror selection version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "remote_mirror_selection_changed" {
        return Err(format!(
            "remote mirror selection at sequence {} has wrong record kind",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    local_conversation_id: LocalConversationId,
) -> Result<(), String> {
    let expected = remote_mirror_selection_scope(local_conversation_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "remote mirror selection at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        format!("typed remote mirror selection is missing string field '{field}'")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::SqliteProjection;
    use crate::remote_identity_audit::record_remote_conversation_bound;
    use crate::remote_mirror_readiness::{RemoteMirrorReady, RemoteMirrorReadiness};
    use crate::remote_read_audit::record_remote_read_observation;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::RemoteReadObservationId;
    use chatarium_core::TurnEvidence;
    use chatarium_core::remote::{
        ProtocolObservationRevision, RemoteConversationBinding, RemoteConversationId,
    };
    use chatarium_protocol::read::{
        JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation,
    };
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn binding(local: LocalConversationId, revision: &str) -> RemoteConversationBinding {
        RemoteConversationBinding::new(
            local,
            RemoteConversationId::new("opaque-remote").unwrap(),
            ProtocolObservationRevision::new(revision).unwrap(),
        )
    }

    fn c02(revision: &str) -> ReadObservation {
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
    fn selection_survives_real_journal_reopen() {
        let path = temp_path("selection-reopen", "jsonl");
        let local = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_remote_mirror_selection_audit(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].selected);
        assert_eq!(records[0].local_conversation_id, local);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn selection_before_binding_is_rejected_even_if_binding_arrives_later() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();

        let error = replay_remote_mirror_selection_audit(store.events()).unwrap_err();
        assert!(error.contains("before its remote binding"));
    }

    #[test]
    fn deselection_survives_reopen() {
        let path = temp_path("deselection-reopen", "jsonl");
        let local = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
            record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
            record_remote_mirror_selection_changed(&mut store, local, false).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_remote_mirror_selection_audit(reopened.events()).unwrap();
        assert!(!records[0].selected);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn deselection_without_active_selection_is_rejected() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, false).unwrap();

        assert!(
            replay_remote_mirror_selection_audit(store.events())
                .unwrap_err()
                .contains("no active prior selection")
        );
    }

    #[test]
    fn redundant_selection_changes_are_rejected() {
        let local = LocalConversationId::new();

        let mut selects = MemoryEventStore::default();
        record_remote_conversation_bound(&mut selects, &binding(local, "rev-a")).unwrap();
        record_remote_mirror_selection_changed(&mut selects, local, true).unwrap();
        record_remote_mirror_selection_changed(&mut selects, local, true).unwrap();
        assert!(
            replay_remote_mirror_selection_audit(selects.events())
                .unwrap_err()
                .contains("redundant remote mirror select")
        );

        let mut deselects = MemoryEventStore::default();
        record_remote_conversation_bound(&mut deselects, &binding(local, "rev-a")).unwrap();
        record_remote_mirror_selection_changed(&mut deselects, local, true).unwrap();
        record_remote_mirror_selection_changed(&mut deselects, local, false).unwrap();
        record_remote_mirror_selection_changed(&mut deselects, local, false).unwrap();
        assert!(
            replay_remote_mirror_selection_audit(deselects.events())
                .unwrap_err()
                .contains("no active prior selection")
        );
    }

    #[test]
    fn malformed_payload_and_scope_mismatch_are_rejected() {
        let local = LocalConversationId::new();

        let mut malformed = MemoryEventStore::default();
        malformed
            .append_scoped(
                Some(remote_mirror_selection_scope(local)),
                EventKind::RemoteMirrorSelectionChanged,
                "{not-json".to_owned(),
            )
            .unwrap();
        assert!(replay_remote_mirror_selection_audit(malformed.events()).is_err());

        let mut wrong_scope = MemoryEventStore::default();
        record_remote_conversation_bound(&mut wrong_scope, &binding(local, "rev-a")).unwrap();
        wrong_scope
            .append_scoped(
                Some("remote-mirror-selection:wrong".to_owned()),
                EventKind::RemoteMirrorSelectionChanged,
                json!({
                    "schema": REMOTE_MIRROR_SELECTION_SCHEMA,
                    "version": REMOTE_MIRROR_SELECTION_VERSION,
                    "record": "remote_mirror_selection_changed",
                    "local_conversation_id": local.to_string(),
                    "selected": true,
                })
                .to_string(),
            )
            .unwrap();
        assert!(
            replay_remote_mirror_selection_audit(wrong_scope.events())
                .unwrap_err()
                .contains("scope")
        );
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "draft".to_owned())
            .unwrap();
        assert!(
            replay_remote_mirror_selection_audit(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn torn_tail_cannot_fabricate_selection_change() {
        let path = temp_path("torn-tail", "jsonl");
        let local = LocalConversationId::new();
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":2,"kind":"remote_mirror_selection_changed""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(
            replay_remote_mirror_selection_audit(reopened.events())
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_selection_without_schema_bump() {
        let db = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();

        let mut projection = SqliteProjection::open(&db).unwrap();
        projection.rebuild(store.events()).unwrap();
        assert_eq!(
            projection
                .events_of_kind(EventKind::RemoteMirrorSelectionChanged)
                .unwrap()
                .len(),
            1
        );
        drop(projection);
        let _ = fs::remove_file(db);
    }

    #[test]
    fn selection_event_does_not_mutate_turn_evidence() {
        let mut evidence = TurnEvidence::default();
        evidence
            .apply_event_kind(EventKind::RemoteMirrorSelectionChanged)
            .unwrap();
        assert_eq!(evidence, TurnEvidence::default());
    }

    #[test]
    fn unselected_plan_is_unselected_even_when_bound() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();

        assert_eq!(
            derive_selected_remote_mirror_plan(store.events(), local).unwrap(),
            SelectedRemoteMirrorPlan::Unselected
        );
    }

    #[test]
    fn current_production_c02_no_baseline_blocks_selected_plan() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        let revision = "future-c02-observation";
        record_remote_conversation_bound(&mut store, &binding(local, revision)).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        record_remote_read_observation(
            &mut store,
            RemoteReadObservationId::new(),
            &c02(revision),
        )
        .unwrap();

        assert!(matches!(
            derive_selected_remote_mirror_plan(store.events(), local).unwrap(),
            SelectedRemoteMirrorPlan::SelectedButBlocked(RemoteMirrorBlocked::NoBaseline { .. })
        ));
    }

    #[test]
    fn synthetic_validated_readiness_composes_to_selected_and_ready() {
        let local = LocalConversationId::new();
        let ready = RemoteMirrorReady {
            local_conversation_id: local,
            remote_conversation_id: RemoteConversationId::new("opaque-remote").unwrap(),
            protocol_revision: ProtocolObservationRevision::new("rev-a").unwrap(),
            read_observation_id: RemoteReadObservationId::new(),
            binding_sequence: 1,
            read_observation_sequence: 2,
        };

        assert!(matches!(
            compose_selection_with_readiness(true, RemoteMirrorReadiness::Ready(ready)),
            SelectedRemoteMirrorPlan::SelectedAndReady(_)
        ));
    }

    #[test]
    fn plan_derivation_is_read_only() {
        let mut store = MemoryEventStore::default();
        let local = LocalConversationId::new();
        record_remote_conversation_bound(&mut store, &binding(local, "rev-a")).unwrap();
        record_remote_mirror_selection_changed(&mut store, local, true).unwrap();
        let before = store.events().to_vec();

        let _ = derive_selected_remote_mirror_plan(store.events(), local).unwrap();
        assert_eq!(store.events(), before.as_slice());
    }

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-remote-mirror-selection-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }
}
