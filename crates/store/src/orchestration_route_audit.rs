//! Composed validation for routed worker controls.
//!
//! This module validates durable control-route bindings against explicit issuer
//! provenance and, for controller-issued controls, against the durable
//! controller/worker session topology. It creates no new authority or transport.

use crate::EventEnvelope;
use crate::control_admission_audit::{
    ValidatedControlAdmission, replay_validated_control_admissions,
    validate_control_freshness_before,
};
use crate::control_audit::ControlAuditRecord;
use crate::control_provenance_audit::{
    ControlProvenanceAuditRecord, replay_control_provenance_audit,
};
use crate::control_route_audit::{ControlRouteAuditRecord, replay_control_route_audit};
use crate::routing_audit::{RouteAuditRecord, replay_routing_audit};
use crate::session_audit::{SessionAuditRecord, replay_session_audit, worker_session_before};
use crate::supervision_audit::{ControllerWorkerAuditRecord, replay_supervision_audit};
use chatarium_core::control::ControlId;
use chatarium_core::control_provenance::ControlIssuer;
use chatarium_core::orchestration::{WorkerId, WorkerPhase};
use chatarium_core::routing::{RouteClass, RouteId, RouteRequest};
use chatarium_core::session::SessionId;
use std::collections::BTreeMap;

/// One control-route binding validated against its durable issuer/topology facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedOrchestrationRoute {
    /// Admitted control identity.
    pub control_id: ControlId,
    /// Target worker identity from the admitted control.
    pub worker_id: WorkerId,
    /// Bound orchestration route.
    pub route: RouteRequest,
    /// Explicit durable issuer provenance.
    pub issuer: ControlIssuer,
    /// Worker phase that justified durable admission.
    pub admitted_phase: WorkerPhase,
    /// Worker phase that still justified the control at route binding.
    pub bound_phase: WorkerPhase,
    /// Resolved controller session for controller-issued controls.
    pub controller_session_id: Option<SessionId>,
    /// Resolved target worker session for controller-issued controls.
    pub worker_session_id: Option<SessionId>,
    /// Durable sequence where control and route were correlated.
    pub bound_sequence: u64,
}

/// Validate every durable control-route binding against issuer provenance.
///
/// Controller-issued controls must follow the registered session/supervision
/// topology. User-issued controls do not invent a user routing endpoint.
pub fn replay_validated_orchestration_routes(
    events: &[EventEnvelope],
) -> Result<Vec<ValidatedOrchestrationRoute>, String> {
    let admissions = replay_validated_control_admissions(events)?;
    let provenance = replay_control_provenance_audit(events)?;
    let bindings = replay_control_route_audit(events)?;
    let sessions = replay_session_audit(events)?;
    let supervision = replay_supervision_audit(events)?;
    let routes = replay_routing_audit(events)?;

    let admissions_by_id = admissions
        .into_iter()
        .map(|record| (record.control.control_id, record))
        .collect::<BTreeMap<_, _>>();
    let provenance_by_control = provenance
        .into_iter()
        .map(|record| (record.provenance.control_id(), record))
        .collect::<BTreeMap<_, _>>();
    let sessions_by_id = sessions
        .iter()
        .copied()
        .map(|record| (record.session_id, record))
        .collect::<BTreeMap<_, _>>();
    let supervision_by_worker_session = supervision
        .bindings
        .iter()
        .copied()
        .map(|record| (record.binding.worker_session_id(), record))
        .collect::<BTreeMap<_, _>>();
    let routes_by_id = routes
        .into_iter()
        .map(|record| (record.request.id, record))
        .collect::<BTreeMap<_, _>>();

    bindings
        .into_iter()
        .map(|binding| {
            validate_binding(
                events,
                binding,
                &admissions_by_id,
                &provenance_by_control,
                &sessions_by_id,
                &sessions,
                &supervision_by_worker_session,
                &routes_by_id,
            )
        })
        .collect()
}

