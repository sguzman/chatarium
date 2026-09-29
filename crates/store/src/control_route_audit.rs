//! Durable correlation between admitted worker controls and orchestration routes.
//!
//! A binding is provenance only. It does not imply policy approval, dispatch,
//! delivery, execution, or worker lifecycle change.

use crate::control_audit::replay_control_audit;
use crate::routing_audit::replay_routing_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::control::ControlId;
use chatarium_core::control_route::{ControlRouteBinding, ControlRouteBindingError};
use chatarium_core::routing::RouteId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CONTROL_ROUTE_SCHEMA: &str = "chatarium-control-route-audit";
const CONTROL_ROUTE_VERSION: u64 = 1;

/// Restart-replayable durable correlation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRouteAuditRecord {
    /// Typed control/route correlation.
    pub binding: ControlRouteBinding,
    /// Durable sequence where the correlation was recorded.
    pub bound_sequence: u64,
}

/// Append one typed control-route correlation.
///
/// Callers should construct the binding through ControlRouteBinding::new so
/// non-orchestration routes cannot be persisted through the normal API.
pub fn record_control_route_bound(
    store: &mut impl EventStore,
    binding: ControlRouteBinding,
) -> std::io::Result<u64> {
    let payload = json!({
        "schema": CONTROL_ROUTE_SCHEMA,
        "version": CONTROL_ROUTE_VERSION,
        "record": "control_route_bound",
        "control_id": binding.control_id().get(),
        "route_id": binding.route_id().get(),
    });

    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(
        Some(control_route_scope(
            binding.control_id(),
            binding.route_id(),
        )),
        EventKind::ControlRouteBound,
        encoded,
    )
}

/// Replay durable control-route bindings against already-durable control/route facts.
///
/// Replay is strict: one control binds to at most one route and one route carries
/// at most one control.
pub fn replay_control_route_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ControlRouteAuditRecord>, String> {
    let controls = replay_control_audit(events)?;
    let routes = replay_routing_audit(events)?;

    let controls_by_id = controls
        .into_iter()
        .map(|record| (record.control_id, record))
        .collect::<BTreeMap<_, _>>();
    let routes_by_id = routes
        .into_iter()
        .map(|record| (record.request.id, record))
        .collect::<BTreeMap<_, _>>();

    let mut by_control = BTreeMap::<ControlId, RouteId>::new();
    let mut by_route = BTreeMap::<RouteId, ControlId>::new();
    let mut records = Vec::new();

    for event in events {
        if event.kind != EventKind::ControlRouteBound {
            continue;
        }

        let payload = typed_payload(event)?;
        let control_id = ControlId::new(required_u64(&payload, "control_id")?);
        let route_id = RouteId::new(required_u64(&payload, "route_id")?);
        validate_scope(event, control_id, route_id)?;

        let control = controls_by_id.get(&control_id).ok_or_else(|| {
            format!(
                "control-route binding at sequence {} references missing admitted control {}",
                event.sequence,
                control_id.get()
            )
        })?;
        if control.admitted_sequence >= event.sequence {
            return Err(format!(
                "control-route binding at sequence {} precedes durable admission of control {} at sequence {}",
                event.sequence,
                control_id.get(),
                control.admitted_sequence
            ));
        }

        let route = routes_by_id.get(&route_id).ok_or_else(|| {
            format!(
                "control-route binding at sequence {} references missing route proposal {}",
                event.sequence,
                route_id.get()
            )
        })?;
        if route.proposed_sequence >= event.sequence {
            return Err(format!(
                "control-route binding at sequence {} precedes durable proposal of route {} at sequence {}",
                event.sequence,
                route_id.get(),
                route.proposed_sequence
            ));
        }

        let binding = ControlRouteBinding::new(control_id, &route.request)
            .map_err(|error| binding_error(event.sequence, error))?;

        if let Some(existing_route) = by_control.get(&control_id) {
            return Err(format!(
                "control {} is already bound to route {}; cannot also bind route {} at sequence {}",
                control_id.get(),
                existing_route.get(),
                route_id.get(),
                event.sequence
            ));
        }
        if let Some(existing_control) = by_route.get(&route_id) {
            return Err(format!(
                "route {} is already bound to control {}; cannot also bind control {} at sequence {}",
                route_id.get(),
                existing_control.get(),
                control_id.get(),
                event.sequence
            ));
        }

        by_control.insert(control_id, route_id);
        by_route.insert(route_id, control_id);
        records.push(ControlRouteAuditRecord {
            binding,
            bound_sequence: event.sequence,
        });
    }

    Ok(records)
}

/// Stable durable scope for one control-route correlation.
#[must_use]
pub fn control_route_scope(control_id: ControlId, route_id: RouteId) -> String {
    format!("control-route:{}:{}", control_id.get(), route_id.get())
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed typed control-route payload at sequence {}: {error}",
            event.sequence
        )
    })?;

    if value.get("schema").and_then(Value::as_str) != Some(CONTROL_ROUTE_SCHEMA) {
        return Err(format!(
            "control-route event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }

    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "control-route event at sequence {} is missing integer field 'version'",
                event.sequence
            )
        })?;
    if version != CONTROL_ROUTE_VERSION {
        return Err(format!(
            "unsupported control-route payload version {version} at sequence {}",
            event.sequence
        ));
    }

    if required_string(&value, "record")? != "control_route_bound" {
        return Err(format!(
            "control-route event at sequence {} is not a control_route_bound record",
            event.sequence
        ));
    }

    Ok(value)
}

