//! Feature-specific tool_outcomes completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_tool_outcomes_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::ToolCallProposed {
                call_id,
                route_id,
                appended_events,
            } => {
                self.events.extend(appended_events);
                self.tool_command_pending = false;
                self.tool_operation_draft.clear();
                self.tool_arguments_draft.clear();
                self.status = format!(
                    "tool call {} durably proposed on pending route {}",
                    call_id.get(),
                    route_id.get(),
                );
            }
            PersistNotice::ToolCallRoutePolicyUpdated { route_id, event } => {
                self.events.push(event);
                self.tool_command_pending = false;
                let denied = replay_routing_audit(&self.events)
                    .ok()
                    .and_then(|routes| {
                        routes
                            .into_iter()
                            .find(|route| route.request.id == route_id)
                    })
                    .is_some_and(|route| {
                        route.latest_user_decision == Some(RouteUserDecision::Deny)
                    });
                if denied
                    && self
                        .agent_mcp_flow
                        .as_ref()
                        .is_some_and(|flow| flow.pending_route == Some(route_id))
                {
                    self.agent_mcp_flow = None;
                }
                self.status = if denied {
                    format!("MCP route {} denied; tool did not execute", route_id.get())
                } else {
                    format!(
                        "MCP route {} approved; select Run once in chat",
                        route_id.get()
                    )
                };
            }
            PersistNotice::HostShellDispatched { route_id, event } => {
                self.events.push(event);
                self.status = format!(
                    "host shell route {} reserved and running; result will be durably recorded",
                    route_id.get()
                );
            }
            PersistNotice::BuiltinHelloDispatched {
                route_id,
                appended_events,
            } => {
                let count = appended_events.len();
                self.events.extend(appended_events);
                self.tool_command_pending = false;
                self.status = format!(
                    "builtin tool route {} completed · {} durable event{}",
                    route_id.get(),
                    count,
                    if count == 1 { "" } else { "s" },
                );
                let tool = self.agent_mcp_flow.as_ref().and_then(|flow| {
                    (flow.pending_route == Some(route_id))
                        .then_some(flow.pending_call)
                        .flatten()
                });
                if let Some(call_id) = tool {
                    self.decide_tool_result_context(call_id, ToolResultContextDecision::Admit);
                }
            }
            PersistNotice::SandboxHostChecked { result } => {
                self.tool_command_pending = false;
                self.last_sandbox_host_check = Some(result.clone());
                self.status = match result {
                    Ok(()) => "Linux confinement host probe passed. No tool provider launched and no route permit consumed.".to_owned(),
                    Err(error) => format!("Linux confinement host probe blocked: {error}"),
                };
            }
            PersistNotice::StdioToolDispatchReserved { call_id, event } => {
                self.events.push(event);
                self.status = format!(
                    "external MCP call {} durably dispatched; no retries…",
                    call_id.get()
                );
            }
            PersistNotice::StdioToolDispatchFinished { call_id, event } => {
                self.events.push(event);
                self.tool_command_pending = false;
                let status = replay_tool_call_outcome_audit(&self.events)
                    .ok()
                    .and_then(|outcomes| {
                        outcomes
                            .into_iter()
                            .find(|outcome| outcome.call_id == call_id)
                    })
                    .map(|outcome| match outcome.kind {
                        ToolCallOutcomeKind::Result => "completed",
                        ToolCallOutcomeKind::Error => "returned a sandbox/tool error",
                    })
                    .unwrap_or("recorded terminal observation");
                self.status = format!(
                    "external MCP call {} {status}; outcome durably recorded",
                    call_id.get()
                );
                if self
                    .agent_mcp_flow
                    .as_ref()
                    .is_some_and(|flow| flow.pending_call == Some(call_id))
                {
                    self.decide_tool_result_context(call_id, ToolResultContextDecision::Admit);
                }
            }
            PersistNotice::ToolResultContextDecisionUpdated { call_id, event } => {
                self.events.push(event);
                self.tool_command_pending = false;
                self.status = format!(
                    "tool call {} context admission durably updated",
                    call_id.get()
                );
                if self
                    .agent_mcp_flow
                    .as_ref()
                    .is_some_and(|flow| flow.pending_call == Some(call_id))
                {
                    let admitted = replay_tool_result_context_audit(&self.events)
                        .ok()
                        .is_some_and(|decisions| {
                            decisions.iter().any(|decision| {
                                decision.call_id == call_id
                                    && decision.decision == ToolResultContextDecision::Admit
                            })
                        });
                    if admitted {
                        self.continue_agent_after_admitted_result(call_id);
                    } else {
                        self.agent_mcp_flow = None;
                        self.status =
                            "MCP turn stopped: result was excluded or admission is unverified"
                                .to_owned();
                    }
                }
            }
            _ => unreachable!("unexpected tool_outcomes notice"),
        }
    }
}