fn validate_binding(
    events: &[EventEnvelope],
    binding: ControlRouteAuditRecord,
    admissions: &BTreeMap<ControlId, ValidatedControlAdmission>,
    provenance: &BTreeMap<ControlId, ControlProvenanceAuditRecord>,
    sessions: &BTreeMap<SessionId, SessionAuditRecord>,
    session_records: &[SessionAuditRecord],
    supervision: &BTreeMap<SessionId, ControllerWorkerAuditRecord>,
    routes: &BTreeMap<RouteId, RouteAuditRecord>,
) -> Result<ValidatedOrchestrationRoute, String> {
    let control_id = binding.binding.control_id();
    let route_id = binding.binding.route_id();

    let admission = admissions.get(&control_id).ok_or_else(|| {
        format!(
            "validated orchestration route references missing validated control admission {}",
            control_id.get()
        )
    })?;
    let control = &admission.control;
    let bound_phase = validate_control_freshness_before(events, control, binding.bound_sequence)?;
    let provenance = provenance.get(&control_id).ok_or_else(|| {
        format!(
            "bound control {} has no explicit issuer provenance",
            control_id.get()
        )
    })?;
    if provenance.bound_sequence >= binding.bound_sequence {
        return Err(format!(
            "control-route binding for control {} at sequence {} precedes issuer provenance at sequence {}",
            control_id.get(),
            binding.bound_sequence,
            provenance.bound_sequence
        ));
    }

    let route = routes.get(&route_id).ok_or_else(|| {
        format!(
            "validated orchestration route references missing route {}",
            route_id.get()
        )
    })?;
    if route.request.class != RouteClass::OrchestrationControl {
        return Err(format!(
            "control {} is bound to non-orchestration route {}",
            control_id.get(),
            route_id.get()
        ));
    }

    match provenance.provenance.issuer() {
        ControlIssuer::User => Ok(ValidatedOrchestrationRoute {
            control_id,
            worker_id: control.worker_id,
            route: route.request,
            issuer: ControlIssuer::User,
            admitted_phase: admission.admitted_phase,
            bound_phase,
            controller_session_id: None,
            worker_session_id: None,
            bound_sequence: binding.bound_sequence,
        }),
        ControlIssuer::ControllerSession(controller_session_id) => {
            let worker_session = validate_controller_route(
                binding.bound_sequence,
                control,
                route,
                controller_session_id,
                sessions,
                session_records,
                supervision,
            )?;

            Ok(ValidatedOrchestrationRoute {
                control_id,
                worker_id: control.worker_id,
                route: route.request,
                issuer: ControlIssuer::ControllerSession(controller_session_id),
                admitted_phase: admission.admitted_phase,
                bound_phase,
                controller_session_id: Some(controller_session_id),
                worker_session_id: Some(worker_session.session_id),
                bound_sequence: binding.bound_sequence,
            })
        }
    }
}

