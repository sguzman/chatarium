//! Feature-specific routing completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_routing_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::OrchestrationTopologyInitialized { appended_events } => {
                let count = appended_events.len();
                self.events.extend(appended_events);
                self.topology_command_pending = false;
                self.status = format!(
                    "local orchestration topology durably initialized · {count} events"
                );
            }
            PersistNotice::RouteEndpointBound { event } => {
                self.events.push(event);
                self.route_addressability_command_pending = false;
                self.status = "current session routing endpoint durably bound".to_owned();
            }
            PersistNotice::LocalRoutePolicyEventAppended { event } => {
                let kind = event.kind.stable_name();
                self.events.push(event);
                self.route_policy_command_pending = false;
                self.status = format!("local route policy durably updated · {kind}");
            }
            PersistNotice::LocalRoutePayloadAttached { route_id, event } => {
                self.events.push(event);
                self.route_payload_command_pending = false;
                self.route_payload_drafts.remove(&route_id);
                self.status =
                    format!("local route {} payload durably attached", route_id.get());
            }
            PersistNotice::LocalRouteDispatchUpdated {
                route_id,
                appended_events,
            } => {
                let count = appended_events.len();
                self.events.extend(appended_events);
                self.route_dispatch_command_pending = false;
                self.status = format!(
                    "local route {} dispatch/delivery durably advanced · {} event{}",
                    route_id.get(),
                    count,
                    if count == 1 { "" } else { "s" },
                );
            }
            PersistNotice::LocalRouteContextDecisionUpdated { route_id, event } => {
                self.events.push(event);
                self.route_context_command_pending = false;
                self.status = format!(
                    "local route {} context eligibility durably updated",
                    route_id.get()
                );
            }
            _ => unreachable!("unexpected routing notice"),
        }
    }
}
