//! Desktop tools actions extracted intact from ChatariumApp.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn register_tool_provider(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let name = match ToolProviderName::new(self.tool_provider_name_draft.clone()) {
            Ok(name) => name,
            Err(error) => {
                self.status = format!("cannot register tool provider: {error:?}");
                return;
            }
        };
        let provider_id = match next_available_tool_provider_id(&self.events) {
            Ok(provider_id) => provider_id,
            Err(error) => {
                self.status = format!("cannot allocate tool provider identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot register tool provider: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RegisterToolProvider { provider_id, name }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!("registering inert tool provider {}…", provider_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue tool provider registration: {error}");
            }
        }
    }

    pub(crate) fn bind_tool_provider_endpoint(&mut self, provider_id: ToolProviderId) {
        if self.tool_command_pending {
            return;
        }
        let endpoint_id = match next_available_route_endpoint_id(&self.events) {
            Ok(endpoint_id) => endpoint_id,
            Err(error) => {
                self.status = format!("cannot allocate tool routing endpoint: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot bind tool provider endpoint: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::BindToolProviderEndpoint {
            provider_id,
            endpoint_id,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "binding tool provider {} to routing endpoint {}…",
                    provider_id.get(),
                    endpoint_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool endpoint binding: {error}");
            }
        }
    }

    pub(crate) fn configure_stdio_tool_provider(&mut self, provider_id: ToolProviderId) {
        if self.tool_command_pending {
            return;
        }
        let config = match local_tool_provider_control::parse_stdio_config_draft(
            &self.tool_stdio_executable_draft,
            &self.tool_stdio_argv_draft,
            &self.tool_stdio_operations_draft,
        ) {
            Ok(config) => config,
            Err(error) => {
                self.status = format!("cannot configure tool provider: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot configure provider: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ConfigureToolProviderTransport {
            provider_id,
            config,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording inert stdio configuration for provider {}…",
                    provider_id.get()
                );
            }
            Err(error) => self.status = format!("failed to queue tool configuration: {error}"),
        }
    }

    pub(crate) fn decide_stdio_provider_activation(
        &mut self,
        provider_id: ToolProviderId,
        configured_sequence: u64,
        decision: ProviderActivationDecision,
    ) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot change activation: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolProviderActivation {
            provider_id,
            configured_sequence,
            decision,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording explicit {:?} for provider {}…",
                    decision,
                    provider_id.get()
                );
            }
            Err(error) => self.status = format!("failed to queue tool activation: {error}"),
        }
    }

    pub(crate) fn propose_tool_call(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let Some(provider_id) = self.tool_selected_provider else {
            self.status = "cannot propose tool call: select a provider".to_owned();
            return;
        };
        let operation = match ToolOperationName::new(self.tool_operation_draft.clone()) {
            Ok(operation) => operation,
            Err(error) => {
                self.status = format!("cannot propose tool call: invalid operation: {error:?}");
                return;
            }
        };
        let call_id = match next_available_tool_call_id(&self.events) {
            Ok(call_id) => call_id,
            Err(error) => {
                self.status = format!("cannot allocate tool call identity: {error}");
                return;
            }
        };
        let route_id = match next_available_route_id(&self.events) {
            Ok(route_id) => route_id,
            Err(error) => {
                self.status = format!("cannot allocate tool route identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot propose tool call: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ProposeToolCall {
            call_id,
            route_id,
            source_conversation_id: self.local_conversation_id,
            provider_id,
            operation,
            arguments_text: self.tool_arguments_draft.clone(),
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording immutable tool call {} and pending route {}…",
                    call_id.get(),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool call proposal: {error}");
            }
        }
    }

    /// Exact model output becomes a durable proposal, never a process launch.
    pub(crate) fn propose_agent_mcp_call(
        &mut self,
        active: &ActiveRemoteTurn,
        final_text: &str,
        mut proposal: mcp_agent_defaults::ModelToolRequest,
    ) {
        // Inject the user's selected per-chat cwd into the audited call only
        // when the model has not explicitly supplied its own cwd.
        if proposal.provider == local_tool_adapter::BUILTIN_PROVIDER_NAME
            && proposal.tool == host_shell_mcp::OPERATION
        {
            if let Some(directory) = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.project_directory.as_deref())
            {
                if let Some(arguments) = proposal.arguments.as_object_mut() {
                    arguments
                        .entry("cwd")
                        .or_insert_with(|| Value::String(directory.to_owned()));
                }
            }
        }
        if self.tool_command_pending {
            self.agent_mcp_flow = None;
            self.status = "MCP call stopped: another tool action is pending".to_owned();
            return;
        }
        let count = self
            .agent_mcp_flow
            .as_ref()
            .map(|flow| flow.call_count)
            .unwrap_or(0)
            .saturating_add(1);
        if count > self.mcp_agent_defaults.max_calls {
            self.agent_mcp_flow = None;
            self.status = format!(
                "MCP agent stopped at configured {}-call limit",
                self.mcp_agent_defaults.max_calls
            );
            return;
        }
        let policy = match mcp_agent_defaults::tool_policy(
            &self.mcp_agent_defaults.policy_rules,
            &proposal.provider,
            &proposal.tool,
        ) {
            Ok(policy) => policy,
            Err(error) => {
                self.agent_mcp_flow = None;
                self.status = format!("MCP policy invalid: {error}");
                return;
            }
        };
        if matches!(
            policy,
            mcp_agent_defaults::ToolPolicy::Disabled | mcp_agent_defaults::ToolPolicy::Manual
        ) {
            self.agent_mcp_flow = None;
            self.status = format!(
                "MCP call not authorized for model invocation: {}::{} is {}",
                proposal.provider,
                proposal.tool,
                policy.name(),
            );
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.agent_mcp_flow = None;
            self.status = "MCP call cannot proceed without durable journal".to_owned();
            return;
        };
        let turn_id = active.turn_id;
        let result = tx.send(PersistCommand::ProposeAgentMcpCall {
            turn_id,
            source_conversation_id: self.local_conversation_id,
            proposal,
            policy,
        });
        if let Err(error) = result {
            self.agent_mcp_flow = None;
            self.status = format!("MCP proposal couldn't enter durable queue: {error}");
            return;
        }
        self.agent_mcp_flow = Some(AgentMcpFlow {
            turn_id,
            conversation_id: self.local_conversation_id,
            command: active.command.clone(),
            proposal_text: final_text.to_owned(),
            call_count: count,
            pending_call: None,
            pending_route: None,
            policy,
        });
        self.tool_command_pending = true;
        self.status = "MCP tool proposal queued for durable policy validation".to_owned();
    }

    pub(crate) fn coordination_store_path(&self) -> PathBuf {
        self.journal_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(coordination_workspace::FILE_NAME)
    }

    /// Resolve the sender from the active, authenticated local conversation.
    /// The model can supply a destination but never a source or permission.
    pub(crate) fn execute_coordination_proposal(
        &mut self,
        active: &ActiveRemoteTurn,
        final_text: &str,
        proposal: &mcp_agent_defaults::ModelToolRequest,
    ) {
        if active.conversation_id != self.local_conversation_id
            && !self.is_authorized_background_wake_turn(active.conversation_id, active.turn_id)
        {
            self.status =
                "coordination call denied: no valid background wake provenance".to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        let limit = self.mcp_agent_defaults.max_calls;
        if !self.coordination_tool_budget.admit(active.turn_id, limit) {
            self.status = "coordination call limit reached for this turn".to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        let source = active.conversation_id.to_string();
        let authorization = mcp_agent_defaults::tool_policy(
            &self.mcp_agent_defaults.policy_rules,
            &proposal.provider,
            &proposal.tool,
        );
        let result: Result<Value, String> = if authorization
            != Ok(mcp_agent_defaults::ToolPolicy::Automatic)
        {
            Err("coordination operation denied by exact MCP tool policy".to_owned())
        } else {
            match proposal.tool.as_str() {
                "coordination.identity" => {
                    if proposal
                        .arguments
                        .as_object()
                        .is_none_or(|args| !args.is_empty())
                    {
                        Err("coordination.identity accepts no arguments".to_owned())
                    } else {
                        self.coordination_workspace.identity_tool(&source)
                    }
                }
                "coordination.send" => {
                    let Some(args) = proposal.arguments.as_object() else {
                        self.status = "coordination.send requires object arguments".to_owned();
                        return;
                    };
                    let allowed_keys = args
                        .keys()
                        .all(|key| matches!(key.as_str(), "to" | "kind" | "body"));
                    if !allowed_keys || args.len() != 3 {
                        Err("coordination.send requires exactly to, kind and body".to_owned())
                    } else {
                        match (
                            args.get("to").and_then(Value::as_u64),
                            args.get("kind").and_then(Value::as_str),
                            args.get("body").and_then(Value::as_str),
                        ) {
                            (Some(to), Some(kind), Some(body)) => {
                                let before = self.coordination_workspace.clone();
                                match self.coordination_workspace.send(
                                    &source,
                                    to,
                                    kind,
                                    body,
                                    unix_now_ms(),
                                ) {
                                    Ok((id, status)) => {
                                        match self
                                            .coordination_workspace
                                            .save_atomic(&self.coordination_store_path())
                                        {
                                            Ok(()) => Ok(serde_json::json!({
                                                "message_id":id,"status":status,
                                                "source_conversation":source,
                                                "target_worker":to,
                                                "verified_by":"Chatarium local coordination authority",
                                            })),
                                            Err(error) => {
                                                self.coordination_workspace = before;
                                                Err(format!(
                                                    "coordination message persistence failed: {error}"
                                                ))
                                            }
                                        }
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            _ => Err("coordination.send arguments have invalid types".to_owned()),
                        }
                    }
                }
                _ => Err("unknown coordination operation".to_owned()),
            }
        };
        let tool_result = match result {
            Ok(value) => serde_json::json!({"ok":true,"result":value}),
            Err(error) => serde_json::json!({"ok":false,"error":error}),
        };
        // This result is derived from the saved workspace and is distinguishable
        // from user text and journaled shell MCP tool results.
        let siwc_bridge::BridgeCommand::StreamResponse {
            model,
            mut input,
            instructions,
            request_patch,
            ..
        } = active.command.clone()
        else {
            self.status = "coordination continuation requires a Responses stream".to_owned();
            return;
        };
        let Some(messages) = input.as_array_mut() else {
            self.status = "coordination continuation requires typed messages".to_owned();
            return;
        };
        messages.push(serde_json::json!({
            "role":"assistant","content":final_text
        }));
        messages.push(serde_json::json!({
            "role":"user",
            "content":format!(
                "CHATARIUM VERIFIED COORDINATION TOOL RESULT (not a human message)\n{}",
                tool_result
            )
        }));
        let request_id = format!(
            "{}-coordination-{}",
            active.turn_id,
            self.coordination_tool_budget.used()
        );
        let next = PendingRemoteTurn {
            conversation_id: active.conversation_id,
            turn_id: active.turn_id,
            request_id: request_id.clone(),
            model,
            input,
            instructions,
            request_patch,
        };
        let journal = remote_turn_payload(
            active.turn_id,
            &request_id,
            Some(&next.model),
            None,
            Some("Chatarium verified local coordination operation; resuming same turn"),
        );
        if !self.queue_turn_event(active.turn_id, EventKind::DispatchAttempted, journal) {
            self.status =
                "coordination result recorded; remote continuation could not be journaled"
                    .to_owned();
            self.agent_mcp_flow = None;
            return;
        }
        self.delivery
            .begin(active.conversation_id.to_string(), request_id);
        self.pending_remote_turn = Some(next);
        self.agent_mcp_flow = None;
        self.status = "coordination operation recorded; assistant continuation queued".to_owned();
    }

    /// Exactly the admitted result from the durable journal is returned to the
    /// model; no manual or model-authored result string can be substituted.
    pub(crate) fn continue_agent_after_admitted_result(&mut self, call_id: ToolCallId) {
        let Some(mut flow) = self.agent_mcp_flow.take() else {
            return;
        };
        if flow.pending_call != Some(call_id) || flow.conversation_id != self.local_conversation_id
        {
            self.agent_mcp_flow = Some(flow);
            return;
        }
        let admitted = match replay_admitted_tool_results(&self.events, flow.conversation_id) {
            Ok(results) => results,
            Err(error) => {
                self.status = format!("MCP continuation blocked by result audit: {error}");
                return;
            }
        };
        let Some(outcome) = admitted
            .into_iter()
            .find(|result| result.record.call_id == call_id)
        else {
            self.status = "MCP continuation stopped: no exact admitted tool result".to_owned();
            return;
        };
        let record = outcome.record;
        let tool_context = context_composer::TranscriptMessage::tool_result(
            &outcome.text,
            record.call_id.get(),
            record.route_id.get(),
            record.provider_id.get(),
            record.source_session_id.get(),
            record.outcome_sequence,
            record.last_decision_sequence,
            record.outcome_kind.stable_name(),
        );
        let siwc_bridge::BridgeCommand::StreamResponse {
            model,
            mut input,
            instructions,
            request_patch,
            ..
        } = flow.command.clone()
        else {
            self.status = "MCP continuation requires a Responses stream command".to_owned();
            return;
        };
        let Some(messages) = input.as_array_mut() else {
            self.status = "MCP continuation requires a typed input-message array".to_owned();
            return;
        };
        messages.push(serde_json::json!({
            "role": "assistant",
            "content": flow.proposal_text,
        }));
        messages.push(serde_json::json!({
            "role": "user",
            "content": tool_context.text,
        }));
        let request_id = format!("{}-mcp-{}", flow.turn_id, flow.call_count);
        self.delivery
            .begin(flow.conversation_id.to_string(), request_id.clone());
        let pending = PendingRemoteTurn {
            conversation_id: flow.conversation_id,
            turn_id: flow.turn_id,
            request_id: request_id.clone(),
            model,
            input,
            instructions,
            request_patch,
        };
        let journal_entry = remote_turn_payload(
            flow.turn_id,
            &request_id,
            Some(&pending.model),
            None,
            Some("same user turn resumed after exact journaled MCP outcome admission"),
        );
        if !self.queue_turn_event(flow.turn_id, EventKind::DispatchAttempted, journal_entry) {
            self.status = "MCP continuation blocked: could not journal dispatch".to_owned();
            return;
        }
        flow.pending_call = None;
        flow.pending_route = None;
        self.pending_remote_turn = Some(pending);
        self.agent_mcp_flow = Some(flow);
        self.status =
            "MCP result admitted; resuming assistant without a new user message".to_owned();
    }

    pub(crate) fn check_sandbox_host_readiness(&mut self) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot check host confinement: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::CheckSandboxHostReadiness) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.last_sandbox_host_check = None;
                self.status =
                    "checking real Linux namespace sandbox with a fixed harmless process…"
                        .to_owned();
            }
            Err(error) => {
                self.status = format!("failed to queue host-readiness check: {error}");
            }
        }
    }

    pub(crate) fn execute_approved_tool(&mut self, call_id: ToolCallId) {
        let is_http = replay_tool_call_audit(&self.events)
            .ok()
            .and_then(|records| records.into_iter().find(|call| call.call_id == call_id))
            .and_then(|call| {
                replay_tool_provider_audit(&self.events)
                    .ok()
                    .and_then(|providers| {
                        providers
                            .into_iter()
                            .find(|provider| provider.provider_id == call.provider_id)
                            .map(|provider| (provider.name.as_str().to_owned(), call.route_id))
                    })
            });
        if let Some((provider, Some(route))) = is_http {
            if local_http_mcp::known_provider(&provider) {
                self.dispatch_local_http_mcp(route);
                return;
            }
        }
        self.execute_approved_stdio_tool(call_id);
    }

    pub(crate) fn dispatch_local_http_mcp(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot invoke localhost MCP: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchLocalHttpMcp { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating and reserving fixed localhost MCP route {}",
                    route_id.get()
                );
            }
            Err(error) => self.status = format!("cannot queue localhost MCP: {error}"),
        }
    }

    pub(crate) fn execute_approved_stdio_tool(&mut self, call_id: ToolCallId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot execute external tool: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::ExecuteApprovedStdioTool { call_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "checking Linux namespace confinement before reserving one-shot tool call {}…",
                    call_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue external tool execution: {error}");
            }
        }
    }

    pub(crate) fn dispatch_host_shell(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot run host shell: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchHostShell { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating host shell settings and durable route {} before one-shot execution",
                    route_id.get()
                );
            }
            Err(error) => self.status = format!("cannot queue host shell: {error}"),
        }
    }

    pub(crate) fn save_host_shell_settings(&mut self) {
        if let Err(error) = self.host_shell_settings.validate() {
            self.status = format!("host shell settings invalid: {error}");
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.status = "cannot save shell settings: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = tx.send(PersistCommand::SaveHostShellSettings {
            settings: self.host_shell_settings.clone(),
        }) {
            self.status = format!("cannot queue shell settings save: {error}");
        } else {
            self.status = "shell choice and command blacklist queued for persistence".to_owned();
        }
    }

    pub(crate) fn dispatch_builtin_hello(&mut self, route_id: RouteId) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot execute local builtin: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DispatchBuiltinHello { route_id }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "validating approved builtin route {} and consuming one-shot dispatch authority…",
                    route_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue builtin dispatch: {error}");
            }
        }
    }

    pub(crate) fn decide_tool_call_route(
        &mut self,
        route_id: RouteId,
        decision: RouteUserDecision,
    ) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update tool route policy: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolCallRoute { route_id, decision }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording explicit {} decision for tool route {}…",
                    route_user_decision_label(decision),
                    route_id.get()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool route decision: {error}");
            }
        }
    }

    pub(crate) fn decide_tool_result_context(
        &mut self,
        call_id: ToolCallId,
        decision: ToolResultContextDecision,
    ) {
        if self.tool_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update tool result context: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideToolResultContext {
            call_id,
            conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.tool_command_pending = true;
                self.status = format!(
                    "recording tool call {} context {} decision…",
                    call_id.get(),
                    decision.stable_name()
                );
            }
            Err(error) => {
                self.status = format!("failed to queue tool result context decision: {error}");
            }
        }
    }
}
