//! Desktop memory actions extracted intact from ChatariumApp.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn record_local_memory(&mut self) {
        if self.local_memory_command_pending || self.local_memory_draft.trim().is_empty() {
            return;
        }
        let memory_id = match next_available_local_memory_id(&self.events) {
            Ok(memory_id) => memory_id,
            Err(error) => {
                self.status = format!("cannot allocate local memory identity: {error}");
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot record local memory: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RecordLocalMemory {
            memory_id,
            source_conversation_id: native_conversation_search::recording_source_conversation(
                self.local_conversation_id,
                self.staged_local_memory_origin,
            ),
            text: self.local_memory_draft.clone(),
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("recording immutable local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory: {error}");
            }
        }
    }

    pub(crate) fn select_local_memory_for_next_request(&mut self, memory_id: LocalMemoryId) {
        if self.commit_in_flight.is_some() {
            return;
        }
        let selected = self
            .local_memory_one_shot_selections
            .entry(self.local_conversation_id)
            .or_default();
        if selected.contains(&memory_id) {
            return;
        }
        if selected.len() >= MAX_ONE_SHOT_MEMORIES_PER_TURN {
            self.status = format!(
                "cannot select more than {} one-shot memories for one request",
                MAX_ONE_SHOT_MEMORIES_PER_TURN
            );
            return;
        }
        selected.insert(memory_id);
        self.status = format!(
            "local memory {} selected for the next authored request only",
            memory_id.get()
        );
    }

    pub(crate) fn deselect_local_memory_for_next_request(&mut self, memory_id: LocalMemoryId) {
        if self.commit_in_flight.is_some() {
            return;
        }
        if let Some(selected) = self
            .local_memory_one_shot_selections
            .get_mut(&self.local_conversation_id)
        {
            selected.remove(&memory_id);
        }
        self.local_memory_one_shot_selections
            .retain(|_, selected| !selected.is_empty());
        self.status = format!(
            "local memory {} removed from next-request-only selection",
            memory_id.get()
        );
    }

    pub(crate) fn decide_local_memory_context(
        &mut self,
        memory_id: LocalMemoryId,
        decision: LocalMemoryContextDecision,
    ) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot update local memory context eligibility: persistence unavailable"
                .to_owned();
            return;
        };
        match sender.send(PersistCommand::DecideLocalMemoryContext {
            memory_id,
            destination_conversation_id: self.local_conversation_id,
            decision,
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!(
                    "recording local memory {} context {} decision…",
                    memory_id.get(),
                    local_memory_context_decision_label(decision),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local memory context decision: {error}");
            }
        }
    }

    pub(crate) fn supersede_local_memory(
        &mut self,
        predecessor_memory_id: LocalMemoryId,
        successor_memory_id: LocalMemoryId,
    ) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot supersede local memory: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::SupersedeLocalMemory {
            predecessor_memory_id,
            successor_memory_id,
        }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!(
                    "recording local memory {} → {} supersession…",
                    predecessor_memory_id.get(),
                    successor_memory_id.get(),
                );
            }
            Err(error) => {
                self.status = format!("failed to queue local memory supersession: {error}");
            }
        }
    }

    pub(crate) fn add_local_memory_label(&mut self, memory_id: LocalMemoryId) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(draft) = self.local_memory_label_drafts.get(&memory_id) else {
            return;
        };
        let label = match LocalMemoryLabel::new(draft.as_str()) {
            Ok(label) => label,
            Err(error) => {
                self.status = format!(
                    "cannot add label to local memory {}: {error:?}",
                    memory_id.get()
                );
                return;
            }
        };
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot add local memory label: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::AddLocalMemoryLabel { memory_id, label }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("adding label to local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory label addition: {error}");
            }
        }
    }

    pub(crate) fn remove_local_memory_label(
        &mut self,
        memory_id: LocalMemoryId,
        label: LocalMemoryLabel,
    ) {
        if self.local_memory_command_pending {
            return;
        }
        let Some(sender) = &self.persist_tx else {
            self.status = "cannot remove local memory label: persistence unavailable".to_owned();
            return;
        };
        match sender.send(PersistCommand::RemoveLocalMemoryLabel { memory_id, label }) {
            Ok(()) => {
                self.local_memory_command_pending = true;
                self.status = format!("removing label from local memory {}…", memory_id.get());
            }
            Err(error) => {
                self.status = format!("failed to queue local memory label removal: {error}");
            }
        }
    }
}
