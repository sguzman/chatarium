//! composer user actions extracted without changing their behavior.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn queue_draft_snapshot(&mut self) {
        self.draft_revision = self.draft_revision.saturating_add(1);
        let revision = self.draft_revision;
        let Some(sender) = &self.persist_tx else {
            self.status = "persistence unavailable; draft is not durable".to_owned();
            return;
        };

        if let Err(error) = sender.send(PersistCommand::SaveDraft {
            conversation_id: self.local_conversation_id,
            revision,
            text: self.draft.clone(),
        }) {
            self.status = format!("persistence worker unavailable: {error}");
        }
    }

    pub(crate) fn stage_attachment(&mut self, path: &std::path::Path) {
        if self.commit_in_flight.is_some() {
            self.status = "cannot change attachment during durable commit".to_owned();
            return;
        }
        match attachment_intake::prepare(path) {
            Ok(attachment) => {
                self.status = format!(
                    "staged {} ({} bytes); content is not durable until Send",
                    attachment.filename, attachment.byte_len
                );
                self.staged_attachment = Some(attachment);
            }
            Err(error) => {
                self.status = format!("attachment not staged: {error}");
            }
        }
    }

    pub(crate) fn commit_current_message(&mut self) {
        if self.draft.trim().is_empty() || self.commit_in_flight.is_some() {
            return;
        }
        if self.staged_attachment.is_some() && !self.remote_connected() {
            self.status = "attachment send requires an active ChatGPT connection".to_owned();
            return;
        }
        let send_started = Instant::now();
        // Final Send admission cannot use a prior topic's verified checkpoint.
        // A newly typed topic may still be fetching; in that case proceed
        // without old-topic memory instead of smuggling stale evidence.
        let send_query = self.draft.clone();
        self.observe_openviking_query(self.local_conversation_id, &send_query, true);
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot commit: persistence unavailable".to_owned();
            return;
        };

        let request_id = self.next_commit_request;
        self.next_commit_request = self.next_commit_request.saturating_add(1);
        let message = AuthoredUserMessage::new(
            self.local_conversation_id,
            LocalTurnId::new(),
            LocalMessageId::new(),
            self.draft.clone(),
        );
        let mut one_shot_memory_ids = Vec::new();
        let mut one_shot_memory_snapshot_after_sequence = None;
        if self.remote_connected() {
            if let Some(model) = self.selected_model.clone() {
                let request_patch = match self.current_behavior_request_patch() {
                    Ok(patch) => patch,
                    Err(error) => {
                        self.status = format!("cannot send with current behavior profile: {error}");
                        return;
                    }
                };
                let presence = send_admission_gate::Presence::from_kinds(
                    self.events.iter().map(|event| event.kind),
                );
                let routed_context = match send_admission_gate::replay_when(presence.routed, || {
                    admitted_routed_context_messages(&self.events, self.local_conversation_id)
                }) {
                    Ok(messages) => messages,
                    Err(error) => {
                        self.status = format!("cannot snapshot admitted routed context: {error}");
                        return;
                    }
                };
                let tool_result_context =
                    match send_admission_gate::replay_when(presence.tools, || {
                        admitted_tool_result_context_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!("cannot snapshot admitted tool results: {error}");
                            return;
                        }
                    };
                let mut local_memory_context =
                    match send_admission_gate::replay_when(presence.local_memory, || {
                        admitted_local_memory_messages(&self.events, self.local_conversation_id)
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!("cannot snapshot admitted local memory: {error}");
                            return;
                        }
                    };
                // Freeze the already verified, strictly same-conversation
                // OpenViking evidence at Send click alongside local admissions.
                // A later background result cannot mutate this request.
                local_memory_context.extend(
                    self.openviking_project_cache.admitted_relevant_context(
                        self.openviking_retrieval_cache
                            .get(&self.local_conversation_id)
                            .map(Vec::as_slice),
                        self.local_conversation_id,
                        &self.local_conversation_catalog,
                        &self.project_memory_grants,
                        &self.draft,
                    ),
                );
                let selected_one_shot = self
                    .local_memory_one_shot_selections
                    .get(&self.local_conversation_id)
                    .cloned()
                    .unwrap_or_default();
                let (local_memory_one_shot, snapshot_after_sequence) =
                    match snapshot_one_shot_local_memory(
                        &self.events,
                        self.local_conversation_id,
                        &selected_one_shot,
                    ) {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.status = format!("cannot snapshot one-shot local memory: {error}");
                            return;
                        }
                    };
                one_shot_memory_ids = local_memory_one_shot
                    .iter()
                    .map(|memory| memory.memory_id)
                    .collect();
                one_shot_memory_snapshot_after_sequence = snapshot_after_sequence;
                let controller_result_context =
                    match send_admission_gate::replay_when(presence.controller_results, || {
                        admitted_controller_worker_result_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status =
                                format!("cannot snapshot admitted controller results: {error}");
                            return;
                        }
                    };
                let controller_coordination_result_context =
                    match send_admission_gate::replay_when(presence.coordination_results, || {
                        admitted_controller_coordination_result_messages(
                            &self.events,
                            self.local_conversation_id,
                        )
                    }) {
                        Ok(messages) => messages,
                        Err(error) => {
                            self.status = format!(
                                "cannot snapshot admitted controller coordination results: {error}"
                            );
                            return;
                        }
                    };
                self.commit_remote_intents.insert(
                    request_id,
                    PendingInferenceIntent {
                        model,
                        instructions: (!self.conversation_instructions.trim().is_empty())
                            .then(|| self.conversation_instructions.clone()),
                        developer_context: {
                            let mut context = self.conversation_developer_context.clone();
                            if let Some(directory) = self
                                .local_conversation_catalog
                                .entry(self.local_conversation_id)
                                .and_then(|entry| entry.project_directory.as_deref())
                            {
                                context.push_str(&format!(
                                    "\n\nChatarium project directory for this conversation: {directory}\n                                     For local Git, file, and shell tasks, invoke chatarium.builtin::shell.command directly,                                      e.g. {{\"command\":\"git status --short --branch\"}}.                                      The shell's default cwd is this project folder; an explicit cwd overrides it.                                      tools.list is optional discovery, not a prerequisite to shell execution.                                      Do not claim results before the tool reply is admitted.                                      This directory is a workspace hint, not a request to execute anything."
                                ));
                            }
                            context.push_str(&self.coordination_workspace.identity_context(
                                &self.local_conversation_id.to_string(),
                            ));
                            context
                        },
                        routed_context,
                        tool_result_context,
                        local_memory_context,
                        controller_result_context,
                        controller_coordination_result_context,
                        request_patch,
                        attachment: self.staged_attachment.clone(),
                        coordination_ids: self.coordination_workspace.context_delivered_ids(
                            &self.local_conversation_id.to_string(),
                        ),
                    },
                );
            }
        }
        let send_elapsed = send_started.elapsed();
        if send_elapsed >= Duration::from_millis(100) {
            diagnostics::warn(
                "send",
                format!(
                    "Send admission took {}ms across {} journal events",
                    send_elapsed.as_millis(),
                    self.events.len()
                ),
            );
        }
        match sender.send(PersistCommand::CommitMessage {
            request_id,
            message,
            attachment: self
                .staged_attachment
                .as_ref()
                .map(attachment_intake::PreparedAttachment::durable_record),
            one_shot_memory_ids,
            one_shot_memory_snapshot_after_sequence,
        }) {
            Ok(()) => {
                self.commit_in_flight = Some(request_id);
                self.status = "committing exact user message to local journal…".to_owned();
            }
            Err(error) => {
                self.commit_remote_intents.remove(&request_id);
                self.status = format!("failed to queue commit: {error}");
            }
        }
    }

    pub(crate) fn queue_turn_event(&mut self, turn_id: LocalTurnId, kind: EventKind, payload: String) -> bool {
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot persist remote turn evidence: persistence unavailable".to_owned();
            return false;
        };
        if let Err(error) = sender.send(PersistCommand::AppendTurnEvent {
            turn_id,
            kind,
            payload,
        }) {
            self.status = format!("failed to queue remote turn evidence: {error}");
            return false;
        }
        true
    }

    /// A diagnostic of the latest non-authored wake. Completion means model
    /// output finished, not that a worker sent a separate coordination reply.
}
