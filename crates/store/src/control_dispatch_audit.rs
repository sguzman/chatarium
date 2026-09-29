//! Dispatch-time freshness validation for routed worker controls.
//!
//! Generic routing replay proves policy/user authorization and one-shot dispatch.
//! This module composes that evidence with worker-control provenance so an
//! orchestration control cannot dispatch after becoming stale while queued.

use crate::EventEnvelope;
use crate::control_admission_audit::{
    ValidatedControlAdmission, replay_validated_control_admissions,
    validate_control_freshness_before,
};
use crate::orchestration_route_audit::{
    ValidatedOrchestrationRoute, replay_validated_orchestration_routes,
};
use crate::routing_audit::{RouteAuditRecord, replay_routing_audit};
use chatarium_core::control::ControlId;
use chatarium_core::control_provenance::ControlIssuer;
use chatarium_core::orchestration::{WorkerId, WorkerPhase};
use chatarium_core::routing::{
    DecisionAuthority, RouteClass, RouteGateState, RouteId, RouteRequest,
};
use chatarium_core::session::SessionId;
use std::collections::BTreeMap;

/// One dispatched orchestration control validated against worker state at the
/// final durable dispatch boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedControlDispatch {
    /// Admitted control identity.
    pub control_id: ControlId,
    /// Target worker identity.
    pub worker_id: WorkerId,
    /// Routed orchestration request.
    pub route: RouteRequest,
    /// Explicit durable control issuer.
    pub issuer: ControlIssuer,
    /// Worker phase that justified original control admission.
    pub admitted_phase: WorkerPhase,
    /// Worker phase that still justified route binding.
    pub bound_phase: WorkerPhase,
    /// Worker phase that still justified final dispatch.
    pub dispatched_phase: WorkerPhase,
    /// Resolved controller session for controller-issued controls.
    pub controller_session_id: Option<SessionId>,
    /// Resolved target worker session for controller-issued controls.
    pub worker_session_id: Option<SessionId>,
    /// Durable route-binding sequence.
    pub bound_sequence: u64,
    /// Durable dispatch sequence.
    pub dispatch_sequence: u64,
    /// Policy/user authority that permitted dispatch.
    pub authorized_by: DecisionAuthority,
}

/// Validate every dispatched orchestration-control route.
///
/// Non-orchestration dispatches are outside this composed view and are ignored.
/// Every dispatched orchestration route must already have a fully validated
/// control binding/provenance chain and the control must still be semantically
/// fresh immediately before dispatch.
pub fn replay_validated_control_dispatches(
    events: &[EventEnvelope],
) -> Result<Vec<ValidatedControlDispatch>, String> {
    let admissions = replay_validated_control_admissions(events)?;
    let validated_routes = replay_validated_orchestration_routes(events)?;
    let routes = replay_routing_audit(events)?;

    let admissions_by_id = admissions
        .into_iter()
        .map(|record| (record.control.control_id, record))
        .collect::<BTreeMap<_, _>>();
    let validated_by_route = validated_routes
        .into_iter()
        .map(|record| (record.route.id, record))
        .collect::<BTreeMap<_, _>>();

    routes
        .into_iter()
        .filter(|route| {
            route.request.class == RouteClass::OrchestrationControl
                && route.dispatch_sequence.is_some()
        })
        .map(|route| validate_dispatch(events, route, &admissions_by_id, &validated_by_route))
        .collect()
}

