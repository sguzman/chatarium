//! Durable bounded-continuation authority replay.
//!
//! The journal is authoritative over lease creation, permit issuance, and permit
//! consumption by admitted Continue controls. Restart reconstructs authority
//! history; it never mints a fresh lease or permit.

use crate::control_audit::{ControlAuditRecord, replay_control_audit};
use crate::worker_audit::replay_worker_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::control::{ControlId, WorkerControlKind};
use chatarium_core::orchestration::{
    ContinuationLease, ContinuationLeaseId, ContinuationPermit, ContinuationPermitRef,
    WorkerGoalId, WorkerId, WorkerPhase,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CONTINUATION_AUDIT_SCHEMA: &str = "chatarium-continuation-audit";
const CONTINUATION_AUDIT_VERSION: u64 = 1;

/// One durably issued continuation permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationPermitAuditRecord {
    /// Originating lease.
    pub lease_id: ContinuationLeaseId,
    /// Worker identity authorized by the lease.
    pub worker_id: WorkerId,
    /// Goal identity authorized by the lease.
    pub goal_id: WorkerGoalId,
    /// One-based ordinal within the lease.
    pub ordinal: u32,
    /// Durable permit-issuance sequence.
    pub issued_sequence: u64,
    /// Continue control that consumed this permit, if any.
    pub consumed_by: Option<ControlId>,
    /// Durable control-admission sequence that consumed it, if any.
    pub consumed_sequence: Option<u64>,
}

/// Restart-replayed state of one bounded continuation lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationLeaseAuditRecord {
    /// Lease identity.
    pub lease_id: ContinuationLeaseId,
    /// Worker bound to the lease.
    pub worker_id: WorkerId,
    /// Goal bound to the lease.
    pub goal_id: WorkerGoalId,
    /// Original explicit allowance.
    pub allowance: u32,
    /// Number of permit ordinals durably issued.
    pub issued: u32,
    /// Remaining unissued allowance.
    pub remaining: u32,
    /// Number of issued permits consumed by Continue controls.
    pub consumed: u32,
    /// Durable lease-creation sequence.
    pub created_sequence: u64,
    /// Issued permits in ordinal order.
    pub permits: Vec<ContinuationPermitAuditRecord>,
}

/// Append one explicit bounded continuation lease creation.
pub fn record_continuation_lease_created(
    store: &mut impl EventStore,
    lease: &ContinuationLease,
) -> std::io::Result<u64> {
    append_typed(
        store,
        continuation_lease_scope(lease.id()),
        EventKind::ContinuationLeaseCreated,
        json!({
            "schema": CONTINUATION_AUDIT_SCHEMA,
            "version": CONTINUATION_AUDIT_VERSION,
            "record": "lease_created",
            "lease_id": lease.id().get(),
            "worker_id": lease.worker_id().get(),
            "goal_id": lease.goal_id().get(),
            "allowance": lease.allowance(),
        }),
    )
}

/// Append one permit issuance after live lease authorization.
///
/// The caller still owns the move-only permit and may consume it into exactly
/// one WorkerControl after this durable append succeeds.
pub fn record_continuation_permit_issued(
    store: &mut impl EventStore,
    permit: &ContinuationPermit,
) -> std::io::Result<u64> {
    append_typed(
        store,
        continuation_permit_scope(permit.lease_id(), permit.ordinal()),
        EventKind::ContinuationPermitIssued,
        json!({
            "schema": CONTINUATION_AUDIT_SCHEMA,
            "version": CONTINUATION_AUDIT_VERSION,
            "record": "permit_issued",
            "lease_id": permit.lease_id().get(),
            "worker_id": permit.worker_id().get(),
            "goal_id": permit.goal_id().get(),
            "ordinal": permit.ordinal(),
        }),
    )
}