fn validate_controller_route(
    binding_sequence: u64,
    control: &ControlAuditRecord,
    route: &RouteAuditRecord,
    controller_session_id: SessionId,
    sessions: &BTreeMap<SessionId, SessionAuditRecord>,
    session_records: &[SessionAuditRecord],
    supervision: &BTreeMap<SessionId, ControllerWorkerAuditRecord>,
) -> Result<SessionAuditRecord, String> {
    let worker_session =
        worker_session_before(session_records, control.worker_id, binding_sequence).ok_or_else(
            || {
                format!(
                    "controller-issued control {} targets worker {} with no active session binding before sequence {}",
                    control.control_id.get(),
                    control.worker_id.get(),
                    binding_sequence
                )
            },
        )?;

    require_before(
        "worker-session binding",
        worker_session.worker_bound_sequence,
        binding_sequence,
    )?;

    let controller_session = sessions.get(&controller_session_id).ok_or_else(|| {
        format!(
            "controller-issued control {} references missing controller session {}",
            control.control_id.get(),
            controller_session_id.get()
        )
    })?;

    let controller_endpoint = controller_session.endpoint_binding.ok_or_else(|| {
        format!(
            "controller session {} has no routing endpoint binding",
            controller_session_id.get()
        )
    })?;
    require_before(
        "controller endpoint binding",
        controller_session.endpoint_bound_sequence,
        binding_sequence,
    )?;

    let worker_endpoint = worker_session.endpoint_binding.ok_or_else(|| {
        format!(
            "worker session {} has no routing endpoint binding",
            worker_session.session_id.get()
        )
    })?;
    require_before(
        "worker endpoint binding",
        worker_session.endpoint_bound_sequence,
        binding_sequence,
    )?;

    let supervision = supervision.get(&worker_session.session_id).ok_or_else(|| {
        format!(
            "worker session {} has no controller supervision binding",
            worker_session.session_id.get()
        )
    })?;
    if supervision.binding.controller_session_id() != controller_session_id {
        return Err(format!(
            "controller issuer session {} does not supervise target worker session {}; supervisor is {}",
            controller_session_id.get(),
            worker_session.session_id.get(),
            supervision.binding.controller_session_id().get()
        ));
    }
    if supervision.bound_sequence >= binding_sequence {
        return Err(format!(
            "control-route binding at sequence {} precedes supervision topology at sequence {}",
            binding_sequence, supervision.bound_sequence
        ));
    }

    if route.request.source != controller_endpoint.endpoint_id() {
        return Err(format!(
            "controller-issued route {} source endpoint {} does not match controller session {} endpoint {}",
            route.request.id.get(),
            route.request.source.get(),
            controller_session_id.get(),
            controller_endpoint.endpoint_id().get()
        ));
    }
    if route.request.destination != worker_endpoint.endpoint_id() {
        return Err(format!(
            "controller-issued route {} destination endpoint {} does not match worker session {} endpoint {}",
            route.request.id.get(),
            route.request.destination.get(),
            worker_session.session_id.get(),
            worker_endpoint.endpoint_id().get()
        ));
    }

    Ok(worker_session)
}

