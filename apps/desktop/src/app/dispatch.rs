//! Desktop dispatch helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn remote_turn_payload(
    turn_id: LocalTurnId,
    request_id: &str,
    model: Option<&str>,
    text: Option<&str>,
    detail: Option<&str>,
) -> String {
    serde_json::to_string(&serde_json::json!({
        "schema": "chatarium-responses-turn-observation",
        "version": 1,
        "text": text,
        "details": {
            "local_turn_id": turn_id.to_string(),
            "request_id": request_id,
            "model": model,
            "detail": detail,
        }
    }))
    .expect("remote turn observation is JSON-serializable")
}

pub(crate) fn interrupted_pending_remote_turn(
    pending_remote_turn: &mut Option<PendingRemoteTurn>,
) -> Option<(LocalTurnId, String)> {
    pending_remote_turn.take().map(|pending| {
        let payload = remote_turn_payload(
            pending.turn_id,
            &pending.request_id,
            Some(&pending.model),
            None,
            Some("Sign in with ChatGPT runtime became unavailable before stream dispatch"),
        );
        (pending.turn_id, payload)
    })
}

pub(crate) fn local_conversation_topology(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Option<LocalConversationTopologyRecord>, String> {
    Ok(replay_local_conversation_topologies(events)?
        .into_iter()
        .find(|topology| topology.conversation_id == conversation_id))
}

pub(crate) fn next_available_local_orchestration_ids(
    events: &[EventEnvelope],
) -> Result<(ChatContainerId, SessionId), String> {
    let next_container = replay_chat_container_audit(events)?
        .into_iter()
        .map(|record| record.container_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "chat-container identity space exhausted".to_owned())?;
    let next_session = replay_session_audit(events)?
        .into_iter()
        .map(|record| record.session_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "session identity space exhausted".to_owned())?;
    Ok((
        ChatContainerId::new(next_container),
        SessionId::new(next_session),
    ))
}

pub(crate) fn next_available_continuation_lease_id(
    events: &[EventEnvelope],
) -> Result<ContinuationLeaseId, String> {
    let highest = replay_continuation_audit(events)?
        .into_iter()
        .map(|record| record.lease_id.get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "continuation lease identity space exhausted".to_owned())?;
    Ok(ContinuationLeaseId::new(next))
}

pub(crate) fn next_available_coordination_suggestion_id(
    events: &[EventEnvelope],
) -> Result<CoordinationSuggestionId, String> {
    let highest = replay_controller_coordination_suggestion_audit(events)?
        .into_iter()
        .map(|record| record.suggestion.id().get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "coordination suggestion identity space exhausted".to_owned())?;
    Ok(CoordinationSuggestionId::new(next))
}

pub(crate) fn next_available_control_id(events: &[EventEnvelope]) -> Result<ControlId, String> {
    let highest = replay_control_audit(events)?
        .into_iter()
        .map(|record| record.control_id.get())
        .max()
        .unwrap_or(0);
    let next = highest
        .checked_add(1)
        .ok_or_else(|| "control identity space exhausted".to_owned())?;
    Ok(ControlId::new(next))
}

pub(crate) fn next_available_tool_provider_id(
    events: &[EventEnvelope],
) -> Result<ToolProviderId, String> {
    let next = replay_tool_provider_audit(events)?
        .into_iter()
        .map(|record| record.provider_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "tool provider identity space exhausted".to_owned())?;
    Ok(ToolProviderId::new(next))
}

pub(crate) fn next_available_tool_call_id(events: &[EventEnvelope]) -> Result<ToolCallId, String> {
    let next = replay_tool_call_audit(events)?
        .into_iter()
        .map(|record| record.call_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "tool call identity space exhausted".to_owned())?;
    Ok(ToolCallId::new(next))
}

pub(crate) fn next_available_local_memory_id(
    events: &[EventEnvelope],
) -> Result<LocalMemoryId, String> {
    let next = replay_local_memory_audit(events)?
        .into_iter()
        .map(|record| record.memory_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "local memory identity space exhausted".to_owned())?;
    Ok(LocalMemoryId::new(next))
}

pub(crate) fn next_available_route_payload_id(
    events: &[EventEnvelope],
) -> Result<RoutePayloadId, String> {
    let next = replay_local_route_payload_audit(events)?
        .into_iter()
        .map(|payload| payload.payload_id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "route payload identity space exhausted".to_owned())?;
    Ok(RoutePayloadId::new(next))
}

pub(crate) fn next_available_route_id(events: &[EventEnvelope]) -> Result<RouteId, String> {
    let next = replay_routing_audit(events)?
        .into_iter()
        .map(|route| route.request.id.get())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "route identity space exhausted".to_owned())?;
    Ok(RouteId::new(next))
}