/// Reconstruct one durable continuation lease as a live core lease ready for
/// its next permit issuance.
///
/// This does not create new authority: the original allowance and every already
/// issued ordinal come from the journal. The caller must still durably record
/// any newly authorized permit before consuming it into a Continue control.
pub fn reconstruct_continuation_lease_for_next_issue(
    events: &[EventEnvelope],
    lease_id: ContinuationLeaseId,
) -> Result<ContinuationLease, String> {
    let record = replay_continuation_audit(events)?
        .into_iter()
        .find(|record| record.lease_id == lease_id)
        .ok_or_else(|| format!("continuation lease {} does not exist", lease_id.get()))?;

    let worker = replay_worker_audit(events)?
        .into_iter()
        .find(|worker| worker.worker_id == record.worker_id)
        .ok_or_else(|| {
            format!(
                "continuation lease {} references worker {} without durable lifecycle state",
                lease_id.get(),
                record.worker_id.get()
            )
        })?;

    let mut lease = ContinuationLease::new(
        record.lease_id,
        record.worker_id,
        record.goal_id,
        record.allowance,
    );
    for expected_ordinal in 1..=record.issued {
        let permit = lease.authorize(&worker.lifecycle).map_err(|error| {
            format!(
                "continuation lease {} cannot reconstruct issued ordinal {} against current worker state: {error:?}",
                lease_id.get(),
                expected_ordinal
            )
        })?;
        if permit.ordinal() != expected_ordinal {
            return Err(format!(
                "continuation lease {} reconstructed ordinal {}, expected {}",
                lease_id.get(),
                permit.ordinal(),
                expected_ordinal
            ));
        }
    }

    if lease.issued() != record.issued || lease.remaining() != record.remaining {
        return Err(format!(
            "continuation lease {} live reconstruction disagrees with durable allowance",
            lease_id.get()
        ));
    }

    Ok(lease)
}

/// Replay all continuation leases, issued permits, and Continue consumption.
pub fn replay_continuation_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ContinuationLeaseAuditRecord>, String> {
    let controls = replay_control_audit(events)?;
    let controls_by_sequence = controls
        .into_iter()
        .map(|record| (record.admitted_sequence, record))
        .collect::<BTreeMap<_, _>>();

    let mut leases = BTreeMap::<ContinuationLeaseId, ReplayLease>::new();

    for event in events {
        match event.kind {
            EventKind::ContinuationLeaseCreated => {
                replay_lease_created(events, &mut leases, event)?;
            }
            EventKind::ContinuationPermitIssued => {
                replay_permit_issued(events, &mut leases, event)?;
            }
            EventKind::WorkerControlAdmitted => {
                let control = controls_by_sequence.get(&event.sequence).ok_or_else(|| {
                    format!(
                        "continuation replay cannot resolve control admission at sequence {}",
                        event.sequence
                    )
                })?;
                replay_control_consumption(&mut leases, control)?;
            }
            _ => {}
        }
    }

    let mut records = leases
        .into_values()
        .map(ReplayLease::into_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.created_sequence);
    Ok(records)
}

struct ReplayLease {
    lease: ContinuationLease,
    created_sequence: u64,
    permits: Vec<ContinuationPermitAuditRecord>,
}

impl ReplayLease {
    fn into_record(self) -> ContinuationLeaseAuditRecord {
        let consumed = self
            .permits
            .iter()
            .filter(|permit| permit.consumed_by.is_some())
            .count() as u32;

        ContinuationLeaseAuditRecord {
            lease_id: self.lease.id(),
            worker_id: self.lease.worker_id(),
            goal_id: self.lease.goal_id(),
            allowance: self.lease.allowance(),
            issued: self.lease.issued(),
            remaining: self.lease.remaining(),
            consumed,
            created_sequence: self.created_sequence,
            permits: self.permits,
        }
    }
}

fn replay_lease_created(
    events: &[EventEnvelope],
    leases: &mut BTreeMap<ContinuationLeaseId, ReplayLease>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "lease_created")?;
    let lease_id = ContinuationLeaseId::new(required_u64(&payload, "lease_id")?);
    let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
    let goal_id = WorkerGoalId::new(required_u64(&payload, "goal_id")?);
    let allowance = required_u32(&payload, "allowance")?;
    validate_scope(event, &continuation_lease_scope(lease_id))?;

    if leases.contains_key(&lease_id) {
        return Err(format!(
            "duplicate continuation lease {} at sequence {}",
            lease_id.get(),
            event.sequence
        ));
    }

    let worker = worker_before(events, worker_id, event.sequence)?.ok_or_else(|| {
        format!(
            "continuation lease {} created before worker {} has durable lifecycle state",
            lease_id.get(),
            worker_id.get()
        )
    })?;

    let current_goal = worker.lifecycle.goal_id().ok_or_else(|| {
        format!(
            "continuation lease {} created while worker {} has no goal",
            lease_id.get(),
            worker_id.get()
        )
    })?;
    if current_goal != goal_id {
        return Err(format!(
            "continuation lease {} targets goal {}, worker {} current goal is {}",
            lease_id.get(),
            goal_id.get(),
            worker_id.get(),
            current_goal.get()
        ));
    }
    if worker.lifecycle.phase().is_terminal() {
        return Err(format!(
            "continuation lease {} cannot be created for terminal worker phase {:?}",
            lease_id.get(),
            worker.lifecycle.phase()
        ));
    }

    leases.insert(
        lease_id,
        ReplayLease {
            lease: ContinuationLease::new(lease_id, worker_id, goal_id, allowance),
            created_sequence: event.sequence,
            permits: Vec::new(),
        },
    );
    Ok(())
}