fn require_before(label: &str, sequence: Option<u64>, binding_sequence: u64) -> Result<(), String> {
    let sequence = sequence.ok_or_else(|| format!("{label} is missing"))?;
    if sequence >= binding_sequence {
        return Err(format!(
            "control-route binding at sequence {binding_sequence} precedes {label} at sequence {sequence}"
        ));
    }
    Ok(())
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
    use crate::routing_audit::record_route_proposed;
    use crate::session_audit::{
        record_local_session_registered, record_session_endpoint_bound,
        record_worker_session_bound, record_worker_session_successor_bound,
    };
    use crate::supervision_audit::{
        record_controller_session_designated, record_controller_worker_bound,
    };
    use crate::worker_audit::{record_worker_goal_assigned, record_worker_transition};
    use crate::{EventStore, MemoryEventStore};
    use chatarium_core::control::WorkerControl;
    use chatarium_core::control_provenance::ControlProvenance;
    use chatarium_core::control_route::ControlRouteBinding;
    use chatarium_core::orchestration::{
        ContinuationLease, ContinuationLeaseId, WorkerAction, WorkerGoalId, WorkerLifecycle,
        WorkerPhase,
    };
    use chatarium_core::routing::{RouteEndpointId, RoutePolicy, RouteRequest};
    use chatarium_core::session::{
        SessionEndpointBinding, WorkerSessionBinding, WorkerSessionSuccessorBinding,
    };
    use chatarium_core::supervision::{ControllerDesignation, ControllerWorkerBinding};

    const CONTROLLER_SESSION: SessionId = SessionId::new(1);
    const OTHER_CONTROLLER_SESSION: SessionId = SessionId::new(2);
    const WORKER_SESSION: SessionId = SessionId::new(10);
    const WORKER_SUCCESSOR_SESSION: SessionId = SessionId::new(11);
    const WORKER: WorkerId = WorkerId::new(100);
    const LEASE: ContinuationLeaseId = ContinuationLeaseId::new(900);
    const GOAL: WorkerGoalId = WorkerGoalId::new(1000);
    const NEXT_GOAL: WorkerGoalId = WorkerGoalId::new(2000);
    const CONTROLLER_ENDPOINT: RouteEndpointId = RouteEndpointId::new(11);
    const OTHER_CONTROLLER_ENDPOINT: RouteEndpointId = RouteEndpointId::new(12);
    const WORKER_ENDPOINT: RouteEndpointId = RouteEndpointId::new(21);
    const WORKER_SUCCESSOR_ENDPOINT: RouteEndpointId = RouteEndpointId::new(22);

    fn control(id: u64) -> WorkerControl {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(GOAL).unwrap();
        lifecycle.start_or_resume(GOAL).unwrap();
        WorkerControl::stop(ControlId::new(id), WORKER, GOAL, &lifecycle).unwrap()
    }

    fn working_lifecycle() -> WorkerLifecycle {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(GOAL).unwrap();
        lifecycle.start_or_resume(GOAL).unwrap();
        lifecycle
    }

    fn record_working_lifecycle(store: &mut impl crate::EventStore) {
        record_worker_goal_assigned(store, WORKER, GOAL).unwrap();
        record_worker_transition(store, WORKER, GOAL, WorkerAction::StartOrResume).unwrap();
    }

    fn register_session(
        store: &mut impl crate::EventStore,
        session_id: SessionId,
        endpoint_id: Option<RouteEndpointId>,
    ) {
        record_local_session_registered(store, session_id).unwrap();
        if let Some(endpoint_id) = endpoint_id {
            record_session_endpoint_bound(
                store,
                SessionEndpointBinding::new(session_id, endpoint_id),
            )
            .unwrap();
        }
    }

    fn register_controller(
        store: &mut impl crate::EventStore,
        session_id: SessionId,
        endpoint_id: Option<RouteEndpointId>,
    ) {
        register_session(store, session_id, endpoint_id);
        record_controller_session_designated(store, ControllerDesignation::new(session_id))
            .unwrap();
    }

    fn register_worker(store: &mut impl crate::EventStore, endpoint_id: Option<RouteEndpointId>) {
        register_session(store, WORKER_SESSION, endpoint_id);
        record_worker_session_bound(store, WorkerSessionBinding::new(WORKER, WORKER_SESSION))
            .unwrap();
    }

    fn supervise(store: &mut impl crate::EventStore, controller_session_id: SessionId) {
        record_controller_worker_bound(
            store,
            ControllerWorkerBinding::new(controller_session_id, WORKER_SESSION).unwrap(),
        )
        .unwrap();
    }

    fn admit_with_issuer(
        store: &mut impl crate::EventStore,
        control_id: u64,
        issuer: ControlIssuer,
    ) -> WorkerControl {
        record_working_lifecycle(store);
        let control = control(control_id);
        record_worker_control_admitted(store, &control).unwrap();
        record_worker_control_issuer_bound(store, ControlProvenance::new(control.id(), issuer))
            .unwrap();
        control
    }

    fn route(id: u64, source: RouteEndpointId, destination: RouteEndpointId) -> RouteRequest {
        RouteRequest {
            id: RouteId::new(id),
            source,
            destination,
            class: RouteClass::OrchestrationControl,
        }
    }

    fn propose_and_bind(
        store: &mut impl crate::EventStore,
        control: &WorkerControl,
        request: RouteRequest,
    ) {
        record_route_proposed(store, request, RoutePolicy::RequireApproval).unwrap();
        record_control_route_bound(
            store,
            ControlRouteBinding::new(control.id(), &request).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn controller_route_validates_against_supervision_topology() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let records = replay_validated_orchestration_routes(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].issuer,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION)
        );
        assert_eq!(records[0].admitted_phase, WorkerPhase::Working);
        assert_eq!(records[0].bound_phase, WorkerPhase::Working);
        assert_eq!(records[0].controller_session_id, Some(CONTROLLER_SESSION));
        assert_eq!(records[0].worker_session_id, Some(WORKER_SESSION));
    }

    #[test]
    fn controller_routes_resolve_worker_session_at_route_binding_sequence() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, CONTROLLER_SESSION);
        record_working_lifecycle(&mut store);

        let first = control(1);
        record_worker_control_admitted(&mut store, &first).unwrap();
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(
                first.id(),
                ControlIssuer::ControllerSession(CONTROLLER_SESSION),
            ),
        )
        .unwrap();
        propose_and_bind(
            &mut store,
            &first,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        register_session(
            &mut store,
            WORKER_SUCCESSOR_SESSION,
            Some(WORKER_SUCCESSOR_ENDPOINT),
        );
        record_worker_session_successor_bound(
            &mut store,
            WorkerSessionSuccessorBinding::new(WORKER, WORKER_SESSION, WORKER_SUCCESSOR_SESSION)
                .unwrap(),
        )
        .unwrap();
        record_controller_worker_bound(
            &mut store,
            ControllerWorkerBinding::new(CONTROLLER_SESSION, WORKER_SUCCESSOR_SESSION).unwrap(),
        )
        .unwrap();

        let second = control(2);
        record_worker_control_admitted(&mut store, &second).unwrap();
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(
                second.id(),
                ControlIssuer::ControllerSession(CONTROLLER_SESSION),
            ),
        )
        .unwrap();
        propose_and_bind(
            &mut store,
            &second,
            route(2, CONTROLLER_ENDPOINT, WORKER_SUCCESSOR_ENDPOINT),
        );

        let records = replay_validated_orchestration_routes(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].worker_session_id, Some(WORKER_SESSION));
        assert_eq!(records[1].worker_session_id, Some(WORKER_SUCCESSOR_SESSION));
    }

    #[test]
    fn wrong_controller_route_source_is_rejected() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, OTHER_CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("source endpoint"));
    }

    #[test]
    fn wrong_worker_route_destination_is_rejected() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, RouteEndpointId::new(99)),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("destination endpoint"));
    }

    #[test]
    fn non_supervising_controller_issuer_is_rejected() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_controller(
            &mut store,
            OTHER_CONTROLLER_SESSION,
            Some(OTHER_CONTROLLER_ENDPOINT),
        );
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, OTHER_CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("does not supervise"));
    }

    #[test]
    fn controller_route_requires_worker_session_binding() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("no active session binding before sequence"));
    }

    #[test]
    fn controller_route_requires_controller_endpoint() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, None);
        register_worker(&mut store, Some(WORKER_ENDPOINT));
        supervise(&mut store, CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("no routing endpoint binding"));
    }

    #[test]
    fn controller_route_requires_worker_endpoint() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, Some(CONTROLLER_ENDPOINT));
        register_worker(&mut store, None);
        supervise(&mut store, CONTROLLER_SESSION);
        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("worker session"));
        assert!(error.contains("no routing endpoint binding"));
    }

    #[test]
    fn endpoint_topology_recorded_after_control_route_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        register_controller(&mut store, CONTROLLER_SESSION, None);
        register_worker(&mut store, None);
        supervise(&mut store, CONTROLLER_SESSION);

        let control = admit_with_issuer(
            &mut store,
            1,
            ControlIssuer::ControllerSession(CONTROLLER_SESSION),
        );
        propose_and_bind(
            &mut store,
            &control,
            route(1, CONTROLLER_ENDPOINT, WORKER_ENDPOINT),
        );

        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(CONTROLLER_SESSION, CONTROLLER_ENDPOINT),
        )
        .unwrap();
        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(WORKER_SESSION, WORKER_ENDPOINT),
        )
        .unwrap();

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(
            error.contains("precedes controller endpoint binding")
                || error.contains("precedes worker endpoint binding")
        );
    }

    #[test]
    fn user_issued_bound_route_does_not_invent_user_endpoint() {
        let mut store = MemoryEventStore::default();
        let control = admit_with_issuer(&mut store, 1, ControlIssuer::User);
        propose_and_bind(
            &mut store,
            &control,
            route(1, RouteEndpointId::new(500), RouteEndpointId::new(600)),
        );

        let records = replay_validated_orchestration_routes(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].issuer, ControlIssuer::User);
        assert_eq!(records[0].controller_session_id, None);
        assert_eq!(records[0].worker_session_id, None);
    }

    #[test]
    fn control_valid_at_admission_but_stale_after_goal_replacement_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = admit_with_issuer(&mut store, 1, ControlIssuer::User);

        record_worker_transition(&mut store, WORKER, GOAL, WorkerAction::Complete).unwrap();
        record_worker_goal_assigned(&mut store, WORKER, NEXT_GOAL).unwrap();
        record_worker_transition(&mut store, WORKER, NEXT_GOAL, WorkerAction::StartOrResume)
            .unwrap();

        propose_and_bind(
            &mut store,
            &control,
            route(1, RouteEndpointId::new(500), RouteEndpointId::new(600)),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("stale worker control"));
    }

    #[test]
    fn continue_that_enters_needs_input_before_route_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        record_working_lifecycle(&mut store);

        let lifecycle = working_lifecycle();
        let mut lease = ContinuationLease::new(LEASE, WORKER, GOAL, 1);
        record_continuation_lease_created(&mut store, &lease).unwrap();
        let permit = lease.authorize(&lifecycle).unwrap();
        record_continuation_permit_issued(&mut store, &permit).unwrap();
        let control =
            WorkerControl::continue_work(ControlId::new(1), WORKER, &lifecycle, permit).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::User),
        )
        .unwrap();

        record_worker_transition(&mut store, WORKER, GOAL, WorkerAction::RequestInput).unwrap();
        propose_and_bind(
            &mut store,
            &control,
            route(1, RouteEndpointId::new(500), RouteEndpointId::new(600)),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("Continue"));
        assert!(error.contains("NeedsInput"));
    }

    #[test]
    fn status_request_remains_fresh_when_same_goal_becomes_terminal() {
        let mut store = MemoryEventStore::default();
        record_working_lifecycle(&mut store);

        let lifecycle = working_lifecycle();
        let control =
            WorkerControl::status_request(ControlId::new(1), WORKER, GOAL, &lifecycle).unwrap();
        record_worker_control_admitted(&mut store, &control).unwrap();
        record_worker_control_issuer_bound(
            &mut store,
            ControlProvenance::new(control.id(), ControlIssuer::User),
        )
        .unwrap();

        record_worker_transition(&mut store, WORKER, GOAL, WorkerAction::Complete).unwrap();
        propose_and_bind(
            &mut store,
            &control,
            route(1, RouteEndpointId::new(500), RouteEndpointId::new(600)),
        );

        let records = replay_validated_orchestration_routes(store.events()).unwrap();
        assert_eq!(records[0].admitted_phase, WorkerPhase::Working);
        assert_eq!(records[0].bound_phase, WorkerPhase::Completed);
    }

    #[test]
    fn bound_control_without_issuer_provenance_fails_closed() {
        let mut store = MemoryEventStore::default();
        record_working_lifecycle(&mut store);
        let control = control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();
        propose_and_bind(
            &mut store,
            &control,
            route(1, RouteEndpointId::new(500), RouteEndpointId::new(600)),
        );

        let error = replay_validated_orchestration_routes(store.events()).unwrap_err();
        assert!(error.contains("no explicit issuer provenance"));
    }
}
