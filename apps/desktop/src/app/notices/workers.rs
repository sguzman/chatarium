//! Feature-specific workers completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_workers_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::LifecycleEventAppended { event } => {
                let kind = event.kind.stable_name();
                self.events.push(event);
                self.lifecycle_command_pending = false;
                self.status = format!("worker lifecycle durably updated · {kind}");
            }
            PersistNotice::SupervisionEventAppended { event } => {
                let kind = event.kind.stable_name();
                self.events.push(event);
                self.supervision_command_pending = false;
                self.status = format!("local supervision durably updated · {kind}");
            }
            PersistNotice::ControllerContinuationLeaseCreated { worker_id, event } => {
                self.events.push(event);
                self.controller_control_command_pending = false;
                self.status = format!(
                    "bounded continuation authority created for worker {}",
                    worker_id.get()
                );
            }
            PersistNotice::ControllerControlProposed {
                control_id,
                route_id,
                appended_events,
            } => {
                self.events.extend(appended_events);
                self.controller_control_command_pending = false;
                self.status = format!(
                    "controller control {} durably proposed on route {} · awaiting explicit approval",
                    control_id.get(),
                    route_id.get()
                );
            }
            PersistNotice::ControllerControlRoutePolicyUpdated { route_id, event } => {
                self.events.push(event);
                self.controller_control_command_pending = false;
                self.status = format!(
                    "controller control route {} policy durably updated",
                    route_id.get()
                );
            }
            PersistNotice::ControllerControlDispatched {
                route_id,
                appended_events,
            } => {
                let count = appended_events.len();
                self.events.extend(appended_events);
                self.controller_control_command_pending = false;
                self.status = format!(
                    "controller control route {} dispatch/delivery durably advanced · {} event{}",
                    route_id.get(),
                    count,
                    if count == 1 { "" } else { "s" },
                );
            }
            PersistNotice::ControllerWorkerResultContextDecisionUpdated { route_id, event } => {
                self.events.push(event);
                self.controller_result_context_command_pending = false;
                self.status = format!(
                    "controller worker result route {} context eligibility durably updated",
                    route_id.get()
                );
            }
            PersistNotice::ControllerCoordinationStarted { turn_id, event } => {
                self.events.push(event);
                self.controller_coordination_command_pending = false;
                self.status =
                    format!("controller coordination turn {} durably started", turn_id);
            }
            PersistNotice::ControllerCoordinationResultRecorded { turn_id, event } => {
                self.events.push(event);
                self.status = format!(
                    "controller coordination turn {} terminal result durably recorded",
                    turn_id
                );
            }
            PersistNotice::ControllerCoordinationResultContextDecisionUpdated {
                turn_id,
                event,
            } => {
                self.events.push(event);
                self.controller_coordination_result_context_command_pending = false;
                self.status = format!(
                    "controller coordination turn {} result context eligibility durably updated",
                    turn_id
                );
            }
            PersistNotice::ControllerCoordinationSuggestionRecorded {
                suggestion_id,
                event,
            } => {
                self.events.push(event);
                self.controller_coordination_suggestion_command_pending = false;
                self.status = format!(
                    "coordination suggestion {} durably recorded · no control authority created",
                    suggestion_id.get()
                );
            }
            PersistNotice::ControllerCoordinationSuggestionPromoted {
                suggestion_id,
                control_id,
                route_id,
                appended_events,
            } => {
                self.events.extend(appended_events);
                self.controller_coordination_suggestion_command_pending = false;
                self.controller_control_command_pending = false;
                self.status = format!(
                    "suggestion {} promoted to control {} on route {} · still awaiting explicit route approval",
                    suggestion_id.get(),
                    control_id.get(),
                    route_id.get(),
                );
            }
            PersistNotice::WorkerControlAcknowledged { route_id, event } => {
                self.events.push(event);
                self.worker_control_command_pending = false;
                self.status = format!(
                    "worker control route {} durably acknowledged",
                    route_id.get()
                );
            }
            PersistNotice::WorkerControlStatusResultRecorded { route_id, event } => {
                self.events.push(event);
                self.worker_control_command_pending = false;
                self.status = format!(
                    "worker control route {} status result durably recorded",
                    route_id.get()
                );
            }
            PersistNotice::WorkerControlActionAdvanced {
                route_id,
                appended_events,
            } => {
                let count = appended_events.len();
                self.events.extend(appended_events);
                self.worker_control_command_pending = false;
                self.status = format!(
                    "worker control route {} action durably advanced · {} event{}",
                    route_id.get(),
                    count,
                    if count == 1 { "" } else { "s" },
                );
            }
            PersistNotice::WorkerContinuationExecutionStarted { route_id, event } => {
                self.events.push(event);
                self.worker_control_command_pending = false;
                self.status = format!(
                    "worker control route {} continuation execution durably started",
                    route_id.get()
                );
            }
            PersistNotice::WorkerContinuationExecutionResultRecorded { route_id, event } => {
                self.events.push(event);
                self.status = format!(
                    "worker control route {} continuation execution result durably recorded",
                    route_id.get()
                );
            }
            _ => unreachable!("unexpected workers notice"),
        }
    }
}