pub(crate) fn next_available_route_endpoint_id(
    events: &[EventEnvelope],
) -> Result<RouteEndpointId, String> {
    let mut highest = 0_u64;

    for record in replay_session_audit(events)? {
        if let Some(binding) = record.endpoint_binding {
            highest = highest.max(binding.endpoint_id().get());
        }
    }
    for provider in replay_tool_provider_audit(events)? {
        if let Some(binding) = provider.endpoint_binding {
            highest = highest.max(binding.endpoint_id().get());
        }
    }
    for route in replay_routing_audit(events)? {
        highest = highest
            .max(route.request.source.get())
            .max(route.request.destination.get());
    }

    let next = highest
        .checked_add(1)
        .ok_or_else(|| "routing endpoint identity space exhausted".to_owned())?;
    Ok(RouteEndpointId::new(next))
}

pub(crate) fn session_endpoint_binding(
    events: &[EventEnvelope],
    session_id: SessionId,
) -> Result<Option<SessionEndpointBinding>, String> {
    Ok(replay_session_audit(events)?
        .into_iter()
        .find(|record| record.session_id == session_id)
        .and_then(|record| record.endpoint_binding))
}

pub(crate) fn controller_worker_result_context_decision_label(
    decision: ControllerWorkerResultContextDecision,
) -> &'static str {
    match decision {
        ControllerWorkerResultContextDecision::Admit => "admit",
        ControllerWorkerResultContextDecision::Exclude => "exclude",
    }
}

pub(crate) fn coordination_suggestion_action_label(
    action: CoordinationSuggestionAction,
) -> &'static str {
    match action {
        CoordinationSuggestionAction::StartOrResume => "START/RESUME",
        CoordinationSuggestionAction::Continue => "CONTINUE",
        CoordinationSuggestionAction::Stop => "STOP",
        CoordinationSuggestionAction::StatusRequest => "STATUS",
    }
}

pub(crate) fn controller_coordination_result_context_decision_label(
    decision: ControllerCoordinationResultContextDecision,
) -> &'static str {
    match decision {
        ControllerCoordinationResultContextDecision::Admit => "admit",
        ControllerCoordinationResultContextDecision::Exclude => "exclude",
    }
}

pub(crate) fn local_memory_context_decision_label(
    decision: LocalMemoryContextDecision,
) -> &'static str {
    match decision {
        LocalMemoryContextDecision::Admit => "admit",
        LocalMemoryContextDecision::Exclude => "exclude",
    }
}

pub(crate) fn local_route_context_decision_label(
    decision: LocalRouteContextDecision,
) -> &'static str {
    match decision {
        LocalRouteContextDecision::Admit => "admit",
        LocalRouteContextDecision::Exclude => "exclude",
    }
}

pub(crate) fn worker_control_kind_label(kind: WorkerControlKind) -> &'static str {
    match kind {
        WorkerControlKind::StartOrResume => "START/RESUME",
        WorkerControlKind::Continue { .. } => "CONTINUE",
        WorkerControlKind::Stop => "STOP",
        WorkerControlKind::StatusRequest => "STATUS",
    }
}

pub(crate) fn route_user_decision_label(decision: RouteUserDecision) -> &'static str {
    match decision {
        RouteUserDecision::Allow => "allow",
        RouteUserDecision::Deny => "deny",
    }
}

pub(crate) fn route_gate_state_label(state: RouteGateState) -> &'static str {
    match state {
        RouteGateState::PendingApproval => "PENDING APPROVAL",
        RouteGateState::Allowed {
            by: DecisionAuthority::Policy,
        } => "ALLOWED · POLICY",
        RouteGateState::Allowed {
            by: DecisionAuthority::User,
        } => "ALLOWED · USER",
        RouteGateState::Denied {
            by: DecisionAuthority::Policy,
        } => "DENIED · POLICY",
        RouteGateState::Denied {
            by: DecisionAuthority::User,
        } => "DENIED · USER",
        RouteGateState::Dispatched {
            authorized_by: DecisionAuthority::Policy,
        } => "DISPATCHED · POLICY",
        RouteGateState::Dispatched {
            authorized_by: DecisionAuthority::User,
        } => "DISPATCHED · USER",
    }
}