fn validate_dispatch(
    events: &[EventEnvelope],
    route: RouteAuditRecord,
    admissions: &BTreeMap<ControlId, ValidatedControlAdmission>,
    validated_routes: &BTreeMap<RouteId, ValidatedOrchestrationRoute>,
) -> Result<ValidatedControlDispatch, String> {
    let dispatch_sequence = route
        .dispatch_sequence
        .expect("caller filters to dispatched routes");

    let validated = validated_routes.get(&route.request.id).ok_or_else(|| {
        format!(
            "dispatched orchestration route {} at sequence {} has no validated control binding/provenance",
            route.request.id.get(),
            dispatch_sequence
        )
    })?;

    if validated.bound_sequence >= dispatch_sequence {
        return Err(format!(
            "orchestration route {} dispatched at sequence {} before control-route binding at sequence {}",
            route.request.id.get(),
            dispatch_sequence,
            validated.bound_sequence
        ));
    }

    let admission = admissions.get(&validated.control_id).ok_or_else(|| {
        format!(
            "dispatched orchestration route {} references missing validated control admission {}",
            route.request.id.get(),
            validated.control_id.get()
        )
    })?;

    let dispatched_phase =
        validate_control_freshness_before(events, &admission.control, dispatch_sequence)?;

    let authorized_by = match route.gate_state {
        RouteGateState::Dispatched { authorized_by } => authorized_by,
        state => {
            return Err(format!(
                "route {} has dispatch sequence {} but replay gate is {:?}",
                route.request.id.get(),
                dispatch_sequence,
                state
            ));
        }
    };

    Ok(ValidatedControlDispatch {
        control_id: validated.control_id,
        worker_id: validated.worker_id,
        route: validated.route,
        issuer: validated.issuer,
        admitted_phase: validated.admitted_phase,
        bound_phase: validated.bound_phase,
        dispatched_phase,
        controller_session_id: validated.controller_session_id,
        worker_session_id: validated.worker_session_id,
        bound_sequence: validated.bound_sequence,
        dispatch_sequence,
        authorized_by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuation_audit::{
        record_continuation_lease_created, record_continuation_permit_issued,
    };
    use crate::control_audit::record_worker_control_admitted;
    use crate::control_provenance_audit::record_worker_control_issuer_bound;
    use crate::control_route_audit::record_control_route_bound;
    use crate::routing_audit::{
        RouteUserDecision, record_route_dispatched, record_route_proposed,
        record_route_user_decision,
    };
    use crate::worker_audit::{record_worker_goal_assigned, record_worker_transition};
    use crate::{EventStore, JsonlEventStore, MemoryEventStore};
    use chatarium_core::control::{ControlId, WorkerControl};
    use chatarium_core::control_provenance::{ControlIssuer, ControlProvenance};
    use chatarium_core::control_route::ControlRouteBinding;
    use chatarium_core::orchestration::{
        ContinuationLease, ContinuationLeaseId, WorkerAction, WorkerGoalId, WorkerLifecycle,
    };
    use chatarium_core::routing::{RouteEndpointId, RouteGate, RoutePolicy};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const L1: ContinuationLeaseId = ContinuationLeaseId::new(50);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const G2: WorkerGoalId = WorkerGoalId::new(200);
    const SOURCE: RouteEndpointId = RouteEndpointId::new(20);
    const DESTINATION: RouteEndpointId = RouteEndpointId::new(30);

    fn temp_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-control-dispatch-{label}-{}-{nonce}.jsonl",
            std::process::id()
        ))
    }

    fn working_lifecycle(goal_id: WorkerGoalId) -> WorkerLifecycle {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(goal_id).unwrap();
        lifecycle.start_or_resume(goal_id).unwrap();
        lifecycle
    }

    fn record_working(store: &mut impl EventStore, goal_id: WorkerGoalId) {
        record_worker_goal_assigned(store, W1, goal_id).unwrap();
        record_worker_transition(store, W1, goal_id, WorkerAction::StartOrResume).unwrap();
    }

    fn route(id: u64, class: RouteClass) -> RouteRequest {
        RouteRequest {
            id: RouteId::new(id),
            source: SOURCE,
            destination: DESTINATION,
            class,
        }
    }

    fn admit_user_control(store: &mut impl EventStore, control: &WorkerControl) {
        record_worker_control_admitted(store, control).unwrap();
        record_worker_control_issuer_bound(
            store,
            ControlProvenance::new(control.id(), ControlIssuer::User),
        )
        .unwrap();
    }

    fn bind_route(
        store: &mut impl EventStore,
        control: &WorkerControl,
        request: RouteRequest,
        policy: RoutePolicy,
    ) {
        record_route_proposed(store, request, policy).unwrap();
        record_control_route_bound(
            store,
            ControlRouteBinding::new(control.id(), &request).unwrap(),
        )
        .unwrap();
    }

    fn dispatch(
        store: &mut impl EventStore,
        request: RouteRequest,
        policy: RoutePolicy,
        user_allow: bool,
    ) {
        let mut gate = RouteGate::new(request, policy);
        if user_allow {
            record_route_user_decision(store, request.id, RouteUserDecision::Allow).unwrap();
            gate.user_allow().unwrap();
        }
        let permit = gate.authorize_dispatch(request.id).unwrap();
        record_route_dispatched(store, permit).unwrap();
    }

    fn admitted_stop(id: u64, goal_id: WorkerGoalId) -> WorkerControl {
        let lifecycle = working_lifecycle(goal_id);
        WorkerControl::stop(ControlId::new(id), W1, goal_id, &lifecycle).unwrap()
    }

    #[test]
    fn auto_policy_allowed_dispatch_validates_while_fresh() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = admitted_stop(1, G1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::Allow);
        dispatch(&mut store, request, RoutePolicy::Allow, false);

        let records = replay_validated_control_dispatches(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].authorized_by, DecisionAuthority::Policy);
        assert_eq!(records[0].admitted_phase, WorkerPhase::Working);
        assert_eq!(records[0].bound_phase, WorkerPhase::Working);
        assert_eq!(records[0].dispatched_phase, WorkerPhase::Working);
    }

    #[test]
    fn user_approved_dispatch_validates_while_fresh() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = admitted_stop(1, G1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);
        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let record = replay_validated_control_dispatches(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.authorized_by, DecisionAuthority::User);
        assert!(record.bound_sequence < record.dispatch_sequence);
    }

    #[test]
    fn replacement_goal_after_binding_before_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = admitted_stop(1, G1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);

        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        record_worker_goal_assigned(&mut store, W1, G2).unwrap();
        record_worker_transition(&mut store, W1, G2, WorkerAction::StartOrResume).unwrap();

        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("stale worker control"));
    }

    fn continue_control(store: &mut impl EventStore, id: u64) -> WorkerControl {
        let lifecycle = working_lifecycle(G1);
        let mut lease = ContinuationLease::new(L1, W1, G1, 1);
        record_continuation_lease_created(store, &lease).unwrap();
        let permit = lease.authorize(&lifecycle).unwrap();
        record_continuation_permit_issued(store, &permit).unwrap();
        WorkerControl::continue_work(ControlId::new(id), W1, &lifecycle, permit).unwrap()
    }

    #[test]
    fn continue_entering_needs_input_before_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = continue_control(&mut store, 1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);
        record_worker_transition(&mut store, W1, G1, WorkerAction::RequestInput).unwrap();
        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("Continue"));
        assert!(error.contains("NeedsInput"));
    }

    #[test]
    fn continue_entering_blocked_before_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = continue_control(&mut store, 1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);
        record_worker_transition(&mut store, W1, G1, WorkerAction::MarkBlocked).unwrap();
        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("Continue"));
        assert!(error.contains("Blocked"));
    }

    #[test]
    fn stop_target_completing_before_dispatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = admitted_stop(1, G1);
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("Stop"));
        assert!(error.contains("Completed"));
    }

    #[test]
    fn status_request_remains_valid_when_same_goal_completes_before_dispatch() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let lifecycle = working_lifecycle(G1);
        let control = WorkerControl::status_request(ControlId::new(1), W1, G1, &lifecycle).unwrap();
        admit_user_control(&mut store, &control);
        let request = route(1, RouteClass::OrchestrationControl);
        bind_route(&mut store, &control, request, RoutePolicy::RequireApproval);
        record_worker_transition(&mut store, W1, G1, WorkerAction::Complete).unwrap();
        dispatch(&mut store, request, RoutePolicy::RequireApproval, true);

        let record = replay_validated_control_dispatches(store.events())
            .unwrap()
            .remove(0);
        assert_eq!(record.admitted_phase, WorkerPhase::Working);
        assert_eq!(record.bound_phase, WorkerPhase::Working);
        assert_eq!(record.dispatched_phase, WorkerPhase::Completed);
    }

    #[test]
    fn dispatch_before_control_route_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working(&mut store, G1);
        let control = admitted_stop(1, G1);
        admit_user_control(&mut store, &control);

        let request = route(1, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        dispatch(&mut store, request, RoutePolicy::Allow, false);

        record_control_route_bound(
            &mut store,
            ControlRouteBinding::new(control.id(), &request).unwrap(),
        )
        .unwrap();

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("before control-route binding"));
    }

    #[test]
    fn dispatched_orchestration_route_without_control_fails_closed() {
        let mut store = MemoryEventStore::default();
        let request = route(1, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        dispatch(&mut store, request, RoutePolicy::Allow, false);

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("no validated control binding/provenance"));
    }

    #[test]
    fn generic_non_orchestration_dispatch_is_ignored() {
        let mut store = MemoryEventStore::default();
        let request = route(1, RouteClass::SessionMessage);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        dispatch(&mut store, request, RoutePolicy::Allow, false);

        assert!(
            replay_validated_control_dispatches(store.events())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pending_approval_dispatch_remains_rejected_by_generic_routing_replay() {
        let mut store = MemoryEventStore::default();
        let request = route(1, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();

        let mut gate = RouteGate::new(request, RoutePolicy::Allow);
        let permit = gate.authorize_dispatch(request.id).unwrap();
        record_route_dispatched(&mut store, permit).unwrap();

        let error = replay_validated_control_dispatches(store.events()).unwrap_err();
        assert!(error.contains("PendingApproval"));
    }

    #[test]
    fn validated_dispatch_survives_real_journal_reopen() {
        let path = temp_path("reopen");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_working(&mut store, G1);
            let control = admitted_stop(1, G1);
            admit_user_control(&mut store, &control);
            let request = route(1, RouteClass::OrchestrationControl);
            bind_route(&mut store, &control, request, RoutePolicy::Allow);
            dispatch(&mut store, request, RoutePolicy::Allow, false);
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_validated_control_dispatches(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].authorized_by, DecisionAuthority::Policy);
        assert_eq!(records[0].dispatched_phase, WorkerPhase::Working);
        let _ = fs::remove_file(path);
    }
}
