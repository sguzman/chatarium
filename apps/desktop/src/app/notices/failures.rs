//! Feature-specific failures completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_failures_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::Failed {
                operation,
                revision,
                request_id,
                turn_id,
                error,
            } => {
                if operation.starts_with("orchestration topology") {
                    self.topology_command_pending = false;
                }
                if operation.starts_with("route addressability") {
                    self.route_addressability_command_pending = false;
                }
                if operation.starts_with("local route payload") {
                    self.route_payload_command_pending = false;
                } else if operation.starts_with("local route dispatch") {
                    self.route_dispatch_command_pending = false;
                } else if operation.starts_with("local route context") {
                    self.route_context_command_pending = false;
                } else if operation.starts_with("local route ") {
                    self.route_policy_command_pending = false;
                }
                // Local HTTP MCP preflight, worker spawn and outcome append
                // failures must release the UI pending flag too. An accepted
                // dispatch may have executed: never retry it implicitly.
                if operation.starts_with("tool ")
                    || operation.starts_with("agent MCP")
                    || operation.starts_with("localhost MCP")
                {
                    self.tool_command_pending = false;
                    if self.agent_mcp_flow.is_some() {
                        self.agent_mcp_flow = None;
                    }
                }
                if operation == "OpenViking summary persistence" {
                    if let Some(attempt) = self.openviking_summary_in_flight.take() {
                        self.openviking_summary_blocked.insert(attempt.request_id);
                    }
                }
                if operation.starts_with("local memory ") {
                    self.local_memory_command_pending = false;
                }
                if operation.starts_with("lifecycle ") {
                    self.lifecycle_command_pending = false;
                }
                if operation.starts_with("supervision ") {
                    self.supervision_command_pending = false;
                }
                if operation.starts_with("controller control ") {
                    self.controller_control_command_pending = false;
                }
                if operation.starts_with("controller result context") {
                    self.controller_result_context_command_pending = false;
                }
                if operation.starts_with("controller coordination result context") {
                    self.controller_coordination_result_context_command_pending = false;
                } else if operation.starts_with("controller coordination suggestion") {
                    self.controller_coordination_suggestion_command_pending = false;
                    if operation.starts_with("controller coordination suggestion promotion") {
                        self.controller_control_command_pending = false;
                    }
                } else if operation.starts_with("controller coordination") {
                    self.controller_coordination_command_pending = false;
                }
                if operation.starts_with("worker control acknowledgement")
                    || operation.starts_with("worker control status result")
                    || operation.starts_with("worker control action")
                    || operation.starts_with("worker continuation execution")
                {
                    self.worker_control_command_pending = false;
                }
                if request_id.is_some() && request_id == self.commit_in_flight {
                    if let Some(request_id) = request_id {
                        self.commit_remote_intents.remove(&request_id);
                    }
                    self.commit_in_flight = None;
                }
                if let Some(revision) = revision {
                    self.saved_revision = self.saved_revision.min(revision.saturating_sub(1));
                }
                if let Some(turn_id) = turn_id {
                    if self
                        .coordination_pending_wake
                        .as_ref()
                        .is_some_and(|pending| pending.0 == turn_id)
                    {
                        self.coordination_pending_wake = None;
                        self.coordination_auto_wake_targets.clear();
                    }
                    if self
                        .pending_remote_turn
                        .as_ref()
                        .is_some_and(|pending| pending.turn_id == turn_id)
                    {
                        self.pending_remote_turn = None;
                    }
                }
                self.status = format!("{operation} failed: {error}");
                if operation.contains("OpenViking") || operation.contains("memory checkpoint") {
                    self.openviking_checkpoint_status = self.status.clone();
                }
            }
            _ => unreachable!("unexpected failures notice"),
        }
    }
}