fn replay_permit_issued(
    events: &[EventEnvelope],
    leases: &mut BTreeMap<ContinuationLeaseId, ReplayLease>,
    event: &EventEnvelope,
) -> Result<(), String> {
    let payload = typed_payload(event, "permit_issued")?;
    let lease_id = ContinuationLeaseId::new(required_u64(&payload, "lease_id")?);
    let worker_id = WorkerId::new(required_u64(&payload, "worker_id")?);
    let goal_id = WorkerGoalId::new(required_u64(&payload, "goal_id")?);
    let ordinal = required_u32(&payload, "ordinal")?;
    validate_scope(event, &continuation_permit_scope(lease_id, ordinal))?;

    let state = leases.get_mut(&lease_id).ok_or_else(|| {
        format!(
            "continuation permit {}:{} issued before lease creation",
            lease_id.get(),
            ordinal
        )
    })?;

    if state.lease.worker_id() != worker_id || state.lease.goal_id() != goal_id {
        return Err(format!(
            "continuation permit {}:{} identity does not match lease worker/goal",
            lease_id.get(),
            ordinal
        ));
    }

    let worker = worker_before(events, worker_id, event.sequence)?.ok_or_else(|| {
        format!(
            "continuation permit {}:{} issued before worker lifecycle exists",
            lease_id.get(),
            ordinal
        )
    })?;

    let expected = state.lease.authorize(&worker.lifecycle).map_err(|error| {
        format!(
            "continuation permit {}:{} is not issuable at sequence {}: {error:?}",
            lease_id.get(),
            ordinal,
            event.sequence
        )
    })?;

    if expected.ordinal() != ordinal {
        return Err(format!(
            "continuation permit {} ordinal out of sequence: journal {}, expected {}",
            lease_id.get(),
            ordinal,
            expected.ordinal()
        ));
    }
    if expected.worker_id() != worker_id || expected.goal_id() != goal_id {
        return Err(format!(
            "continuation permit {}:{} does not match replayed lease authority",
            lease_id.get(),
            ordinal
        ));
    }

    state.permits.push(ContinuationPermitAuditRecord {
        lease_id,
        worker_id,
        goal_id,
        ordinal,
        issued_sequence: event.sequence,
        consumed_by: None,
        consumed_sequence: None,
    });
    Ok(())
}

fn replay_control_consumption(
    leases: &mut BTreeMap<ContinuationLeaseId, ReplayLease>,
    control: &ControlAuditRecord,
) -> Result<(), String> {
    match control.kind {
        WorkerControlKind::Continue { permit_ordinal } => {
            let permit_ref = control.continuation_permit.ok_or_else(|| {
                format!(
                    "Continue control {} has no durable continuation lease provenance",
                    control.control_id.get()
                )
            })?;

            if permit_ref.ordinal != permit_ordinal {
                return Err(format!(
                    "Continue control {} permit ordinal {} disagrees with permit reference {}",
                    control.control_id.get(),
                    permit_ordinal,
                    permit_ref.ordinal
                ));
            }

            let state = leases.get_mut(&permit_ref.lease_id).ok_or_else(|| {
                format!(
                    "Continue control {} references missing continuation lease {}",
                    control.control_id.get(),
                    permit_ref.lease_id.get()
                )
            })?;

            if state.lease.worker_id() != control.worker_id
                || state.lease.goal_id() != control.goal_id
            {
                return Err(format!(
                    "Continue control {} worker/goal does not match continuation lease {}",
                    control.control_id.get(),
                    permit_ref.lease_id.get()
                ));
            }

            let permit = state
                .permits
                .iter_mut()
                .find(|permit| permit.ordinal == permit_ref.ordinal)
                .ok_or_else(|| {
                    format!(
                        "Continue control {} consumes permit {}:{} before durable issuance",
                        control.control_id.get(),
                        permit_ref.lease_id.get(),
                        permit_ref.ordinal
                    )
                })?;

            if permit.issued_sequence >= control.admitted_sequence {
                return Err(format!(
                    "Continue control {} admission at sequence {} precedes permit issuance at sequence {}",
                    control.control_id.get(),
                    control.admitted_sequence,
                    permit.issued_sequence
                ));
            }

            if let Some(existing) = permit.consumed_by {
                return Err(format!(
                    "continuation permit {}:{} already consumed by control {}; cannot also consume control {}",
                    permit_ref.lease_id.get(),
                    permit_ref.ordinal,
                    existing.get(),
                    control.control_id.get()
                ));
            }

            permit.consumed_by = Some(control.control_id);
            permit.consumed_sequence = Some(control.admitted_sequence);
            Ok(())
        }
        _ => {
            if control.continuation_permit.is_some() {
                return Err(format!(
                    "non-Continue control {} carries continuation permit provenance",
                    control.control_id.get()
                ));
            }
            Ok(())
        }
    }
}