pub(crate) fn session_lifecycle_phase_label(phase: SessionLifecyclePhase) -> &'static str {
    match phase {
        SessionLifecyclePhase::Healthy => "HEALTHY",
        SessionLifecyclePhase::Aging => "AGING",
        SessionLifecyclePhase::Saturated => "SATURATED",
        SessionLifecyclePhase::Retired => "RETIRED",
    }
}

pub(crate) fn local_worker_binding(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Option<LocalConversationWorkerBindingRecord>, String> {
    Ok(replay_local_conversation_worker_bindings(events)?
        .into_iter()
        .find(|binding| binding.conversation_id == conversation_id))
}

pub(crate) fn local_worker_session_alignment(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
    worker_id: WorkerId,
) -> Result<(SessionId, Option<SessionId>), String> {
    let topology = local_conversation_topology(events, conversation_id)?.ok_or_else(|| {
        format!("local conversation {conversation_id} has no orchestration topology")
    })?;
    let sessions = replay_session_audit(events)?;
    let active = active_worker_session(&sessions, worker_id).map(|record| record.session_id);
    Ok((topology.current_session_id, active))
}

pub(crate) fn worker_record(
    events: &[EventEnvelope],
    worker_id: WorkerId,
) -> Result<Option<WorkerAuditRecord>, String> {
    Ok(replay_worker_audit(events)?
        .into_iter()
        .find(|record| record.worker_id == worker_id))
}

pub(crate) fn next_available_worker_id(events: &[EventEnvelope]) -> Result<WorkerId, String> {
    let mut highest = 0_u64;

    for binding in replay_local_conversation_worker_bindings(events)? {
        highest = highest.max(binding.worker_id.get());
    }
    for record in replay_worker_audit(events)? {
        highest = highest.max(record.worker_id.get());
    }
    for record in replay_session_audit(events)? {
        if let Some(binding) = record.worker_binding {
            highest = highest.max(binding.worker_id().get());
        }
    }

    let next = highest
        .checked_add(1)
        .ok_or_else(|| "worker identity space exhausted".to_owned())?;
    Ok(WorkerId::new(next))
}

pub(crate) fn worker_phase_label(phase: WorkerPhase) -> &'static str {
    match phase {
        WorkerPhase::Unassigned => "UNASSIGNED",
        WorkerPhase::Ready => "READY",
        WorkerPhase::Working => "WORKING",
        WorkerPhase::NeedsInput => "NEEDS INPUT",
        WorkerPhase::Blocked => "BLOCKED",
        WorkerPhase::Completed => "COMPLETED",
        WorkerPhase::Failed => "FAILED",
        WorkerPhase::Stopped => "STOPPED",
    }
}

pub(crate) fn context_transcript(
    messages: &[DisplayMessage],
) -> Vec<context_composer::TranscriptMessage> {
    messages
        .iter()
        .filter_map(|message| {
            // MCP records are reader/audit evidence, not a synthetic message
            // from the user. The separately verified admission pipeline alone
            // supplies tool results to the model.
            let role = match message.role {
                DisplayRole::User => context_composer::TranscriptRole::User,
                DisplayRole::Assistant => context_composer::TranscriptRole::Assistant,
                DisplayRole::Mcp | DisplayRole::Activity => return None,
            };
            Some(context_composer::TranscriptMessage::durable(
                role,
                message.text.clone(),
                message.sequence,
            ))
        })
        .collect()
}

pub(crate) fn admitted_tool_result_context_messages(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    Ok(replay_admitted_tool_results(events, conversation_id)?
        .into_iter()
        .map(|outcome| {
            let record = outcome.record;
            context_composer::TranscriptMessage::tool_result(
                outcome.text.as_str(),
                record.call_id.get(),
                record.route_id.get(),
                record.provider_id.get(),
                record.source_session_id.get(),
                record.outcome_sequence,
                record.last_decision_sequence,
                record.outcome_kind.stable_name(),
            )
        })
        .collect())
}

