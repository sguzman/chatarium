//! Read-only result projection for controller-issued worker controls.
//!
//! Worker-side result audits remain authoritative. This module joins those
//! existing durable facts to the validated controller-issued route and, when
//! available, to the local conversation that owned the controller session.
//! It creates no acknowledgement, route, lifecycle transition, context item,
//! or journal event.

use crate::EventEnvelope;
use crate::chat_container_audit::replay_chat_container_audit;
use crate::continuation_execution_audit::{
    ContinuationExecutionOutcome, continuation_execution_output_text,
    replay_worker_continuation_execution_audit,
};
use crate::control_action_audit::replay_worker_control_action_audit;
use crate::control_result_audit::replay_worker_control_status_results;
use crate::local_conversation_chat_container_audit::{
    replay_local_conversation_chat_container_bindings,
};
use crate::orchestration_route_audit::{
    ValidatedOrchestrationRoute, replay_validated_orchestration_routes,
};
use chatarium_core::LocalConversationId;
use chatarium_core::control::{ControlId, WorkerControlKind};
use chatarium_core::control_provenance::ControlIssuer;
use chatarium_core::orchestration::{
    ContinuationLeaseId, WorkerGoalId, WorkerId, WorkerPhase,
};
use chatarium_core::routing::RouteId;
use chatarium_core::session::SessionId;
use chatarium_core::LocalTurnId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerWorkerResultDetail {
    Status {
        phase: WorkerPhase,
        acknowledged_sequence: u64,
    },
    Action {
        kind: WorkerControlKind,
        from_phase: WorkerPhase,
        resulting_phase: WorkerPhase,
        started_sequence: u64,
        lifecycle_sequence: u64,
    },
    Continuation {
        outcome: ContinuationExecutionOutcome,
        execution_turn_id: LocalTurnId,
        lease_id: ContinuationLeaseId,
        permit_ordinal: u32,
        started_sequence: u64,
        terminal_sequence: u64,
        output_text: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerWorkerResultItem {
    pub control_id: ControlId,
    pub route_id: RouteId,
    pub worker_id: WorkerId,
    pub worker_conversation_id: LocalConversationId,
    pub controller_session_id: SessionId,
    pub controller_conversation_id: Option<LocalConversationId>,
    pub goal_id: WorkerGoalId,
    pub result_sequence: u64,
    pub detail: ControllerWorkerResultDetail,
}

#[derive(Debug, Clone, Copy)]
struct LocalSessionOwner {
    conversation_id: LocalConversationId,
    conversation_bound_sequence: u64,
    session_joined_sequence: u64,
}

/// Replay all terminal worker results whose durable issuer is a controller.
///
/// Controller-issued results remain valid even when the controller session is
/// not owned by a local Chatarium conversation; in that case
/// `controller_conversation_id` is None. Local desktop callers can use
/// `replay_controller_worker_results_for_conversation`.
pub fn replay_controller_worker_results(
    events: &[EventEnvelope],
) -> Result<Vec<ControllerWorkerResultItem>, String> {
    let routes = replay_validated_orchestration_routes(events)?;
    let routes_by_id = routes
        .into_iter()
        .map(|record| (record.route.id, record))
        .collect::<BTreeMap<_, _>>();
    let session_owners = local_session_owners(events)?;

    let mut items = Vec::new();

    for result in replay_worker_control_status_results(events)? {
        let Some((route, controller_session_id)) =
            controller_route(&routes_by_id, result.route_id, result.control_id, result.worker_id)?
        else {
            continue;
        };
        items.push(ControllerWorkerResultItem {
            control_id: result.control_id,
            route_id: result.route_id,
            worker_id: result.worker_id,
            worker_conversation_id: result.worker_conversation_id,
            controller_session_id,
            controller_conversation_id: local_owner_for_route(
                &session_owners,
                controller_session_id,
                route.bound_sequence,
            ),
            goal_id: result.goal_id,
            result_sequence: result.recorded_sequence,
            detail: ControllerWorkerResultDetail::Status {
                phase: result.phase,
                acknowledged_sequence: result.acknowledged_sequence,
            },
        });
    }

    for action in replay_worker_control_action_audit(events)? {
        let Some(result_sequence) = action.result_sequence else {
            continue;
        };
        let resulting_phase = action.resulting_phase.ok_or_else(|| {
            format!(
                "completed worker control action route {} is missing resulting phase",
                action.route_id.get()
            )
        })?;
        let lifecycle_sequence = action.lifecycle_sequence.ok_or_else(|| {
            format!(
                "completed worker control action route {} is missing lifecycle sequence",
                action.route_id.get()
            )
        })?;
        let Some((route, controller_session_id)) =
            controller_route(&routes_by_id, action.route_id, action.control_id, action.worker_id)?
        else {
            continue;
        };
        items.push(ControllerWorkerResultItem {
            control_id: action.control_id,
            route_id: action.route_id,
            worker_id: action.worker_id,
            worker_conversation_id: action.worker_conversation_id,
            controller_session_id,
            controller_conversation_id: local_owner_for_route(
                &session_owners,
                controller_session_id,
                route.bound_sequence,
            ),
            goal_id: action.goal_id,
            result_sequence,
            detail: ControllerWorkerResultDetail::Action {
                kind: action.kind,
                from_phase: action.from_phase,
                resulting_phase,
                started_sequence: action.started_sequence,
                lifecycle_sequence,
            },
        });
    }

    for execution in replay_worker_continuation_execution_audit(events)? {
        let Some(result_sequence) = execution.result_sequence else {
            continue;
        };
        let outcome = execution.outcome.ok_or_else(|| {
            format!(
                "completed continuation route {} is missing terminal outcome",
                execution.route_id.get()
            )
        })?;
        let terminal_sequence = execution.terminal_sequence.ok_or_else(|| {
            format!(
                "completed continuation route {} is missing terminal sequence",
                execution.route_id.get()
            )
        })?;
        let Some((route, controller_session_id)) = controller_route(
            &routes_by_id,
            execution.route_id,
            execution.control_id,
            execution.worker_id,
        )?
        else {
            continue;
        };
        items.push(ControllerWorkerResultItem {
            control_id: execution.control_id,
            route_id: execution.route_id,
            worker_id: execution.worker_id,
            worker_conversation_id: execution.worker_conversation_id,
            controller_session_id,
            controller_conversation_id: local_owner_for_route(
                &session_owners,
                controller_session_id,
                route.bound_sequence,
            ),
            goal_id: execution.goal_id,
            result_sequence,
            detail: ControllerWorkerResultDetail::Continuation {
                outcome,
                execution_turn_id: execution.execution_turn_id,
                lease_id: execution.lease_id,
                permit_ordinal: execution.permit_ordinal,
                started_sequence: execution.started_sequence,
                terminal_sequence,
                output_text: continuation_execution_output_text(
                    events,
                    execution.execution_turn_id,
                )?,
            },
        });
    }

    items.sort_by_key(|item| item.result_sequence);
    Ok(items)
}

pub fn replay_controller_worker_results_for_conversation(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<ControllerWorkerResultItem>, String> {
    Ok(replay_controller_worker_results(events)?
        .into_iter()
        .filter(|item| item.controller_conversation_id == Some(conversation_id))
        .collect())
}

fn controller_route<'a>(
    routes: &'a BTreeMap<RouteId, ValidatedOrchestrationRoute>,
    route_id: RouteId,
    control_id: ControlId,
    worker_id: WorkerId,
) -> Result<Option<(&'a ValidatedOrchestrationRoute, SessionId)>, String> {
    let route = routes.get(&route_id).ok_or_else(|| {
        format!(
            "worker result route {} has no validated orchestration route",
            route_id.get()
        )
    })?;
    if route.control_id != control_id || route.worker_id != worker_id {
        return Err(format!(
            "worker result route {} disagrees with validated control/worker provenance",
            route_id.get()
        ));
    }

    match route.issuer {
        ControlIssuer::User => Ok(None),
        ControlIssuer::ControllerSession(controller_session_id) => {
            if route.controller_session_id != Some(controller_session_id) {
                return Err(format!(
                    "validated route {} controller session provenance is inconsistent",
                    route_id.get()
                ));
            }
            Ok(Some((route, controller_session_id)))
        }
    }
}

fn local_session_owners(
    events: &[EventEnvelope],
) -> Result<BTreeMap<SessionId, LocalSessionOwner>, String> {
    let containers = replay_chat_container_audit(events)?
        .into_iter()
        .map(|record| (record.container_id, record))
        .collect::<BTreeMap<_, _>>();
    let mut owners = BTreeMap::new();

    for binding in replay_local_conversation_chat_container_bindings(events)? {
        let container = containers.get(&binding.container_id).ok_or_else(|| {
            format!(
                "local conversation container {} disappeared while projecting controller results",
                binding.container_id.get()
            )
        })?;
        for session in &container.sessions {
            if let Some(existing) = owners.insert(
                session.session_id,
                LocalSessionOwner {
                    conversation_id: binding.conversation_id,
                    conversation_bound_sequence: binding.bound_sequence,
                    session_joined_sequence: session.joined_sequence,
                },
            ) {
                return Err(format!(
                    "session {} maps to multiple local conversations {} and {}",
                    session.session_id.get(),
                    existing.conversation_id,
                    binding.conversation_id
                ));
            }
        }
    }

    Ok(owners)
}

fn local_owner_for_route(
    owners: &BTreeMap<SessionId, LocalSessionOwner>,
    controller_session_id: SessionId,
    route_bound_sequence: u64,
) -> Option<LocalConversationId> {
    owners.get(&controller_session_id).and_then(|owner| {
        (owner.conversation_bound_sequence < route_bound_sequence
            && owner.session_joined_sequence < route_bound_sequence)
            .then_some(owner.conversation_id)
    })
}
