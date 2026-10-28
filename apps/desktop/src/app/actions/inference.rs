//! inference user actions extracted without changing their behavior.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn process_due_delivery_retry(&mut self) {
        let Some((request_id, command)) = self.active_remote_turn.as_ref().and_then(|active| {
            active
                .retry_at
                .filter(|at| Instant::now() >= *at)
                .map(|_| (active.request_id.clone(), active.command.clone()))
        }) else {
            return;
        };
        if let Some(active) = self.active_remote_turn.as_mut() {
            active.retry_at = None;
            active.cumulative_text.clear();
            active.observed_output = false;
        }
        match self.remote.send(command) {
            Ok(()) => {
                self.delivery
                    .set(&request_id, delivery_telemetry::Phase::BridgeQueued);
                self.remote_status =
                    "retry submitted to the local bridge · awaiting network".to_owned();
            }
            Err(error) => {
                self.delivery.error(
                    &request_id,
                    delivery_telemetry::Phase::Unconfirmed,
                    error.clone(),
                );
                self.remote_status = error;
                self.active_remote_turn = None;
            }
        }
    }

    pub(crate) fn start_chatgpt_sign_in(&mut self, repaint: &egui::Context) {
        if self.remote_runtime_failed {
            self.remote = siwc_bridge::BridgeRuntime::start(repaint);
            self.remote_runtime_ready = false;
            self.remote_runtime_failed = false;
            self.sign_in_requested = true;
            self.sign_in_pending = false;
            self.remote_status = "restarting ChatGPT sign-in runtime…".to_owned();
            return;
        }

        if !self.remote_runtime_ready {
            self.sign_in_requested = true;
            self.remote_status = "preparing ChatGPT sign-in runtime…".to_owned();
            return;
        }

        self.sign_in_pending = true;
        self.remote_status = "opening ChatGPT sign-in…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::SignIn) {
            self.sign_in_pending = false;
            self.remote_runtime_ready = false;
            self.remote_runtime_failed = true;
            self.remote_status = error;
        }
    }

    pub(crate) fn cancel_chatgpt_sign_in(&mut self) {
        // The runtime can take time to bootstrap. Cancel a sign-in that has
        // not been dispatched yet instead of leaving an armed request that
        // will unexpectedly open the browser once the sidecar is ready.
        if self.sign_in_requested {
            self.sign_in_requested = false;
            self.remote_status = "ChatGPT sign-in cancelled before launch".to_owned();
            return;
        }
        self.remote_status = "cancelling ChatGPT sign-in…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::CancelSignIn) {
            self.remote_status = error;
        }
    }

    pub(crate) fn disconnect_chatgpt(&mut self) {
        self.remote_status = "disconnecting ChatGPT…".to_owned();
        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::Disconnect) {
            self.remote_status = error;
        }
    }

    pub(crate) fn request_chatgpt_models(&mut self) {
        match self.remote.send(siwc_bridge::BridgeCommand::ListModels {
            max_attempts: self.inference_settings.read_retry_attempts(),
        }) {
            Ok(()) => {
                self.model_list_pending = true;
                self.remote_status = "loading ChatGPT models…".to_owned();
            }
            Err(error) => {
                self.model_list_pending = false;
                self.remote_status = error;
            }
        }
    }

    pub(crate) fn start_capability_probes(&mut self) {
        if !self.remote_connected() {
            self.capability_probe.status =
                "capability probes require a connected ChatGPT plan".to_owned();
            return;
        }
        if self.pending_remote_turn.is_some() || self.active_remote_turn.is_some() {
            self.capability_probe.status =
                "wait for the active ChatGPT response before probing capabilities".to_owned();
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.capability_probe.status = "choose a ChatGPT model before probing".to_owned();
            return;
        };
        if self.capability_probe.running() {
            return;
        }

        self.capability_probe
            .start(model, self.remote_session.profile_id.clone());
        self.dispatch_next_capability_probe();
    }

    pub(crate) fn start_web_search_probe(&mut self) {
        if !self.remote_connected()
            || self.capability_probe.running()
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
        {
            self.capability_probe.status =
                "cannot verify web search while disconnected, busy, or already probing".to_owned();
            return;
        }
        let (Some(model), Some(profile_id)) = (
            self.selected_model.clone(),
            self.remote_session.profile_id.clone(),
        ) else {
            self.capability_probe.status =
                "web search verification needs an active model and profile identity".to_owned();
            return;
        };
        self.capability_probe
            .start_web_search(model, Some(profile_id));
        self.dispatch_next_capability_probe();
    }

    pub(crate) fn dispatch_next_capability_probe(&mut self) {
        if self.capability_probe.active.is_some() {
            return;
        }
        let Some(spec) = self.capability_probe.next() else {
            self.finish_capability_probes();
            return;
        };
        let probe_name = spec.name.to_owned();
        let command = siwc_bridge::BridgeCommand::ProbeResponse {
            request_id: capability_probes::request_id(spec.name),
            model: self.capability_probe.model.clone().unwrap_or_default(),
            input: spec.input,
            request_patch: spec.request_patch,
        };
        match self.remote.send(command) {
            Ok(()) => self.capability_probe.mark_active(&probe_name),
            Err(error) => {
                self.capability_probe
                    .complete_dispatch_error(&probe_name, error);
                self.dispatch_next_capability_probe();
            }
        }
    }

    pub(crate) fn finish_capability_probes(&mut self) {
        if self.capability_probe.running() || self.capability_probe.results.is_empty() {
            return;
        }
        let data_dir = self.journal_path.parent().unwrap_or_else(|| Path::new("."));
        let report_path = data_dir.join("siwc-capability-probes.json");
        let contract_path = data_dir.join("local-inference-contract.json");
        let generated_unix_ms = unix_now_ms();
        let status = match capability_probes::save_report(
            &report_path,
            &self.capability_probe,
            generated_unix_ms,
        ) {
            Ok(()) => {
                self.capability_probe.generated_unix_ms = Some(generated_unix_ms);
                match local_inference_contract::save_contract(
                    &contract_path,
                    &self.capability_probe,
                    unix_now_ms(),
                ) {
                    Ok(()) => match local_inference_contract::load_contract(&contract_path) {
                        Ok(contract @ Some(_)) => {
                            self.local_inference_contract = contract;
                            format!(
                                "capability probes complete · saved {} · contract {}",
                                report_path.display(),
                                local_inference_contract::contract_state(&self.capability_probe)
                            )
                        }
                        Ok(None) => {
                            self.local_inference_contract = None;
                            "capability probes complete · report saved · contract reload missing"
                                .to_owned()
                        }
                        Err(error) => {
                            self.local_inference_contract = None;
                            format!(
                                "capability probes complete · report saved · contract reload error: {error}"
                            )
                        }
                    },
                    Err(error) => {
                        self.local_inference_contract = None;
                        format!(
                            "capability probes complete · report saved · contract error: {error}"
                        )
                    }
                }
            }
            Err(error) => format!("capability probes complete · {error}"),
        };
        self.capability_probe.status = status;
        if let Some((conversation, model, profile)) = self.pending_web_search_enable.take() {
            if self.local_conversation_id == conversation
                && self.selected_model.as_deref() == Some(model.as_str())
                && self.remote_session.profile_id.as_deref() == Some(profile.as_str())
                && self
                    .current_capability_gate()
                    .allows(context_composer::CapabilitySlot::WebSearch)
            {
                self.conversation_behavior_profile.web_search = true;
                self.persist_current_behavior_profile();
                self.capability_probe
                    .status
                    .push_str(" · web search enabled for this conversation");
            } else {
                self.capability_probe.status.push_str(
                    " · web search remains off: probe evidence did not authorize the current conversation/model/profile",
                );
            }
        }
    }

    pub(crate) fn remote_connected(&self) -> bool {
        self.remote_session.status == "connected" && self.remote_session.sharing
    }

    pub(crate) fn remote_identity_label(&self) -> String {
        self.remote_session
            .email
            .clone()
            .or_else(|| self.remote_session.profile_label.clone())
            .unwrap_or_else(|| "ChatGPT account".to_owned())
    }

    pub(crate) fn persist_current_inference_settings(&mut self) {
        let settings = local_inference_settings::ConversationInferenceSettings {
            model: self.selected_model.clone(),
            instructions: self.conversation_instructions.clone(),
            developer_context: self.conversation_developer_context.clone(),
            mcp_enabled: self.conversation_mcp_enabled,
            slow_warning_seconds: self.slow_warning_seconds,
        };
        if let Err(error) = self
            .inference_settings
            .set(self.local_conversation_id, settings)
        {
            self.status = format!("failed to update inference controls: {error}");
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist inference controls: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = sender.send(PersistCommand::SaveInferenceSettings {
            store: self.inference_settings.clone(),
        }) {
            self.status = format!("failed to queue inference controls: {error}");
        }
    }

    pub(crate) fn save_mcp_agent_defaults(&mut self) {
        if let Err(error) = self.mcp_agent_defaults.validate() {
            self.status = format!("MCP defaults not saved: {error}");
            return;
        }
        let Some(tx) = &self.persist_tx else {
            self.status = "MCP defaults not saved: persistence unavailable".to_owned();
            return;
        };
        match tx.send(PersistCommand::SaveMcpAgentDefaults {
            defaults: self.mcp_agent_defaults.clone(),
        }) {
            Ok(()) => {
                self.status =
                    "MCP bootstrap and exact tool policies queued for local save.".to_owned();
            }
            Err(error) => self.status = format!("MCP defaults could not be queued: {error}"),
        }
    }

    pub(crate) fn current_capability_gate(&self) -> context_composer::CapabilityGate {
        context_composer::CapabilityGate::from_contract(
            self.local_inference_contract.as_ref(),
            self.remote_session.profile_id.as_deref(),
            self.selected_model.as_deref(),
        )
    }

    pub(crate) fn current_behavior_request_patch(&self) -> Result<Value, String> {
        self.conversation_behavior_profile
            .request_patch(&self.current_capability_gate())
    }

    pub(crate) fn persist_current_behavior_profile(&mut self) {
        self.behavior_profiles.set(
            self.local_conversation_id,
            self.conversation_behavior_profile.clone(),
        );
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist behavior profile: persistence unavailable".to_owned();
            return;
        };
        if let Err(error) = sender.send(PersistCommand::SaveBehaviorProfiles {
            store: self.behavior_profiles.clone(),
        }) {
            self.status = format!("failed to queue behavior profile: {error}");
        }
    }
}