pub(crate) fn admitted_routed_context_messages(
    events: &[EventEnvelope],
    conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let inbox = replay_local_routed_inbox_for_conversation(events, conversation_id)?;
    let admitted = replay_admitted_local_route_context(events, conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let item = inbox
            .iter()
            .find(|item| item.route_id == record.route_id)
            .ok_or_else(|| {
                format!(
                    "admitted routed context for route {} has no delivered inbox item",
                    record.route_id.get()
                )
            })?;
        if item.payload_id != record.payload_id
            || item.source_conversation_id != record.source_conversation_id
            || item.destination_conversation_id != record.destination_conversation_id
            || item.source_session_id != record.source_session_id
            || item.destination_session_id != record.destination_session_id
            || item.delivered_sequence != record.delivered_sequence
        {
            return Err(format!(
                "admitted routed context for route {} disagrees with delivered inbox provenance",
                record.route_id.get()
            ));
        }

        messages.push(context_composer::TranscriptMessage::routed(
            item.text.as_str(),
            record.route_id.get(),
            record.payload_id.get(),
            record.source_conversation_id.to_string(),
            record.delivered_sequence,
            record.last_decision_sequence,
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

pub(crate) fn snapshot_one_shot_local_memory(
    events: &[EventEnvelope],
    destination_conversation_id: LocalConversationId,
    selected_memory_ids: &BTreeSet<LocalMemoryId>,
) -> Result<(Vec<PendingOneShotMemory>, Option<u64>), String> {
    if selected_memory_ids.is_empty() {
        return Ok((Vec::new(), None));
    }

    let snapshot_after_sequence = events.last().map(|event| event.sequence).unwrap_or(0);
    let memory_ids = selected_memory_ids.iter().copied().collect::<Vec<_>>();
    let artifacts = validate_local_memory_one_shot_snapshot(
        events,
        destination_conversation_id,
        &memory_ids,
        snapshot_after_sequence,
    )?;

    let pending = artifacts
        .into_iter()
        .map(|artifact| PendingOneShotMemory {
            memory_id: artifact.memory_id,
            source_conversation_id: artifact.source_conversation_id,
            text: artifact.text,
            artifact_sequence: artifact.recorded_sequence,
        })
        .collect::<Vec<_>>();

    Ok((pending, Some(snapshot_after_sequence)))
}

pub(crate) fn one_shot_local_memory_messages_for_turn(
    events: &[EventEnvelope],
    turn_id: LocalTurnId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let Some(selection) = replay_local_memory_turn_selection_audit(events)?
        .into_iter()
        .find(|record| record.turn_id == turn_id)
    else {
        return Ok(Vec::new());
    };

    let artifacts = replay_local_memory_audit(events)?
        .into_iter()
        .map(|artifact| (artifact.memory_id, artifact))
        .collect::<BTreeMap<_, _>>();
    let mut messages = Vec::with_capacity(selection.memory_ids.len());

    for memory_id in &selection.memory_ids {
        let artifact = artifacts.get(memory_id).ok_or_else(|| {
            format!(
                "one-shot local memory turn {} references missing immutable memory {}",
                turn_id,
                memory_id.get()
            )
        })?;
        messages.push(context_composer::TranscriptMessage::local_memory_one_shot(
            artifact.text.as_str(),
            artifact.memory_id.get(),
            artifact.source_conversation_id.to_string(),
            artifact.recorded_sequence,
            selection.selection_snapshot_after_sequence,
            Some(selection.recorded_sequence),
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

pub(crate) fn admitted_local_memory_messages(
    events: &[EventEnvelope],
    destination_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let artifacts = replay_local_memory_audit(events)?;
    let admitted = replay_admitted_local_memory_context(events, destination_conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.memory_id == record.memory_id)
            .ok_or_else(|| {
                format!(
                    "admitted local memory {} has no immutable artifact",
                    record.memory_id.get()
                )
            })?;
        if artifact.source_conversation_id != record.source_conversation_id
            || artifact.recorded_sequence != record.artifact_sequence
        {
            return Err(format!(
                "admitted local memory {} disagrees with artifact provenance",
                record.memory_id.get()
            ));
        }

        messages.push(context_composer::TranscriptMessage::local_memory(
            artifact.text.as_str(),
            record.memory_id.get(),
            record.source_conversation_id.to_string(),
            record.artifact_sequence,
            record.last_decision_sequence,
        ));
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

pub(crate) fn admitted_controller_worker_result_messages(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let results =
        replay_controller_worker_results_for_conversation(events, controller_conversation_id)?;
    let admitted =
        replay_admitted_controller_worker_result_context(events, controller_conversation_id)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let result = results
            .iter()
            .find(|item| item.route_id == record.route_id)
            .ok_or_else(|| {
                format!(
                    "admitted controller result route {} has no terminal result",
                    record.route_id.get()
                )
            })?;
        if result.control_id != record.control_id
            || result.controller_conversation_id != Some(controller_conversation_id)
            || result.result_sequence != record.result_sequence
        {
            return Err(format!(
                "admitted controller result route {} disagrees with terminal result provenance",
                record.route_id.get()
            ));
        }

        let (result_kind, result_text) = match &result.detail {
            ControllerWorkerResultDetail::Status {
                phase,
                acknowledged_sequence,
            } => (
                "status",
                format!(
                    "phase: {}\nacknowledged_event: #{}",
                    worker_phase_label(*phase),
                    acknowledged_sequence,
                ),
            ),
            ControllerWorkerResultDetail::Action {
                kind,
                from_phase,
                resulting_phase,
                started_sequence,
                lifecycle_sequence,
            } => (
                "action",
                format!(
                    "control_kind: {}\nfrom_phase: {}\nresulting_phase: {}\naction_started_event: #{}\nlifecycle_event: #{}",
                    worker_control_kind_label(*kind),
                    worker_phase_label(*from_phase),
                    worker_phase_label(*resulting_phase),
                    started_sequence,
                    lifecycle_sequence,
                ),
            ),
            ControllerWorkerResultDetail::Continuation {
                outcome,
                execution_turn_id,
                lease_id,
                permit_ordinal,
                started_sequence,
                terminal_sequence,
                output_text,
            } => {
                let output = output_text
                    .as_deref()
                    .unwrap_or("(no durable assistant output)");
                (
                    "continuation",
                    format!(
                        "outcome: {}\ncontinuation_lease: {}\npermit_ordinal: {}\nexecution_turn_id: {}\nexecution_started_event: #{}\nterminal_transport_event: #{}\nworker_output:\n{}",
                        outcome.stable_name(),
                        lease_id.get(),
                        permit_ordinal,
                        execution_turn_id,
                        started_sequence,
                        terminal_sequence,
                        output,
                    ),
                )
            }
        };

        messages.push(
            context_composer::TranscriptMessage::controller_worker_result(
                result.control_id.get(),
                result.route_id.get(),
                result.worker_id.get(),
                result.goal_id.get(),
                result.result_sequence,
                record.last_decision_sequence,
                result_kind,
                result_text.as_str(),
            ),
        );
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}

pub(crate) fn admitted_controller_coordination_result_messages(
    events: &[EventEnvelope],
    controller_conversation_id: LocalConversationId,
) -> Result<Vec<context_composer::TranscriptMessage>, String> {
    let admitted =
        replay_admitted_controller_coordination_result_context(events, controller_conversation_id)?;
    let coordinations = replay_controller_coordination_audit(events)?;
    let mut messages = Vec::with_capacity(admitted.len());

    for record in admitted {
        let coordination = coordinations
            .iter()
            .find(|item| item.coordination_turn_id == record.coordination_turn_id)
            .ok_or_else(|| {
                format!(
                    "admitted controller coordination turn {} has no durable execution",
                    record.coordination_turn_id
                )
            })?;
        if coordination.controller_conversation_id != controller_conversation_id
            || coordination.result_sequence != Some(record.result_sequence)
        {
            return Err(format!(
                "admitted controller coordination turn {} disagrees with terminal result provenance",
                record.coordination_turn_id
            ));
        }
        let outcome = coordination.outcome.ok_or_else(|| {
            format!(
                "admitted controller coordination turn {} has no terminal outcome",
                record.coordination_turn_id
            )
        })?;
        let output = controller_coordination_output_text(events, record.coordination_turn_id)?
            .unwrap_or_else(|| "(no durable coordination output)".to_owned());

        messages.push(
            context_composer::TranscriptMessage::controller_coordination_result(
                record.coordination_turn_id.to_string(),
                record.result_sequence,
                record.last_decision_sequence,
                outcome.stable_name(),
                output.as_str(),
            ),
        );
    }

    messages.sort_by_key(context_composer::TranscriptMessage::order_sequence);
    Ok(messages)
}