fn validate_scope(
    event: &EventEnvelope,
    control_id: ControlId,
    route_id: RouteId,
) -> Result<(), String> {
    let expected = control_route_scope(control_id, route_id);
    if event.scope.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "control-route event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed control-route payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed control-route payload is missing string field '{field}'"))
}

fn binding_error(sequence: u64, error: ControlRouteBindingError) -> String {
    format!("invalid control-route binding at sequence {sequence}: {error:?}")
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_audit::record_worker_control_admitted;
    use crate::projection::SqliteProjection;
    use crate::routing_audit::record_route_proposed;
    use crate::worker_audit::replay_worker_audit;
    use crate::{JsonlEventStore, MemoryEventStore};
    use chatarium_core::control::WorkerControl;
    use chatarium_core::orchestration::{WorkerGoalId, WorkerId, WorkerLifecycle};
    use chatarium_core::routing::{
        RouteClass, RouteEndpointId, RouteGateState, RoutePolicy, RouteRequest,
    };
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    const W1: WorkerId = WorkerId::new(10);
    const G1: WorkerGoalId = WorkerGoalId::new(100);
    const SOURCE: RouteEndpointId = RouteEndpointId::new(20);
    const DESTINATION: RouteEndpointId = RouteEndpointId::new(30);

    fn temp_path(label: &str, extension: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-control-route-{label}-{}-{nonce}.{extension}",
            std::process::id()
        ))
    }

    fn admitted_control(id: u64) -> WorkerControl {
        let mut lifecycle = WorkerLifecycle::default();
        lifecycle.assign_goal(G1).unwrap();
        lifecycle.start_or_resume(G1).unwrap();
        WorkerControl::stop(ControlId::new(id), W1, G1, &lifecycle).unwrap()
    }

    fn route(id: u64, class: RouteClass) -> RouteRequest {
        RouteRequest {
            id: RouteId::new(id),
            source: SOURCE,
            destination: DESTINATION,
            class,
        }
    }

    fn append_raw_binding(
        store: &mut impl EventStore,
        control_id: ControlId,
        route_id: RouteId,
    ) {
        store
            .append_scoped(
                Some(control_route_scope(control_id, route_id)),
                EventKind::ControlRouteBound,
                json!({
                    "schema": CONTROL_ROUTE_SCHEMA,
                    "version": CONTROL_ROUTE_VERSION,
                    "record": "control_route_bound",
                    "control_id": control_id.get(),
                    "route_id": route_id.get(),
                })
                .to_string(),
            )
            .unwrap();
    }

    fn record_valid_pair(
        store: &mut impl EventStore,
        control_id: u64,
        route_id: u64,
    ) -> ControlRouteBinding {
        let control = admitted_control(control_id);
        record_worker_control_admitted(store, &control).unwrap();

        let request = route(route_id, RouteClass::OrchestrationControl);
        record_route_proposed(store, request, RoutePolicy::RequireApproval).unwrap();

        let binding = ControlRouteBinding::new(control.id(), &request).unwrap();
        record_control_route_bound(store, binding).unwrap();
        binding
    }

    #[test]
    fn valid_binding_survives_reopen() {
        let path = temp_path("valid", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            record_valid_pair(&mut store, 1, 11);
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        let records = replay_control_route_audit(reopened.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].binding.control_id(), ControlId::new(1));
        assert_eq!(records[0].binding.route_id(), RouteId::new(11));
        assert_eq!(records[0].bound_sequence, 3);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn non_orchestration_routes_are_rejected_by_replay() {
        for class in [RouteClass::SessionMessage, RouteClass::ToolCall] {
            let mut store = MemoryEventStore::default();
            let control = admitted_control(1);
            record_worker_control_admitted(&mut store, &control).unwrap();
            let request = route(11, class);
            record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
            append_raw_binding(&mut store, control.id(), request.id);

            let error = replay_control_route_audit(store.events()).unwrap_err();
            assert!(error.contains("WrongRouteClass"));
        }
    }

    #[test]
    fn missing_admitted_control_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(11, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        append_raw_binding(&mut store, ControlId::new(1), request.id);

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("missing admitted control"));
    }

    #[test]
    fn missing_route_proposal_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = admitted_control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();
        append_raw_binding(&mut store, control.id(), RouteId::new(11));

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("missing route proposal"));
    }

    #[test]
    fn binding_before_control_admission_is_rejected() {
        let mut store = MemoryEventStore::default();
        let request = route(11, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        append_raw_binding(&mut store, ControlId::new(1), request.id);
        let control = admitted_control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("precedes durable admission"));
    }

    #[test]
    fn binding_before_route_proposal_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = admitted_control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();
        append_raw_binding(&mut store, control.id(), RouteId::new(11));
        let request = route(11, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("precedes durable proposal"));
    }

    #[test]
    fn one_control_cannot_bind_to_two_routes() {
        let mut store = MemoryEventStore::default();
        let control = admitted_control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();

        let first = route(11, RouteClass::OrchestrationControl);
        let second = route(12, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, first, RoutePolicy::Allow).unwrap();
        record_route_proposed(&mut store, second, RoutePolicy::Allow).unwrap();
        append_raw_binding(&mut store, control.id(), first.id);
        append_raw_binding(&mut store, control.id(), second.id);

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to route"));
    }

    #[test]
    fn one_route_cannot_bind_to_two_controls() {
        let mut store = MemoryEventStore::default();
        let first = admitted_control(1);
        let second = admitted_control(2);
        record_worker_control_admitted(&mut store, &first).unwrap();
        record_worker_control_admitted(&mut store, &second).unwrap();

        let request = route(11, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        append_raw_binding(&mut store, first.id(), request.id);
        append_raw_binding(&mut store, second.id(), request.id);

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to control"));
    }

    #[test]
    fn duplicate_binding_is_rejected() {
        let mut store = MemoryEventStore::default();
        let binding = record_valid_pair(&mut store, 1, 11);
        append_raw_binding(&mut store, binding.control_id(), binding.route_id());

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("already bound to route"));
    }

    #[test]
    fn torn_tail_cannot_fabricate_binding() {
        let path = temp_path("torn-tail", "jsonl");
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            let control = admitted_control(1);
            record_worker_control_admitted(&mut store, &control).unwrap();
            let request = route(11, RouteClass::OrchestrationControl);
            record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();
        }
        {
            let mut raw = OpenOptions::new().append(true).open(&path).unwrap();
            raw.write_all(br#"{"v":2,"sequence":3,"kind":"control_route_bound""#)
                .unwrap();
            raw.sync_data().unwrap();
        }

        let reopened = JsonlEventStore::open(&path).unwrap();
        assert!(replay_control_route_audit(reopened.events()).unwrap().is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_complete_binding_is_hard_replay_error() {
        let mut store = MemoryEventStore::default();
        store
            .append_scoped(
                Some(control_route_scope(ControlId::new(1), RouteId::new(11))),
                EventKind::ControlRouteBound,
                json!({
                    "schema": CONTROL_ROUTE_SCHEMA,
                    "version": CONTROL_ROUTE_VERSION,
                    "record": "control_route_bound",
                    "control_id": 1,
                })
                .to_string(),
            )
            .unwrap();

        assert!(replay_control_route_audit(store.events()).is_err());
    }

    #[test]
    fn scope_mismatch_is_rejected() {
        let mut store = MemoryEventStore::default();
        let control = admitted_control(1);
        record_worker_control_admitted(&mut store, &control).unwrap();
        let request = route(11, RouteClass::OrchestrationControl);
        record_route_proposed(&mut store, request, RoutePolicy::Allow).unwrap();

        store
            .append_scoped(
                Some(control_route_scope(ControlId::new(99), request.id)),
                EventKind::ControlRouteBound,
                json!({
                    "schema": CONTROL_ROUTE_SCHEMA,
                    "version": CONTROL_ROUTE_VERSION,
                    "record": "control_route_bound",
                    "control_id": control.id().get(),
                    "route_id": request.id.get(),
                })
                .to_string(),
            )
            .unwrap();

        let error = replay_control_route_audit(store.events()).unwrap_err();
        assert!(error.contains("scope"));
    }

    #[test]
    fn independent_bindings_preserve_journal_order() {
        let mut store = MemoryEventStore::default();
        record_valid_pair(&mut store, 1, 11);
        record_valid_pair(&mut store, 2, 12);

        let records = replay_control_route_audit(store.events()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].binding.control_id(), ControlId::new(1));
        assert_eq!(records[1].binding.control_id(), ControlId::new(2));
        assert!(records[0].bound_sequence < records[1].bound_sequence);
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "unrelated".to_owned())
            .unwrap();
        assert!(replay_control_route_audit(store.events()).unwrap().is_empty());
    }

    #[test]
    fn generic_sqlite_projection_carries_binding_without_schema_change() {
        let path = temp_path("projection", "sqlite");
        let mut store = MemoryEventStore::default();
        record_valid_pair(&mut store, 1, 11);

        let mut projection = SqliteProjection::open(&path).unwrap();
        projection.rebuild(store.events()).unwrap();

        let projected = projection.events_of_kind(EventKind::ControlRouteBound).unwrap();
        assert_eq!(projected.len(), 1);

        drop(projection);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn binding_replay_does_not_mutate_worker_or_route_state() {
        let mut store = MemoryEventStore::default();
        record_valid_pair(&mut store, 1, 11);

        let bindings = replay_control_route_audit(store.events()).unwrap();
        assert_eq!(bindings.len(), 1);

        let workers = replay_worker_audit(store.events()).unwrap();
        assert!(workers.is_empty());

        let routes = replay_routing_audit(store.events()).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].gate_state, RouteGateState::PendingApproval);
        assert_eq!(routes[0].dispatch_sequence, None);
    }
}
