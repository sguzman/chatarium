//! Behavior-preserving extraction of remote_events methods from the desktop application.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn process_remote_notices(&mut self) {
        for notice in self.remote.drain() {
            match notice {
                siwc_bridge::BridgeEvent::Ready => {
                    self.remote_runtime_ready = true;
                    self.remote_runtime_failed = false;
                    if self.sign_in_requested {
                        self.sign_in_requested = false;
                        self.sign_in_pending = true;
                        self.remote_status = "opening ChatGPT sign-in…".to_owned();
                        if let Err(error) = self.remote.send(siwc_bridge::BridgeCommand::SignIn) {
                            self.sign_in_pending = false;
                            self.remote_runtime_ready = false;
                            self.remote_runtime_failed = true;
                            self.remote_status = error;
                        }
                    } else {
                        self.remote_status = "sign-in runtime ready".to_owned();
                        if let Err(error) =
                            self.remote.send(siwc_bridge::BridgeCommand::RefreshSession)
                        {
                            self.remote_runtime_ready = false;
                            self.remote_runtime_failed = true;
                            self.remote_status = error;
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Session(session) => {
                    let was_connected = self.remote_connected();
                    let account_changed = session_identity_changed(&self.remote_session, &session);

                    self.sign_in_pending =
                        sign_in_pending_after_session(self.sign_in_pending, &session.status);
                    self.remote_session = session;

                    if self.remote_session.status == "connected" && self.remote_session.sharing {
                        if account_changed {
                            self.remote_models.clear();
                            self.model_list_pending = false;
                        }

                        if should_request_models(
                            was_connected,
                            account_changed,
                            self.remote_models.is_empty(),
                            self.model_list_pending,
                        ) {
                            self.request_chatgpt_models();
                        } else if !was_connected && !self.model_list_pending {
                            self.remote_status = "ChatGPT plan connected".to_owned();
                        }
                    } else if self.sign_in_pending {
                        // A stale disconnected session refresh cannot finish an
                        // already requested OAuth flow.
                        self.remote_status = "waiting for ChatGPT sign-in…".to_owned();
                    } else if let Some(error) = &self.remote_session.error_message {
                        self.remote_status = error.clone();
                    } else if self.remote_session.status == "connected" {
                        self.remote_status =
                            "ChatGPT connected, but token sharing is not enabled".to_owned();
                    } else if self.remote_session.status == "reauth_required" {
                        self.remote_status = "ChatGPT sign-in needs renewal".to_owned();
                    } else {
                        self.model_list_pending = false;
                        self.remote_models.clear();
                        self.remote_status = "not connected".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::Models(models) => {
                    self.model_list_pending = false;
                    let keep_selected = self
                        .selected_model
                        .as_ref()
                        .is_some_and(|selected| models.iter().any(|model| &model.slug == selected));
                    if !keep_selected {
                        self.selected_model = models.first().map(|model| model.slug.clone());
                        self.persist_current_inference_settings();
                    }
                    self.remote_models = models;
                    self.remote_status = if self.remote_models.is_empty() {
                        "connected; no models reported".to_owned()
                    } else {
                        format!("connected · {} models", self.remote_models.len())
                    };
                }
                siwc_bridge::BridgeEvent::AgentActivity { request_id, frame } => {
                    // Attribute only an active turn; never accept a sidecar's
                    // claimed conversation identity or stale request frame.
                    if let Some(active) = self
                        .active_remote_turn
                        .as_ref()
                        .filter(|active| active.request_id == request_id)
                    {
                        if let Some(sender) = &self.persist_tx {
                            if let Err(error) = sender.send(PersistCommand::AppendAgentActivity {
                                conversation_id: active.conversation_id,
                                turn_id: active.turn_id,
                                frame,
                            }) {
                                self.status = format!("failed to queue agent activity: {error}");
                            }
                        } else {
                            self.status = "agent activity journal unavailable".to_owned();
                        }
                    }
                }
                siwc_bridge::BridgeEvent::TransportProgress { request_id, stage } => {
                    if self
                        .active_remote_turn
                        .as_ref()
                        .is_some_and(|active| active.request_id == request_id)
                    {
                        match stage.as_str() {
                            "wire_attempted" => self
                                .delivery
                                .set(&request_id, delivery_telemetry::Phase::WireAttempted),
                            "acknowledged" => self
                                .delivery
                                .set(&request_id, delivery_telemetry::Phase::Acknowledged),
                            _ => {}
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Delta { request_id, delta } => {
                    if capability_probes::probe_name_from_request_id(&request_id).is_some() {
                        continue;
                    }
                    let Some(active) = self
                        .active_remote_turn
                        .as_mut()
                        .filter(|active| active.request_id == request_id)
                    else {
                        continue;
                    };

                    let first_output = !active.observed_output;
                    active.observed_output = true;
                    active.cumulative_text.push_str(&delta);
                    if !active.source_suspect_logged {
                        if let Some(marker) = response_text_provenance::suspect_in_stream_tail(
                            &active.cumulative_text,
                        ) {
                            diagnostics::info(
                                "siwc-text-provenance",
                                format!(
                                    "bridge delta request={} suspect={} source=stream; assistant text retained verbatim",
                                    diagnostics::short_id(&request_id),
                                    marker.label(),
                                ),
                            );
                            active.source_suspect_logged = true;
                        }
                    }
                    let turn_id = active.turn_id;
                    let cumulative_text = active.cumulative_text.clone();

                    if first_output {
                        self.delivery
                            .set(&request_id, delivery_telemetry::Phase::Streaming);
                        let acceptance = remote_turn_payload(
                            turn_id,
                            &request_id,
                            None,
                            None,
                            Some("Responses stream produced assistant output"),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::RemoteAcceptanceObserved,
                            acceptance,
                        );
                        let started = remote_turn_payload(
                            turn_id,
                            &request_id,
                            None,
                            None,
                            Some("assistant output stream started"),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::AssistantStreamStarted,
                            started,
                        );
                    }

                    let snapshot = remote_turn_payload(
                        turn_id,
                        &request_id,
                        None,
                        Some(&cumulative_text),
                        Some("cumulative Responses stream text"),
                    );
                    let _ = self.queue_turn_event(
                        turn_id,
                        EventKind::AssistantSnapshotObserved,
                        snapshot,
                    );
                }
                siwc_bridge::BridgeEvent::ResponseCompleted {
                    request_id,
                    text,
                    web_search_executed,
                } => {
                    if let Some(attempt) = self
                        .openviking_summary_in_flight
                        .clone()
                        .filter(|pending| pending.request_id == request_id)
                    {
                        match openviking_memory_summary::validate_rolling_summary(
                            &text,
                            attempt.had_confirmed_prior,
                        ) {
                            Ok(summary) => {
                                let result = self
                                    .persist_tx
                                    .as_ref()
                                    .ok_or_else(|| "local persistence unavailable".to_owned())
                                    .and_then(|sender| {
                                        sender
                                            .send(PersistCommand::SaveOpenVikingMemorySummary {
                                                conversation_id: attempt.conversation_id,
                                                turn_id: attempt.turn_id,
                                                completed_sequence: attempt.completed_sequence,
                                                summary,
                                            })
                                            .map_err(|error| error.to_string())
                                    });
                                if let Err(error) = result {
                                    self.openviking_summary_in_flight = None;
                                    self.openviking_summary_blocked.insert(request_id);
                                    self.status =
                                        format!("OpenViking summary not persisted: {error}");
                                }
                            }
                            Err(error) => {
                                self.openviking_summary_in_flight = None;
                                self.openviking_summary_blocked.insert(request_id);
                                self.status = format!("OpenViking summary rejected: {error}");
                            }
                        }
                        continue;
                    }
                    if let Some(probe_name) =
                        capability_probes::probe_name_from_request_id(&request_id)
                    {
                        if probe_name == "web_search" {
                            self.capability_probe
                                .complete_web_search(!text.is_empty(), web_search_executed);
                        } else {
                            self.capability_probe
                                .complete_supported(probe_name, !text.is_empty());
                        }
                        self.dispatch_next_capability_probe();
                        continue;
                    }
                    if !self
                        .active_remote_turn
                        .as_ref()
                        .is_some_and(|active| active.request_id == request_id)
                    {
                        continue;
                    }
                    let active = self
                        .active_remote_turn
                        .take()
                        .expect("matched active remote turn");
                    let relation = response_text_provenance::final_source_relation(
                        &active.cumulative_text,
                        &text,
                    );
                    if relation
                        == response_text_provenance::FinalSourceRelation::FinalDiffersFromStream
                    {
                        diagnostics::info(
                            "siwc-text-provenance",
                            format!(
                                "bridge completion request={} final_differs_from_stream=true; content kept unmodified",
                                diagnostics::short_id(&request_id),
                            ),
                        );
                    }
                    if !active.source_suspect_logged {
                        if let Some(marker) =
                            response_text_provenance::suspect_in_stream_tail(&text)
                        {
                            diagnostics::info(
                                "siwc-text-provenance",
                                format!(
                                    "bridge completion request={} suspect={} source=final; assistant text retained verbatim",
                                    diagnostics::short_id(&request_id),
                                    marker.label(),
                                ),
                            );
                        }
                    }
                    self.delivery
                        .set(&request_id, delivery_telemetry::Phase::Complete);
                    let final_text = if text.is_empty() {
                        active.cumulative_text.clone()
                    } else {
                        text
                    };

                    if !active.observed_output {
                        let acceptance = remote_turn_payload(
                            active.turn_id,
                            &request_id,
                            None,
                            None,
                            Some("Responses stream completed"),
                        );
                        let _ = self.queue_turn_event(
                            active.turn_id,
                            EventKind::RemoteAcceptanceObserved,
                            acceptance,
                        );
                        if !final_text.is_empty() {
                            let started = remote_turn_payload(
                                active.turn_id,
                                &request_id,
                                None,
                                None,
                                Some("completed response contained assistant output"),
                            );
                            let _ = self.queue_turn_event(
                                active.turn_id,
                                EventKind::AssistantStreamStarted,
                                started,
                            );
                        }
                    }

                    let completed = remote_turn_payload(
                        active.turn_id,
                        &request_id,
                        None,
                        Some(&final_text),
                        Some("response.completed observed"),
                    );
                    let _ = self.queue_turn_event(
                        active.turn_id,
                        EventKind::AssistantCompletionObserved,
                        completed,
                    );

                    let foreground = active.conversation_id == self.local_conversation_id;
                    let opt_in = if foreground {
                        self.conversation_mcp_enabled
                    } else {
                        self.inference_settings
                            .for_conversation_with_mcp_defaults(
                                active.conversation_id,
                                &self.mcp_agent_defaults,
                            )
                            .mcp_enabled
                    };
                    match mcp_agent_defaults::parse_model_tool_request(&final_text) {
                        Ok(Some(proposal)) if opt_in || self.agent_mcp_flow.is_some() => {
                            if proposal.provider == "chatarium.builtin"
                                && matches!(
                                    proposal.tool.as_str(),
                                    "coordination.identity" | "coordination.send"
                                )
                            {
                                self.execute_coordination_proposal(&active, &final_text, &proposal);
                            } else if foreground {
                                self.propose_agent_mcp_call(&active, &final_text, proposal);
                            } else {
                                self.status =
                                    "background wake tool denied: only exact coordination operations are admitted"
                                        .to_owned();
                            }
                        }
                        Ok(Some(_)) => {
                            self.status = "Assistant produced an MCP proposal, but tool execution is not enabled for this conversation. No tool was dispatched.".to_owned();
                        }
                        Ok(None) => {
                            // The completion journal command precedes this
                            // checkpoint intent on the same persistence queue.
                            // No user reminder or model-authored MCP call needed.
                            // Do not queue a checkpoint from intermediate tool
                            // proposals or unknown remote outcomes.
                            if let Some(sender) = &self.persist_tx {
                                let _ = sender.send(PersistCommand::QueueMemoryCheckpoint {
                                    conversation_id: active.conversation_id,
                                    turn_id: active.turn_id,
                                });
                            }
                            self.agent_mcp_flow = None;
                        }
                        Err(error) => {
                            self.agent_mcp_flow = None;
                            self.status = format!("Malformed agent MCP proposal rejected: {error}");
                        }
                    }
                }
                siwc_bridge::BridgeEvent::Failed { request_id, error } => {
                    if let Some(pending) = self.openviking_summary_in_flight.as_ref() {
                        if request_id.as_deref() == Some(pending.request_id.as_str()) {
                            let key = pending.request_id.clone();
                            self.openviking_summary_in_flight = None;
                            self.openviking_summary_blocked.insert(key);
                            self.status =
                                format!("OpenViking summary generation failed: {}", error.message);
                            continue;
                        }
                    }
                    if let Some(probe_name) = request_id
                        .as_deref()
                        .and_then(capability_probes::probe_name_from_request_id)
                    {
                        let probe_name = probe_name.to_owned();
                        self.capability_probe.complete_failed(&probe_name, &error);
                        self.dispatch_next_capability_probe();
                        continue;
                    }
                    // Responses, model discovery, and cancellation failures may
                    // arrive while OAuth is still running. Only the sign-in
                    // request itself may clear its pending UI state.
                    if failed_bridge_command_finishes_sign_in(request_id.as_deref()) {
                        self.sign_in_pending = false;
                    }
                    if request_id.as_deref() == Some("models") {
                        self.model_list_pending = false;
                    }
                    let mut handled_turn = false;
                    if let Some(request_id) = request_id.as_deref() {
                        if self
                            .active_remote_turn
                            .as_ref()
                            .is_some_and(|active| active.request_id == request_id)
                        {
                            let mut active = self
                                .active_remote_turn
                                .take()
                                .expect("matched active remote turn");
                            let detail = format!("{}: {}", error.code, error.message);
                            // A definite HTTP 429 rejection before any output is safe to
                            // retry. A timeout, EOF or failed connection has an unknown
                            // outcome and must NEVER be blindly resubmitted.
                            if error.status == Some(429)
                                && !active.observed_output
                                && active.retries < delivery_telemetry::MAX_RATE_LIMIT_RETRIES
                            {
                                let backoff = Duration::from_secs(1_u64 << (active.retries + 1));
                                active.retries += 1;
                                active.retry_at = Some(Instant::now() + backoff);
                                self.delivery.retry(&active.request_id, backoff, detail);
                                self.remote_status = format!(
                                    "HTTP 429 · retry {} of {} in {}s",
                                    active.retries,
                                    delivery_telemetry::MAX_RATE_LIMIT_RETRIES,
                                    backoff.as_secs(),
                                );
                                self.active_remote_turn = Some(active);
                                continue;
                            }
                            self.delivery.error(
                                &active.request_id,
                                if remote_error_is_observed_failure(&error) {
                                    delivery_telemetry::Phase::Failed
                                } else {
                                    delivery_telemetry::Phase::Unconfirmed
                                },
                                detail.clone(),
                            );
                            if remote_error_is_observed_failure(&error) {
                                let failure = remote_turn_payload(
                                    active.turn_id,
                                    request_id,
                                    None,
                                    (!active.cumulative_text.is_empty())
                                        .then_some(active.cumulative_text.as_str()),
                                    Some(&detail),
                                );
                                let _ = self.queue_turn_event(
                                    active.turn_id,
                                    EventKind::RemoteFailureObserved,
                                    failure,
                                );
                                if active.observed_output {
                                    let interrupted = remote_turn_payload(
                                        active.turn_id,
                                        request_id,
                                        None,
                                        Some(active.cumulative_text.as_str()),
                                        Some(
                                            "assistant output ended before completion after a definitive remote failure",
                                        ),
                                    );
                                    let _ = self.queue_turn_event(
                                        active.turn_id,
                                        EventKind::TransportInterrupted,
                                        interrupted,
                                    );
                                }
                            } else {
                                let interrupted = remote_turn_payload(
                                    active.turn_id,
                                    request_id,
                                    None,
                                    (!active.cumulative_text.is_empty())
                                        .then_some(active.cumulative_text.as_str()),
                                    Some(&detail),
                                );
                                let _ = self.queue_turn_event(
                                    active.turn_id,
                                    EventKind::TransportInterrupted,
                                    interrupted,
                                );
                            }
                            handled_turn = true;
                        }
                    }
                    self.remote_status = format!("{}: {}", error.code, error.message);
                    if handled_turn {
                        self.agent_mcp_flow = None;
                        self.status = "remote turn ended without automatic retry".to_owned();
                    }
                }
                siwc_bridge::BridgeEvent::RuntimeUnavailable(detail) => {
                    if let Some(attempt) = self.openviking_summary_in_flight.take() {
                        self.openviking_summary_blocked.insert(attempt.request_id);
                    }
                    if self.capability_probe.running() {
                        self.capability_probe.abort(&detail);
                        self.finish_capability_probes();
                    }
                    self.remote_runtime_ready = false;
                    self.remote_runtime_failed = true;
                    self.sign_in_requested = false;
                    self.sign_in_pending = false;
                    if let Some(active) = self.active_remote_turn.take() {
                        self.delivery.error(
                            &active.request_id,
                            delivery_telemetry::Phase::Unconfirmed,
                            detail.clone(),
                        );
                        let payload = remote_turn_payload(
                            active.turn_id,
                            &active.request_id,
                            None,
                            (!active.cumulative_text.is_empty())
                                .then_some(active.cumulative_text.as_str()),
                            Some("Sign in with ChatGPT runtime became unavailable"),
                        );
                        let _ = self.queue_turn_event(
                            active.turn_id,
                            EventKind::TransportInterrupted,
                            payload,
                        );
                    }
                    // DispatchAttempted is journaled before the sidecar send.
                    // If its acknowledgment is still pending at bridge death,
                    // clear the intent before that notice can later dispatch
                    // against a dead runtime. Never retry the ambiguous turn.
                    if let Some((turn_id, payload)) =
                        interrupted_pending_remote_turn(&mut self.pending_remote_turn)
                    {
                        self.delivery.error(
                            &turn_id.to_string(),
                            delivery_telemetry::Phase::Unconfirmed,
                            detail.clone(),
                        );
                        let _ = self.queue_turn_event(
                            turn_id,
                            EventKind::TransportInterrupted,
                            payload,
                        );
                    }
                    self.remote_status = detail;
                }
                siwc_bridge::BridgeEvent::CommandSucceeded { .. } => {}
            }
        }
    }
}
