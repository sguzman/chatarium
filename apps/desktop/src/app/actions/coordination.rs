//! coordination user actions extracted without changing their behavior.
use super::super::super::*;

impl ChatariumApp {
    /// A diagnostic of the latest non-authored wake. Completion means model
    /// output finished, not that a worker sent a separate coordination reply.
    pub(crate) fn latest_coordination_wake_status(&self) -> Option<String> {
        let conversation = self.local_conversation_id.to_string();
        let latest = self
            .events
            .iter()
            .rev()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .find(|payload| payload["conversation_id"].as_str() == Some(conversation.as_str()))?;
        let request_id = latest["turn_id"].as_str()?;
        let ids = latest["inbox_ids"]
            .as_array()?
            .iter()
            .filter_map(Value::as_u64)
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = self
            .delivery
            .for_conversation(&conversation)
            .find(|attempt| attempt.request_id == request_id)
            .map(|attempt| match attempt.phase {
                delivery_telemetry::Phase::Durable => "activation saved; dispatch pending",
                delivery_telemetry::Phase::BridgeQueued => "request queued to local bridge",
                delivery_telemetry::Phase::WireAttempted => "HTTP attempted; receipt unconfirmed",
                delivery_telemetry::Phase::Acknowledged => "server acknowledged; awaiting model",
                delivery_telemetry::Phase::Streaming => "model producing output",
                delivery_telemetry::Phase::RetryScheduled => "rate-limit retry scheduled",
                delivery_telemetry::Phase::Complete => {
                    "model replied; separate coordination reply unverified"
                }
                delivery_telemetry::Phase::Failed => "request failed",
                delivery_telemetry::Phase::Unconfirmed => "delivery uncertain; not resent",
            })
            .unwrap_or("recorded earlier or not dispatched this session");
        Some(format!("Last auto-wake [{ids}]: {outcome}"))
    }

    /// Operator-armed only for this open chat. Each incoming directive must
    /// receive a durable, at-most-once claim before a model turn is journaled.
    pub(crate) fn maybe_wake_open_coordination_worker(&mut self) {
        if self.coordination_auto_wake_targets.is_empty()
            || self.selected_historical_conversation.is_some()
            || self.selected_remote_catalog_id.is_some()
            || self.coordination_auto_wake_count >= 8
            || self.local_conversation_busy()
            || !self.remote_connected()
            || self.capability_probe.running()
        {
            return;
        }
        // Serialized, deterministic scheduling; every worker uses its own
        // saved model and behavior, never the currently displayed chat's.
        let candidates = self
            .coordination_auto_wake_targets
            .iter()
            .filter_map(|&worker| {
                if self.local_conversation_catalog.entry(worker)?.archived {
                    return None;
                }
                if worker == self.local_conversation_id && !self.draft.trim().is_empty() {
                    return None;
                }
                let model = if worker == self.local_conversation_id {
                    self.selected_model.clone()?
                } else {
                    self.inference_settings.for_conversation(worker).model?
                };
                if !self
                    .remote_models
                    .iter()
                    .any(|available| available.slug == model)
                {
                    return None;
                }
                let profile = if worker == self.local_conversation_id {
                    self.conversation_behavior_profile.clone()
                } else {
                    self.behavior_profiles.for_conversation(worker)
                };
                let gate = context_composer::CapabilityGate::from_contract(
                    self.local_inference_contract.as_ref(),
                    self.remote_session.profile_id.as_deref(),
                    Some(model.as_str()),
                );
                if profile.request_patch(&gate).is_err() {
                    return None;
                }
                self.coordination_workspace
                    .next_eligible_wake_id(&worker.to_string())
                    .map(|message_id| (worker, message_id))
            })
            .collect::<Vec<_>>();
        let Some(target) = coordination_wake_scheduler::oldest_candidate(&candidates) else {
            return;
        };
        let target_message_id = candidates
            .iter()
            .find_map(|(worker, message_id)| (*worker == target).then_some(*message_id))
            .expect("selected wake candidate must have its message ID");
        // Do not spend the durable at-most-once wake claim while authorized
        // OpenViking reads for this exact worker are still in flight. Each
        // read runs off the egui thread. An offline backend uses backoff and
        // the wake proceeds without invented historical memory.
        let directive = match self
            .coordination_workspace
            .wake_dispatch_envelope(&target.to_string(), target_message_id)
        {
            Ok(exact) => exact.model_context(),
            Err(_) => return,
        };
        if !self.background_worker_memory_ready(target, &directive) {
            return;
        }
        let conversation = target.to_string();
        let path = self.coordination_store_path();
        let claim = match self
            .coordination_workspace
            .claim_next_wake_durable(&path, &conversation)
        {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(error) => {
                self.status = format!("coordination wake claim not saved: {error}");
                self.coordination_auto_wake_targets.remove(&target);
                return;
            }
        };
        // The saved claim remains spent if journal delivery becomes uncertain.
        // It is never silently replayed or reissued as a second remote request.
        let inbox_ids = vec![claim.message_id];
        let turn_id = LocalTurnId::new();
        let payload = serde_json::json!({
            "schema": "chatarium-coordination-activation",
            "version": 1,
            "conversation_id": conversation,
            "turn_id": turn_id.to_string(),
            "inbox_ids": inbox_ids,
            "not_user_authored": true
        })
        .to_string();
        self.coordination_pending_wake = Some((turn_id, target, inbox_ids.clone()));
        if self.queue_turn_event(turn_id, EventKind::CoordinationActivationStarted, payload) {
            self.coordination_auto_wake_count += 1;
            self.status = format!(
                "journaling non-authored coordination wake for {} message(s)",
                inbox_ids.len()
            );
        } else {
            self.coordination_pending_wake = None;
            self.coordination_auto_wake_targets.remove(&target);
        }
    }