/// Stable scope for one continuation lease.
#[must_use]
pub fn continuation_lease_scope(lease_id: ContinuationLeaseId) -> String {
    format!("continuation-lease:{}", lease_id.get())
}

/// Stable scope for one issued permit ordinal.
#[must_use]
pub fn continuation_permit_scope(lease_id: ContinuationLeaseId, ordinal: u32) -> String {
    format!("continuation-permit:{}:{}", lease_id.get(), ordinal)
}

fn worker_before(
    events: &[EventEnvelope],
    worker_id: WorkerId,
    exclusive_sequence: u64,
) -> Result<Option<crate::worker_audit::WorkerAuditRecord>, String> {
    let prefix_end = events
        .iter()
        .position(|event| event.sequence >= exclusive_sequence)
        .unwrap_or(events.len());
    Ok(replay_worker_audit(&events[..prefix_end])?
        .into_iter()
        .find(|record| record.worker_id == worker_id))
}

fn append_typed(
    store: &mut impl EventStore,
    scope: String,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(Some(scope), kind, encoded)
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed continuation payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(CONTINUATION_AUDIT_SCHEMA) {
        return Err(format!(
            "continuation event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "continuation event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != CONTINUATION_AUDIT_VERSION {
        return Err(format!(
            "unsupported continuation payload version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != expected_record {
        return Err(format!(
            "continuation event at sequence {} has wrong record kind",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "continuation event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed continuation payload is missing integer field '{field}'"))
}

fn required_u32(value: &Value, field: &str) -> Result<u32, String> {
    let value = required_u64(value, field)?;
    u32::try_from(value).map_err(|_| format!("continuation field '{field}' exceeds u32"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed continuation payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_audit::{control_scope, record_worker_control_admitted};
    use crate::projection::SqliteProjection;
    use crate::worker_audit::{record_worker_goal_assigned, record_worker_transition};
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::control::WorkerControl;
    use chatarium_core::orchestration::{WorkerAction, WorkerLifecycle};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const W2: WorkerId = WorkerId::new(20);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const G2: WorkerGoalId = WorkerGoalId::new(200);
    const L1: ContinuationLeaseId = ContinuationLeaseId::new(1000);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-continuation-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn working_lifecycle(goal: WorkerGoalId) -> WorkerLifecycle {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(goal).unwrap();
        lifecycle.start_or_resume(goal).unwrap();
        lifecycle
    }

    fn record_working(store: &mut impl EventStore, worker: WorkerId, goal: WorkerGoalId) {
        record_worker_goal_assigned(store, worker, goal).unwrap();
        record_worker_transition(store, worker, goal, WorkerAction::StartOrResume).unwrap();
    }

    fn create_lease(store: &mut impl EventStore, allowance: u32) -> ContinuationLease {
        let lease = ContinuationLease::new(L1, W1, G1, allowance);
        record_continuation_lease_created(store, &lease).unwrap();
        lease
    }

    fn issue(
        store: &mut impl EventStore,
        lease: &mut ContinuationLease,
        lifecycle: &WorkerLifecycle,
    ) -> ContinuationPermit {
        let permit = lease.authorize(lifecycle).unwrap();
        record_continuation_permit_issued(store, &permit).unwrap();
        permit
    }

    #[test]
    fn lease_permit_and_consumption_survive_reopen() {
        let path = temp_path("reopen", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, W1, G1);
            let lifecycle = working_lifecycle(G1);
            let mut lease = create_lease(&mut store, 2);
            let permit = issue(&mut store, &mut lease, &lifecycle);
            let control =
                WorkerControl::continue_work(ControlId::new(1), W1, &lifecycle, permit).unwrap();
            record_worker_control_admitted(&mut store, &control).unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_continuation_audit(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].lease_id, L1);
        assert_eq!(records[0].allowance, 2);
        assert_eq!(records[0].issued, 1);
        assert_eq!(records[0].remaining, 1);
        assert_eq!(records[0].consumed, 1);
        assert_eq!(records[0].permits[0].consumed_by, Some(ControlId::new(1)));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn live_reconstruction_resumes_at_next_durable_ordinal() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let mut lease = create_lease(&mut store, 3);
        let lifecycle = working_lifecycle(G1);
        let first = issue(&mut store, &mut lease, &lifecycle);
        assert_eq!(first.ordinal(), 1);
        let second = issue(&mut store, &mut lease, &lifecycle);
        assert_eq!(second.ordinal(), 2);

        let mut reconstructed =
            reconstruct_continuation_lease_for_next_issue(store.events(), LEASE).unwrap();
        assert_eq!(reconstructed.issued(), 2);
        assert_eq!(reconstructed.remaining(), 1);
        let third = reconstructed.authorize(&lifecycle).unwrap();
        assert_eq!(third.ordinal(), 3);
    }

    #[test]
    fn live_reconstruction_fails_closed_when_worker_is_no_longer_working() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let mut lease = create_lease(&mut store, 2);
        let lifecycle = working_lifecycle(G1);
        let _ = issue(&mut store, &mut lease, &lifecycle);
        record_worker_transition(&mut store, W1, G1, WorkerAction::RequestInput).unwrap();

        let error =
            reconstruct_continuation_lease_for_next_issue(store.events(), LEASE).unwrap_err();
        assert!(error.contains("cannot reconstruct issued ordinal"));
    }

    #[test]
    fn sequential_permits_reconstruct_remaining_allowance() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lifecycle = working_lifecycle(G1);
        let mut lease = create_lease(&mut store, 3);
        let _ = issue(&mut store, &mut lease, &lifecycle);
        let _ = issue(&mut store, &mut lease, &lifecycle);

        let record = replay_continuation_audit(store.events()).unwrap().remove(0);
        assert_eq!(record.allowance, 3);
        assert_eq!(record.issued, 2);
        assert_eq!(record.remaining, 1);
        assert_eq!(record.consumed, 0);
    }

    #[test]
    fn permit_issuance_while_not_working_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_worker_goal_assigned(&mut store, W1, G1).unwrap();
        let lease = ContinuationLease::new(L1, W1, G1, 1);
        record_continuation_lease_created(&mut store, &lease).unwrap();
        store
            .append_scoped(
                Some(continuation_permit_scope(L1, 1)),
                EventKind::ContinuationPermitIssued,
                permit_payload(L1, W1, G1, 1),
            )
            .unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("NotWorking"));
    }

    #[test]
    fn permit_issuance_for_replaced_goal_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lease = ContinuationLease::new(L1, W1, G1, 2);
        record_continuation_lease_created(&mut store, &lease).unwrap();
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        record_worker_goal_assigned(&mut store, W1, G2).unwrap();
        record_worker_transition(&mut store, W1, G2, WorkerAction::StartOrResume).unwrap();
        store
            .append_scoped(
                Some(continuation_permit_scope(L1, 1)),
                EventKind::ContinuationPermitIssued,
                permit_payload(L1, W1, G1, 1),
            )
            .unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("GoalMismatch"));
    }

    #[test]
    fn duplicate_lease_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lease = ContinuationLease::new(L1, W1, G1, 2);
        record_continuation_lease_created(&mut store, &lease).unwrap();
        record_continuation_lease_created(&mut store, &lease).unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("duplicate continuation lease"));
    }

    #[test]
    fn skipped_or_duplicate_permit_ordinals_are_rejected() {
        for ordinals in [[2_u32, 0_u32], [1_u32, 1_u32]] {
            let mut store = MemoryEventStore::default();
            record_working(&mut store, W1, G1);
            let lease = ContinuationLease::new(L1, W1, G1, 3);
            record_continuation_lease_created(&mut store, &lease).unwrap();

            for ordinal in ordinals {
                if ordinal == 0 {
                    continue;
                }
                store
                    .append_scoped(
                        Some(continuation_permit_scope(L1, ordinal)),
                        EventKind::ContinuationPermitIssued,
                        permit_payload(L1, W1, G1, ordinal),
                    )
                    .unwrap();
            }

            assert!(replay_continuation_audit(store.events()).is_err());
        }
    }

    #[test]
    fn issuance_beyond_allowance_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lease = ContinuationLease::new(L1, W1, G1, 1);
        record_continuation_lease_created(&mut store, &lease).unwrap();

        for ordinal in [1, 2] {
            store
                .append_scoped(
                    Some(continuation_permit_scope(L1, ordinal)),
                    EventKind::ContinuationPermitIssued,
                    permit_payload(L1, W1, G1, ordinal),
                )
                .unwrap();
        }

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("AllowanceExhausted"));
    }

    #[test]
    fn same_permit_cannot_feed_two_continue_controls() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lifecycle = working_lifecycle(G1);
        let mut lease = create_lease(&mut store, 1);
        let permit = issue(&mut store, &mut lease, &lifecycle);
        let control =
            WorkerControl::continue_work(ControlId::new(1), W1, &lifecycle, permit).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();

        let payload = json!({
            "schema": "chatarium-control-audit",
            "version": 2,
            "record": "control_admitted",
            "control_id": 2,
            "worker_id": W1.get(),
            "goal_id": G1.get(),
            "kind": "continue",
            "permit_ordinal": 1,
            "lease_id": L1.get(),
        })
        .to_string();
        store
            .append_scoped(
                Some(control_scope(ControlId::new(2))),
                EventKind::WorkerControlAdmitted,
                payload,
            )
            .unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("already consumed"));
    }

    #[test]
    fn fabricated_or_unissued_permit_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lease = create_lease(&mut store, 2);

        let payload = json!({
            "schema": "chatarium-control-audit",
            "version": 2,
            "record": "control_admitted",
            "control_id": 1,
            "worker_id": W1.get(),
            "goal_id": G1.get(),
            "kind": "continue",
            "permit_ordinal": 1,
            "lease_id": lease.id().get(),
        })
        .to_string();
        store
            .append_scoped(
                Some(control_scope(ControlId::new(1))),
                EventKind::WorkerControlAdmitted,
                payload,
            )
            .unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("before durable issuance"));
    }

    #[test]
    fn legacy_continue_without_lease_provenance_fails_authority_replay() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        store
            .append_scoped(
                Some(control_scope(ControlId::new(1))),
                EventKind::WorkerControlAdmitted,
                json!({
                    "schema": "chatarium-control-audit",
                    "version": 1,
                    "record": "control_admitted",
                    "control_id": 1,
                    "worker_id": W1.get(),
                    "goal_id": G1.get(),
                    "kind": "continue",
                    "permit_ordinal": 1,
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("no durable continuation lease provenance"));
    }

    #[test]
    fn lease_creation_for_terminal_goal_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        let lease = ContinuationLease::new(L1, W1, G1, 1);
        record_continuation_lease_created(&mut store, &lease).unwrap();

        let error = replay_continuation_audit(store.events()).unwrap_err();
        assert!(error.contains("terminal worker phase"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_permit() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, W1, G1);
            let lease = ContinuationLease::new(L1, W1, G1, 2);
            record_continuation_lease_created(&mut store, &lease).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":4,"kind":"continuation_permit_issued""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let record = replay_continuation_audit(reopened.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.issued, 0);
        assert_eq!(record.remaining, 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn generic_sqlite_projection_carries_authority_events_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        record_working(&mut store, W1, G1);
        let lifecycle = working_lifecycle(G1);
        let mut lease = create_lease(&mut store, 1);
        let _ = issue(&mut store, &mut lease, &lifecycle);

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        assert_eq!(
            projection
                .events_of_kind(EventKind::ContinuationLeaseCreated)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            projection
                .events_of_kind(EventKind::ContinuationPermitIssued)
                .unwrap()
                .len(),
            1
        );

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn wrong_worker_permit_cannot_be_admitted_live() {
        let lifecycle = working_lifecycle(G1);
        let mut lease = ContinuationLease::new(L1, W2, G1, 1);
        let permit = lease.authorize(&lifecycle).unwrap();
        assert!(WorkerControl::continue_work(ControlId::new(1), W1, &lifecycle, permit).is_err());
    }

    fn permit_payload(
        lease_id: ContinuationLeaseId,
        worker_id: WorkerId,
        goal_id: WorkerGoalId,
        ordinal: u32,
    ) -> String {
        json!({
            "schema": CONTINUATION_AUDIT_SCHEMA,
            "version": CONTINUATION_AUDIT_VERSION,
            "record": "permit_issued",
            "lease_id": lease_id.get(),
            "worker_id": worker_id.get(),
            "goal_id": goal_id.get(),
            "ordinal": ordinal,
        })
        .to_string()
    }
}
