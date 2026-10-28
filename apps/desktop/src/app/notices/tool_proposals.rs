//! Feature-specific tool_proposals completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_tool_proposals_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::ToolProviderUpdated { provider_id, event } => {
                let registered = event.kind == EventKind::ToolProviderRegistered;
                let configured = event.kind == EventKind::ToolProviderTransportConfigured;
                self.events.push(event);
                self.tool_command_pending = false;
                self.tool_selected_provider = Some(provider_id);
                if registered {
                    self.tool_provider_name_draft.clear();
                }
                if configured {
                    self.tool_stdio_executable_draft.clear();
                    self.tool_stdio_argv_draft.clear();
                    self.tool_stdio_operations_draft.clear();
                }
                self.status = format!("tool provider {} durably updated", provider_id.get());
            }
            PersistNotice::AgentMcpCallProposed {
                turn_id,
                call_id,
                route_id,
                policy,
                appended_events,
            } => {
                self.events.extend(appended_events);
                self.tool_command_pending = false;
                let matches_flow = self.agent_mcp_flow.as_ref().is_some_and(|flow| {
                    flow.turn_id == turn_id
                        && flow.pending_call.is_none()
                        && flow.policy == policy
                });
                if !matches_flow {
                    self.status = "MCP proposal was recorded, but its agent session is no longer active. No tool launched.".to_owned();
                    return;
                }
                if let Some(flow) = self.agent_mcp_flow.as_mut() {
                    flow.pending_call = Some(call_id);
                    flow.pending_route = Some(route_id);
                }
                let record = replay_tool_call_audit(&self.events)
                    .ok()
                    .and_then(|calls| calls.into_iter().find(|call| call.call_id == call_id));
                // Follow the pending agent call in the manual review panel.
                // Without this, the panel stays filtered to an older provider
                // and makes the required Allow/Run actions hard to discover.
                if let Some(call) = record.as_ref() {
                    self.tool_selected_provider = Some(call.provider_id);
                }
                let provider_is_builtin = record
                    .and_then(|call| {
                        replay_tool_provider_audit(&self.events)
                            .ok()
                            .and_then(|providers| {
                                providers
                                    .into_iter()
                                    .find(|provider| provider.provider_id == call.provider_id)
                            })
                    })
                    .is_some_and(|provider| {
                        provider.name.as_str() == local_tool_adapter::BUILTIN_PROVIDER_NAME
                    });
                if policy == mcp_agent_defaults::ToolPolicy::Automatic {
                    if provider_is_builtin {
                        let shell_request = replay_tool_call_audit(&self.events)
                            .ok()
                            .and_then(|calls| {
                                calls.into_iter().find(|call| call.call_id == call_id)
                            })
                            .is_some_and(|call| {
                                call.operation.as_str() == host_shell_mcp::OPERATION
                            });
                        if shell_request {
                            self.dispatch_host_shell(route_id);
                        } else {
                            self.dispatch_builtin_hello(route_id);
                        }
                    } else {
                        self.execute_approved_tool(call_id);
                    }
                } else {
                    self.status = format!(
                        "MCP call {} awaits approval. Expand Tools / MCP, open Tool call audit, then use NEXT MANUAL MCP CALL REVIEW: Allow this route, followed by Run. The assistant turn is paused.",
                        call_id.get(),
                    );
                }
            }
            _ => unreachable!("unexpected tool_proposals notice"),
        }
    }
}