    pub(crate) fn dispatch_durable_coordination_wake(&mut self, turn_id: LocalTurnId, start: &EventEnvelope) {
        let Some((pending_id, conversation_id, inbox_ids)) = self.coordination_pending_wake.take()
        else {
            return;
        };
        if pending_id != turn_id
            || !self
                .coordination_auto_wake_targets
                .contains(&conversation_id)
        {
            self.status =
                "coordination wake target changed or disarmed; dispatch denied".to_owned();
            return;
        }
        let [message_id] = inbox_ids.as_slice() else {
            self.status = "coordination wake requires exactly one claimed directive".to_owned();
            return;
        };
        // Delivery permission and source identity must still be valid now,
        // after the activation event was durably appended.
        let exact = match self
            .coordination_workspace
            .wake_dispatch_envelope(&conversation_id.to_string(), *message_id)
        {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake authority denied: {error}");
                return;
            }
        };
        if self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || !self.remote_connected()
        {
            self.status = "coordination wake persisted; bridge unavailable".to_owned();
            return;
        }
        let selected = conversation_id == self.local_conversation_id;
        let settings = self
            .inference_settings
            .for_conversation_with_mcp_defaults(conversation_id, &self.mcp_agent_defaults);
        let Some(model) = (if selected {
            self.selected_model.clone()
        } else {
            settings.model.clone()
        }) else {
            self.status = "coordination wake model is not configured".to_owned();
            return;
        };
        let profile = if selected {
            self.conversation_behavior_profile.clone()
        } else {
            self.behavior_profiles.for_conversation(conversation_id)
        };
        let gate = context_composer::CapabilityGate::from_contract(
            self.local_inference_contract.as_ref(),
            self.remote_session.profile_id.as_deref(),
            Some(&model),
        );
        let patch = match profile.request_patch(&gate) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake policy rejected: {error}");
                return;
            }
        };
        let mut transcript = context_transcript(&projected_local_display_messages(
            &self.events,
            conversation_id,
        ));
        let tools = match admitted_tool_result_context_messages(&self.events, conversation_id) {
            Ok(tools) => tools,
            Err(error) => {
                self.status = format!("coordination wake tool evidence invalid: {error}");
                return;
            }
        };
        let frozen_sources = tools
            .iter()
            .map(|item| item.source.clone())
            .collect::<Vec<_>>();
        transcript.extend(tools);
        match admitted_routed_context_messages(&self.events, conversation_id) {
            Ok(context) => transcript.extend(context),
            Err(error) => {
                self.status = format!("coordination wake routed context invalid: {error}");
                return;
            }
        }
        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
        // Freeze the exact worker-owned and currently granted project memory
        // at wake dispatch. Never use the displayed conversation's cache or
        // borrow the controller's authorization for an inactive worker.
        transcript.extend(
            self.openviking_project_cache.admitted_relevant_context(
                self.openviking_retrieval_cache
                    .get(&conversation_id)
                    .map(Vec::as_slice),
                conversation_id,
                &self.local_conversation_catalog,
                &self.project_memory_grants,
                &exact.model_context(),
            ),
        );
        let mut activation = context_composer::TranscriptMessage::coordination_activation(
            turn_id.to_string(),
            start.sequence,
            inbox_ids.clone(),
        );
        // User-level context, never a developer/system instruction. This
        // includes the exact claimed directive even when the regular inbox
        // preview has excluded it because later messages filled that window.
        activation.text.push_str("\n\n");
        activation.text.push_str(&exact.model_context());
        transcript.push(activation);
        let mut developer = if selected {
            self.conversation_developer_context.clone()
        } else {
            settings.developer_context.clone()
        };
        developer.push_str(
            &self
                .coordination_workspace
                .identity_context(&conversation_id.to_string()),
        );
        let instructions = if selected {
            &self.conversation_instructions
        } else {
            &settings.instructions
        };
        let plan = match context_composer::ContextPlan::compose_bounded(
            context_composer::ContextPolicy::dispatch(),
            instructions,
            &developer,
            transcript,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = format!("coordination wake context budget rejected: {error}");
                return;
            }
        };
        let manifest = match mcp_dispatch_manifest::capture(
            conversation_id,
            "coordination_auto_wake",
            &frozen_sources,
            &plan,
        ) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("coordination wake manifest rejected: {error}");
                return;
            }
        };
        let request_id = turn_id.to_string();
        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
            remote_turn_payload(
                turn_id,
                &request_id,
                Some(&model),
                None,
                Some("non-authored coordination inbox wake"),
            ),
            manifest,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                self.status = format!("coordination wake context rejected: {error}");
                return;
            }
        };
        self.coordination_pending_admissions
            .insert(request_id.clone(), inbox_ids);
        self.delivery
            .begin(conversation_id.to_string(), request_id.clone());
        self.pending_remote_turn = Some(PendingRemoteTurn {
            conversation_id,
            turn_id,
            request_id,
            model,
            input: plan.input_json(),
            instructions: plan.instructions,
            request_patch: patch,
        });
        if self.queue_turn_event(turn_id, EventKind::DispatchAttempted, payload) {
            self.status = "coordination wake journaled; remote dispatch queued".to_owned();
        } else {
            self.coordination_pending_admissions
                .remove(&turn_id.to_string());
            self.pending_remote_turn = None;
        }
    }

    /// Authenticate the inactive worker identity against its persisted
    /// activation turn and still-active exact claimed directive.
    pub(crate) fn is_authorized_background_wake_turn(
        &self,
        conversation_id: LocalConversationId,
        turn_id: LocalTurnId,
    ) -> bool {
        if !self
            .coordination_auto_wake_targets
            .contains(&conversation_id)
        {
            return false;
        }
        let conversation = conversation_id.to_string();
        let turn = turn_id.to_string();
        self.events
            .iter()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .any(|payload| {
                payload["conversation_id"].as_str() == Some(conversation.as_str())
                    && payload["turn_id"].as_str() == Some(turn.as_str())
                    && payload["inbox_ids"].as_array().is_some_and(|ids| {
                        ids.len() == 1
                            && ids[0].as_u64().is_some_and(|message_id| {
                                self.coordination_workspace
                                    .validate_wake_claim(&conversation, message_id)
                                    .is_ok()
                            })
                    })
            })
    }

    /// Terminal evidence has already been appended to the main event journal
    /// when this method runs. Only now may an active, saved wake claim be
    /// cleared; an unknown transport outcome still consumes its allowance.
    pub(crate) fn finish_durable_coordination_wake(&mut self, turn_id: LocalTurnId) {
        // The first model completion may be intermediate if an authorized
        // coordination tool requires a second Responses request.
        if self
            .pending_remote_turn
            .as_ref()
            .is_some_and(|pending| pending.turn_id == turn_id)
            || self
                .active_remote_turn
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id)
            || self
                .agent_mcp_flow
                .as_ref()
                .is_some_and(|flow| flow.turn_id == turn_id)
        {
            return;
        }
        let wake = self
            .events
            .iter()
            .rev()
            .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
            .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
            .find(|value| value["turn_id"].as_str() == Some(turn_id.to_string().as_str()));
        let Some(wake) = wake else {
            return;
        };
        let (Some(conversation), Some(message_id)) = (
            wake["conversation_id"].as_str(),
            wake["inbox_ids"]
                .as_array()
                .and_then(|ids| (ids.len() == 1).then(|| ids[0].as_u64()).flatten()),
        ) else {
            return;
        };
        if !self.coordination_workspace.wake_leases.iter().any(|lease| {
            lease.conversation == conversation && lease.active_message_id == Some(message_id)
        }) {
            return;
        }
        let path = self.coordination_store_path();
        if let Err(error) =
            self.coordination_workspace
                .finish_wake_claim_durable(&path, conversation, message_id)
        {
            self.status = format!("coordination wake terminal saved but claim not closed: {error}");
            self.coordination_auto_wake_targets.clear();
        }
    }

}
