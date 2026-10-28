//! Feature-specific durable completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_durable_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::DraftSaved {
                conversation_id,
                revision,
                event,
            } => {
                self.events.push(event);
                if conversation_id == self.local_conversation_id {
                    self.saved_revision = self.saved_revision.max(revision);
                    if self.saved_revision == self.draft_revision
                        && self.commit_in_flight.is_none()
                        && self.active_remote_turn.is_none()
                    {
                        self.status = "draft durable".to_owned();
                    }
                }
            }
            PersistNotice::MessageCommitted {
                request_id,
                message,
                event,
                one_shot_memory_event,
            } => {
                let sequence = event.sequence;
                let committed_attachment = decode_user_message_attachment(&event);
                let one_shot_selection_sequence =
                    one_shot_memory_event.as_ref().map(|event| event.sequence);
                self.events.push(event);
                if let Some(event) = one_shot_memory_event {
                    self.events.push(event);
                    self.local_memory_one_shot_selections
                        .remove(&message.conversation_id);
                }
                if self.commit_in_flight == Some(request_id) {
                    self.commit_in_flight = None;
                    self.evidence.commit_local_message();
                    self.draft.clear();
                    self.staged_attachment = None;

                    if let Some(intent) = self.commit_remote_intents.remove(&request_id) {
                        let expected_attachment = intent
                            .attachment
                            .as_ref()
                            .map(attachment_intake::PreparedAttachment::durable_record);
                        if committed_attachment.as_ref().ok() != Some(&expected_attachment) {
                            self.status = "durable attachment evidence does not match frozen send; remote dispatch blocked".to_owned();
                            return;
                        }
                        let remote_request_id = message.turn_id.to_string();
                        let mut transcript = context_transcript(&projected_local_display_messages(
                            &self.events,
                            message.conversation_id,
                        ));
                        transcript.extend(intent.routed_context);
                        // The Send-click admission snapshot is frozen in this
                        // pending intent. Do not re-read live admissions after
                        // the durable user-message commit.
                        let frozen_tool_sources = intent
                            .tool_result_context
                            .iter()
                            .map(|item| item.source.clone())
                            .collect::<Vec<_>>();
                        transcript.extend(intent.tool_result_context);
                        transcript.extend(intent.local_memory_context);
                        if one_shot_selection_sequence.is_some() {
                            match one_shot_local_memory_messages_for_turn(
                                &self.events,
                                message.turn_id,
                            ) {
                                Ok(one_shot) => transcript.extend(one_shot),
                                Err(error) => {
                                    self.status = format!(
                                        "cannot compose durable one-shot local memory: {error}"
                                    );
                                    return;
                                }
                            }
                        }
                        transcript.extend(intent.controller_result_context);
                        transcript.extend(intent.controller_coordination_result_context);
                        transcript.sort_by_key(context_composer::TranscriptMessage::order_sequence);
                        let context_plan = match context_composer::ContextPlan::compose_bounded(
                            context_composer::ContextPolicy::dispatch(),
                            intent.instructions.as_deref().unwrap_or_default(),
                            intent.developer_context.as_str(),
                            transcript,
                        ) {
                            Ok(plan) => plan,
                            Err(error) => {
                                self.status =
                                    format!("cannot compose bounded outgoing context: {error}");
                                return;
                            }
                        };
                        let manifest = match mcp_dispatch_manifest::capture(
                            message.conversation_id,
                            "authored",
                            &frozen_tool_sources,
                            &context_plan,
                        ) {
                            Ok(manifest) => manifest,
                            Err(error) => {
                                self.status =
                                    format!("cannot record exact outgoing context: {error}");
                                return;
                            }
                        };
                        let payload = match mcp_dispatch_manifest::attach_to_dispatch_payload(
                            remote_turn_payload(
                                message.turn_id,
                                &remote_request_id,
                                Some(&intent.model),
                                None,
                                None,
                            ),
                            manifest,
                        ) {
                            Ok(payload) => payload,
                            Err(error) => {
                                self.status = format!("cannot journal outgoing context: {error}");
                                return;
                            }
                        };
                        let mut input = context_plan.input_json();
                        if let Some(attachment) = intent.attachment.as_ref() {
                            input = match attachment_intake::attach_to_authored_input(
                                input,
                                &message.text,
                                attachment.response_content(),
                            ) {
                                Ok(input) => input,
                                Err(error) => {
                                    self.status = format!(
                                        "durable attachment input cannot be composed: {error}"
                                    );
                                    return;
                                }
                            };
                        }
                        let payload = if let Some(attachment) = intent.attachment.as_ref() {
                            let mut evidence: Value = serde_json::from_str(&payload)
                                .expect("validated dispatch observation JSON");
                            evidence["details"]["attachment_intake"] = attachment.timing_metadata();
                            serde_json::to_string(&evidence)
                                .expect("bounded attachment metadata serializes")
                        } else {
                            payload
                        };
                        self.delivery.begin(
                            message.conversation_id.to_string(),
                            remote_request_id.clone(),
                        );
                        self.coordination_pending_admissions
                            .insert(remote_request_id.clone(), intent.coordination_ids);
                        self.pending_remote_turn = Some(PendingRemoteTurn {
                            conversation_id: message.conversation_id,
                            turn_id: message.turn_id,
                            request_id: remote_request_id.clone(),
                            model: intent.model.clone(),
                            input,
                            instructions: context_plan.instructions.clone(),
                            request_patch: intent.request_patch,
                        });
                        if self.queue_turn_event(
                            message.turn_id,
                            EventKind::DispatchAttempted,
                            payload,
                        ) {
                            self.status = format!(
                                "message durable as event #{sequence}; preparing ChatGPT dispatch"
                            );
                        } else {
                            self.coordination_pending_admissions
                                .remove(&remote_request_id);
                            self.pending_remote_turn = None;
                        }
                    } else {
                        self.status =
                            format!("user message durably committed as event #{sequence}");
                    }

                    self.queue_draft_snapshot();
                }
            }
            PersistNotice::AgentActivityAppended { event } => {
                self.events.push(event);
            }
            PersistNotice::MemoryCheckpointQueued {
                conversation_id,
                pending_count,
            } => {
                if conversation_id == self.local_conversation_id {
                    self.status = format!(
                        "memory checkpoint queued locally; {pending_count} awaiting summarization"
                    );
                }
            }
            PersistNotice::TurnEventAppended {
                turn_id,
                kind,
                event,
            } => {
                self.events.push(event.clone());
                if matches!(
                    kind,
                    EventKind::AssistantCompletionObserved
                        | EventKind::RemoteFailureObserved
                        | EventKind::TransportInterrupted
                ) {
                    self.finish_durable_coordination_wake(turn_id);
                }
                if kind == EventKind::CoordinationActivationStarted {
                    self.dispatch_durable_coordination_wake(turn_id, &event);
                }
                if kind == EventKind::DispatchAttempted
                    && self
                        .pending_remote_turn
                        .as_ref()
                        .is_some_and(|pending| pending.turn_id == turn_id)
                {
                    let pending = self
                        .pending_remote_turn
                        .take()
                        .expect("checked pending remote turn");
                    // The operator may stop or revoke authority after the
                    // DispatchAttempted journal event but before the bridge
                    // command actually leaves this process. Recheck at the
                    // final send boundary, not only during context assembly.
                    let wake_start = self
                        .events
                        .iter()
                        .rev()
                        .filter(|event| event.kind == EventKind::CoordinationActivationStarted)
                        .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
                        .find(|value| {
                            value["turn_id"].as_str() == Some(turn_id.to_string().as_str())
                        });
                    if let Some(start) = wake_start {
                        let allowed = start["inbox_ids"]
                            .as_array()
                            .and_then(|ids| (ids.len() == 1).then(|| ids[0].as_u64()).flatten())
                            .is_some_and(|message_id| {
                                self.coordination_auto_wake_targets
                                    .contains(&pending.conversation_id)
                                    && self
                                        .coordination_workspace
                                        .wake_dispatch_envelope(
                                            &pending.conversation_id.to_string(),
                                            message_id,
                                        )
                                        .is_ok()
                            });
                        if !allowed {
                            self.coordination_pending_admissions
                                .remove(&pending.request_id);
                            self.delivery.error(
                                &pending.request_id,
                                delivery_telemetry::Phase::Failed,
                                "wake disarmed or sender authority revoked before bridge send"
                                    .to_owned(),
                            );
                            let interrupted = remote_turn_payload(
                                turn_id,
                                &pending.request_id,
                                Some(&pending.model),
                                None,
                                Some("wake denied before bridge send; no network request issued"),
                            );
                            let _ = self.queue_turn_event(
                                turn_id,
                                EventKind::TransportInterrupted,
                                interrupted,
                            );
                            self.status = "coordination wake stopped before remote send".to_owned();
                            return;
                        }
                    }
                    let command = siwc_bridge::BridgeCommand::StreamResponse {
                        request_id: pending.request_id.clone(),
                        model: pending.model.clone(),
                        input: pending.input,
                        instructions: pending.instructions,
                        request_patch: pending.request_patch,
                    };
                    let retry_command = command.clone();
                    match self.remote.send(command) {
                        Ok(()) => {
                            if let Some(ids) = self
                                .coordination_pending_admissions
                                .remove(&pending.request_id)
                            {
                                if !ids.is_empty() {
                                    let before = self.coordination_workspace.clone();
                                    let changed = self.coordination_workspace.consume_ids(
                                        &pending.conversation_id.to_string(),
                                        &ids,
                                        unix_now_ms(),
                                    );
                                    if changed != 0 {
                                        if let Err(error) = self
                                            .coordination_workspace
                                            .save_atomic(&self.coordination_store_path())
                                        {
                                            self.coordination_workspace = before;
                                            self.status = format!(
                                                "Coordination consumption persistence failed: {error}"
                                            );
                                        }
                                    }
                                }
                            }
                            self.delivery
                                .set(&pending.request_id, delivery_telemetry::Phase::BridgeQueued);
                            self.active_remote_turn = Some(ActiveRemoteTurn {
                                conversation_id: pending.conversation_id,
                                turn_id,
                                request_id: pending.request_id,
                                cumulative_text: String::new(),
                                observed_output: false,
                                source_suspect_logged: false,
                                command: retry_command,
                                retries: 0,
                                retry_at: None,
                            });
                        }
                        Err(error) => {
                            self.coordination_pending_admissions
                                .remove(&pending.request_id);
                            self.delivery.error(
                                &pending.request_id,
                                delivery_telemetry::Phase::Unconfirmed,
                                error.clone(),
                            );
                            let payload = remote_turn_payload(
                                turn_id,
                                &pending.request_id,
                                Some(&pending.model),
                                None,
                                Some("bridge command channel closed before outcome"),
                            );
                            let _ = self.queue_turn_event(
                                turn_id,
                                EventKind::TransportInterrupted,
                                payload,
                            );
                            self.remote_status = error;
                        }
                    }
                }
            }
            _ => unreachable!("unexpected durable notice"),
        }
    }
}
