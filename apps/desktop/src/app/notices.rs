//! Durable journal notice handling; no changes to original ordered match logic.
use super::super::*;

impl ChatariumApp {
    pub(crate) fn process_notices(&mut self) {
        let notices = self
            .notice_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for notice in notices {
            match notice {
                PersistNotice::ProjectOpenVikingRetrievalFinished {
                    conversation_id,
                    generation,
                    result,
                } => {
                    let result = self.openviking_project_cache.finish(
                        conversation_id,
                        generation,
                        result,
                        Instant::now(),
                        &self.local_conversation_catalog,
                        &self.project_memory_grants,
                    );
                    if let openviking_project_cache::Completion::Deferred(error) = result {
                        if conversation_id == self.local_conversation_id {
                            self.status = format!(
                                "Project memory retrieval unavailable; continuing without shared memory: {error}"
                            );
                        }
                    }
                }
                PersistNotice::OpenVikingRetrievalFinished {
                    conversation_id,
                    result,
                } => {
                    self.openviking_retrieval_in_flight.remove(&conversation_id);
                    if self.openviking_retrieval_stale.remove(&conversation_id) {
                        // A checkpoint was confirmed while this read was in
                        // flight; its old snapshot cannot refresh the cache.
                        self.openviking_retrieval_cache.remove(&conversation_id);
                        self.openviking_retrieval_retry_after
                            .remove(&conversation_id);
                        continue;
                    }
                    match result {
                        Ok(checkpoints) => {
                            self.openviking_retrieval_cache
                                .insert(conversation_id, checkpoints);
                            self.openviking_retrieval_retry_after
                                .remove(&conversation_id);
                        }
                        Err(error) => {
                            // The conversation remains usable. Retry after a
                            // bounded delay rather than on every UI frame.
                            self.openviking_retrieval_retry_after
                                .insert(conversation_id, Instant::now() + Duration::from_secs(300));
                            if conversation_id == self.local_conversation_id {
                                self.status = format!(
                                    "OpenViking retrieval unavailable; continuing without memory: {error}"
                                );
                            }
                        }
                    }
                }
                PersistNotice::OpenVikingMemoryWriteFinished {
                    conversation_id,
                    confirmed,
                } => {
                    if confirmed {
                        self.invalidate_project_memory_cache();
                        // Invalidate any conversation cache affected by the
                        // newly confirmed immutable checkpoint.
                        let affected = self
                            .openviking_retrieval_cache
                            .keys()
                            .chain(self.openviking_retrieval_retry_after.keys())
                            .chain(self.openviking_retrieval_in_flight.iter())
                            .copied()
                            .filter(|id| id.to_string() == conversation_id)
                            .collect::<Vec<_>>();
                        for owner in affected {
                            self.openviking_retrieval_cache.remove(&owner);
                            self.openviking_retrieval_retry_after.remove(&owner);
                            if self.openviking_retrieval_in_flight.contains(&owner) {
                                self.openviking_retrieval_stale.insert(owner);
                            }
                        }
                    }
                    if self.local_conversation_id.to_string() == conversation_id {
                        self.status = if confirmed {
                            "OpenViking memory checkpoint saved and verified by read-back"
                                .to_owned()
                        } else {
                            "OpenViking checkpoint write uncertain; not automatically repeated"
                                .to_owned()
                        };
                        self.openviking_checkpoint_status = self.status.clone();
                    }
                }
                PersistNotice::OpenVikingMemorySummarySaved {
                    conversation_id,
                    turn_id,
                    skipped,
                } => {
                    if self
                        .openviking_summary_in_flight
                        .as_ref()
                        .is_some_and(|attempt| {
                            attempt.conversation_id == conversation_id && attempt.turn_id == turn_id
                        })
                    {
                        self.openviking_summary_in_flight = None;
                    }
                    if conversation_id == self.local_conversation_id {
                        self.status = if skipped {
                            "OpenViking checkpoint skipped: nothing durable to retain".to_owned()
                        } else {
                            "OpenViking summary durable locally; scheduling verified external write"
                                .to_owned()
                        };
                        self.openviking_checkpoint_status = self.status.clone();
                    }
                    if !skipped
                        && std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() != Ok("0")
                    {
                        if let Some(sender) = &self.persist_tx {
                            let _ = sender.send(PersistCommand::DispatchOpenVikingReadyWrite);
                        }
                    }
                }
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
                                continue;
                            }
                            let remote_request_id = message.turn_id.to_string();
                            let mut transcript =
                                context_transcript(&projected_local_display_messages(
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
                                        continue;
                                    }
                                }
                            }
                            transcript.extend(intent.controller_result_context);
                            transcript.extend(intent.controller_coordination_result_context);
                            transcript
                                .sort_by_key(context_composer::TranscriptMessage::order_sequence);
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
                                    continue;
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
                                    continue;
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
                                    self.status =
                                        format!("cannot journal outgoing context: {error}");
                                    continue;
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
                                        continue;
                                    }
                                };
                            }
                            let payload = if let Some(attachment) = intent.attachment.as_ref() {
                                let mut evidence: Value = serde_json::from_str(&payload)
                                    .expect("validated dispatch observation JSON");
                                evidence["details"]["attachment_intake"] =
                                    attachment.timing_metadata();
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
                                    Some(
                                        "wake denied before bridge send; no network request issued",
                                    ),
                                );
                                let _ = self.queue_turn_event(
                                    turn_id,
                                    EventKind::TransportInterrupted,
                                    interrupted,
                                );
                                self.status =
                                    "coordination wake stopped before remote send".to_owned();
                                continue;
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
                                self.delivery.set(
                                    &pending.request_id,
                                    delivery_telemetry::Phase::BridgeQueued,
                                );
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
                PersistNotice::HistoricalConversationLoaded {
                    local_conversation_id,
                    imported_sequence,
                    messages,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_messages =
                            historical_display_messages(messages, imported_sequence);
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.historical_load_pending = None;
                        self.status = format!(
                            "verified historical snapshot loaded from import event #{imported_sequence}"
                        );
                    }
                }
                PersistNotice::HistoricalConversationLoadFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = None;
                        self.historical_messages.clear();
                        self.status = format!("historical snapshot load failed: {error}");
                    }
                }
                PersistNotice::LiveConversationLoaded {
                    local_conversation_id,
                    snapshot_sequence,
                    truncated_before,
                    messages,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_messages =
                            remote_display_messages(messages, snapshot_sequence);
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.historical_load_pending = None;
                        self.live_mirror_truncated_before = truncated_before;
                        self.status = format!(
                            "validated live mirror loaded from remote snapshot event #{snapshot_sequence}"
                        );
                    }
                }
                PersistNotice::LiveConversationLoadFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = None;
                        self.historical_messages.clear();
                        self.status = format!("live mirror snapshot load failed: {error}");
                    }
                }
                PersistNotice::HistoricalLiveMirrorPromoted {
                    local_conversation_id,
                    snapshot_sequence,
                    appended_events,
                    truncated_before,
                    messages,
                    projection_error,
                } => {
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }

                    match self.pending_history_fetch_proof.take() {
                        Some(pending)
                            if pending.local_conversation_id == Some(local_conversation_id) =>
                        {
                            self.mirror_status = format!(
                                "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                                if truncated_before {
                                    "MIRRORED PARTIAL"
                                } else {
                                    "MIRRORED COMPLETE"
                                },
                                browser_proof_label(&pending.proof),
                                pending.http_status,
                                snapshot_sequence,
                            );
                        }
                        Some(pending) => {
                            self.mirror_status = format!(
                                "FAILED · durable event #{} has no matching browser proof (pending remote id {})",
                                snapshot_sequence, pending.remote_conversation_id,
                            );
                        }
                        None => {
                            self.mirror_status = format!(
                                "MIRRORED · durable event #{} committed without a current browser proof chain",
                                snapshot_sequence,
                            );
                        }
                    }

                    if self.selected_historical_conversation == Some(local_conversation_id) {
                        self.historical_load_pending = None;
                        self.loaded_historical_conversation = Some(local_conversation_id);
                        self.live_mirror_truncated_before = truncated_before;
                        if let Some(error) = projection_error {
                            self.historical_messages.clear();
                            self.status = format!(
                                "live mirror is durable, but visible transcript projection is blocked: {error}"
                            );
                        } else {
                            self.historical_messages =
                                remote_display_messages(messages, snapshot_sequence);
                            self.status = format!(
                                "ChatGPT live mirror durable at event #{snapshot_sequence}"
                            );
                        }
                    } else {
                        self.status =
                            format!("ChatGPT live mirror durable at event #{snapshot_sequence}");
                    }
                }
                PersistNotice::HistoricalLiveMirrorPromotionFailed {
                    local_conversation_id,
                    error,
                } => {
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                    self.status = format!("live mirror promotion failed: {error}");
                }
                PersistNotice::DiscoveredLiveMirrorPromoted {
                    local_conversation_id,
                    remote_conversation_id,
                    snapshot_sequence,
                    appended_events,
                    truncated_before,
                    messages,
                    projection_error,
                } => {
                    diagnostics::info(
                        "mirror",
                        format!(
                            "durable mirror notice remote={} local={local_conversation_id} event=#{snapshot_sequence} partial={truncated_before}",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.selected_remote_catalog_id = None;
                    self.selected_historical_conversation = Some(local_conversation_id);
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.historical_load_pending = None;
                    self.live_mirror_truncated_before = truncated_before;

                    self.remote_mirror_failures.remove(&remote_conversation_id);
                    self.remote_mirror_retry_after
                        .remove(&remote_conversation_id);
                    match self.pending_history_fetch_proof.take() {
                        Some(pending)
                            if pending.local_conversation_id.is_none()
                                && pending.remote_conversation_id == remote_conversation_id =>
                        {
                            self.mirror_status = format!(
                                "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                                if truncated_before {
                                    "MIRRORED PARTIAL"
                                } else {
                                    "MIRRORED COMPLETE"
                                },
                                browser_proof_label(&pending.proof),
                                pending.http_status,
                                snapshot_sequence,
                            );
                        }
                        Some(pending) => {
                            self.mirror_status = format!(
                                "FAILED · durable event #{} did not match pending proof for remote id {}",
                                snapshot_sequence, pending.remote_conversation_id,
                            );
                        }
                        None => {
                            self.mirror_status = format!(
                                "MIRRORED · durable event #{} committed without a current browser proof chain",
                                snapshot_sequence,
                            );
                        }
                    }

                    if let Some(error) = projection_error {
                        self.historical_messages.clear();
                        self.status = format!(
                            "live mirror is durable, but visible transcript projection is blocked: {error}"
                        );
                    } else {
                        self.historical_messages =
                            remote_display_messages(messages, snapshot_sequence);
                        self.status = format!(
                            "remote ChatGPT conversation mirrored locally at event #{snapshot_sequence}"
                        );
                    }
                }
                PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                    remote_conversation_id,
                    error,
                } => {
                    diagnostics::error(
                        "mirror",
                        format!(
                            "durable mirror commit failed remote={}: {error}",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    if self.remote_discovery_pending.as_deref()
                        == Some(remote_conversation_id.as_str())
                    {
                        self.remote_discovery_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.remote_mirror_failures
                        .insert(remote_conversation_id.clone(), error.clone());
                    self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                    self.status = format!("remote mirror creation failed: {error}");
                }
                PersistNotice::MirrorQueueUpdated { appended_events } => {
                    self.events.extend(appended_events);
                    self.refresh_live_mirror_catalog();
                    self.status = "production mirror queue state durably updated".to_owned();
                }
                PersistNotice::RemoteHealthUpdated { event, controller } => {
                    self.events.push(event);
                    self.remote_health = controller;
                    self.status = format!(
                        "remote health: {} · mirror intent: {}",
                        self.remote_health.state.as_str(),
                        self.remote_health.intent.as_str()
                    );
                }
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
                        continue;
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
                PersistNotice::LocalMemoryArtifactRecorded { memory_id, event } => {
                    self.events.push(event);
                    self.local_memory_command_pending = false;
                    self.local_memory_draft.clear();
                    self.staged_local_memory_origin = None;
                    self.status = format!("local memory {} durably recorded", memory_id.get());
                }
                PersistNotice::LocalMemoryContextDecisionUpdated { memory_id, event } => {
                    self.events.push(event);
                    self.local_memory_command_pending = false;
                    if let Ok(records) = replay_local_memory_context_audit(&self.events) {
                        for record in records.into_iter().filter(|record| {
                            record.memory_id == memory_id
                                && record.decision == LocalMemoryContextDecision::Admit
                        }) {
                            if let Some(selected) = self
                                .local_memory_one_shot_selections
                                .get_mut(&record.destination_conversation_id)
                            {
                                selected.remove(&memory_id);
                            }
                        }
                        self.local_memory_one_shot_selections
                            .retain(|_, selected| !selected.is_empty());
                    }
                    self.status = format!(
                        "local memory {} context eligibility durably updated",
                        memory_id.get()
                    );
                }
                PersistNotice::LocalMemorySupersessionRecorded {
                    predecessor_memory_id,
                    successor_memory_id,
                    event,
                } => {
                    self.events.push(event);
                    self.local_memory_command_pending = false;
                    for selected in self.local_memory_one_shot_selections.values_mut() {
                        selected.remove(&predecessor_memory_id);
                    }
                    self.local_memory_one_shot_selections
                        .retain(|_, selected| !selected.is_empty());
                    self.status = format!(
                        "local memory {} durably superseded by {}",
                        predecessor_memory_id.get(),
                        successor_memory_id.get(),
                    );
                }
                PersistNotice::LocalMemoryLabelUpdated { memory_id, event } => {
                    let added = event.kind == EventKind::LocalMemoryLabelAdded;
                    self.events.push(event);
                    self.local_memory_command_pending = false;
                    if added {
                        self.local_memory_label_drafts.remove(&memory_id);
                    }
                    self.status =
                        format!("local memory {} labels durably updated", memory_id.get());
                }
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
            }
        }
    }
}
