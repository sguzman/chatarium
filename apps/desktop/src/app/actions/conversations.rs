//! conversations user actions extracted without changing their behavior.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn select_local_conversation(&mut self) {
        self.selected_historical_conversation = None;
        self.selected_remote_catalog_id = None;
        self.historical_load_pending = None;
        self.live_mirror_truncated_before = false;
        self.reader_restore_pending = true;
        self.reader_search_hit = None;
        self.status = "local conversation selected".to_owned();
    }

    pub(crate) fn local_conversation_busy(&self) -> bool {
        self.saved_revision < self.draft_revision
            || self.commit_in_flight.is_some()
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || self.agent_mcp_flow.is_some()
            || self.coordination_pending_wake.is_some()
    }

    pub(crate) fn apply_local_conversation(&mut self, conversation_id: LocalConversationId) {
        self.local_conversation_id = conversation_id;
        self.staged_attachment = None;
        self.attachment_path_draft.clear();
        self.draft = projected_working_draft(&self.events, conversation_id);
        self.draft_revision = 0;
        self.saved_revision = 0;
        self.evidence = TurnEvidence::default();
        self.coordination_tool_budget.reset();

        let settings = self
            .inference_settings
            .for_conversation_with_mcp_defaults(conversation_id, &self.mcp_agent_defaults);
        self.slow_warning_seconds = settings.slow_warning_seconds;
        self.selected_model = settings.model;
        self.conversation_instructions = settings.instructions;
        self.conversation_developer_context = settings.developer_context;
        self.conversation_mcp_enabled = settings.mcp_enabled;
        self.conversation_behavior_profile =
            self.behavior_profiles.for_conversation(conversation_id);
        if !self.remote_models.is_empty()
            && !self.selected_model.as_ref().is_some_and(|selected| {
                self.remote_models
                    .iter()
                    .any(|model| &model.slug == selected)
            })
        {
            self.selected_model = self.remote_models.first().map(|model| model.slug.clone());
            self.persist_current_inference_settings();
        }

        self.local_conversation_rename = self
            .local_conversation_catalog
            .entry(conversation_id)
            .and_then(|entry| entry.title.clone())
            .unwrap_or_default();
        self.local_conversation_project_directory = self
            .local_conversation_catalog
            .entry(conversation_id)
            .and_then(|entry| entry.project_directory.clone())
            .unwrap_or_default();
        self.select_local_conversation();
    }

    pub(crate) fn activate_local_conversation(&mut self, conversation_id: LocalConversationId) {
        if self.local_conversation_busy() {
            self.status =
                "finish or stop the active local turn before switching conversations".to_owned();
            return;
        }
        let was_archived = self
            .local_conversation_catalog
            .entry(conversation_id)
            .map(|entry| entry.archived);
        let Some(was_archived) = was_archived else {
            self.status = "local conversation is absent from workspace catalog".to_owned();
            return;
        };
        if was_archived {
            if let Err(error) =
                self.local_conversation_catalog
                    .set_archived(conversation_id, false, unix_now_ms())
            {
                self.status = format!("failed to restore local conversation: {error}");
                return;
            }
        }
        if let Err(error) = self
            .local_conversation_catalog
            .set_active(conversation_id, unix_now_ms())
        {
            self.status = format!("failed to select local conversation: {error}");
            return;
        }
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(conversation_id);
    }

    pub(crate) fn create_local_conversation(&mut self) {
        if self.local_conversation_busy() {
            self.status =
                "finish or stop the active local turn before creating a conversation".to_owned();
            return;
        }
        let conversation_id = LocalConversationId::new();
        self.local_conversation_catalog
            .create(conversation_id, unix_now_ms());
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(conversation_id);
        if self.mcp_agent_defaults.seed_new_conversations {
            self.conversation_developer_context = self.mcp_agent_defaults.bootstrap.clone();
            self.conversation_mcp_enabled = true;
            self.persist_current_inference_settings();
        }
        self.status = "new isolated local conversation created".to_owned();
    }

    pub(crate) fn rename_current_local_conversation(&mut self) {
        let title = self.local_conversation_rename.clone();
        if let Err(error) = self.local_conversation_catalog.rename(
            self.local_conversation_id,
            Some(title),
            unix_now_ms(),
        ) {
            self.status = format!("failed to rename local conversation: {error}");
        } else if self.persist_local_conversation_catalog() {
            self.local_conversation_rename = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.title.clone())
                .unwrap_or_default();
            self.status = "local conversation title saved".to_owned();
        }
    }

    pub(crate) fn save_current_project_directory(&mut self) {
        if self.local_conversation_busy() {
            self.status =
                "finish the active turn before changing this chat's project folder".to_owned();
            return;
        }
        if let Err(error) = self.local_conversation_catalog.set_project_directory(
            self.local_conversation_id,
            Some(self.local_conversation_project_directory.clone()),
            unix_now_ms(),
        ) {
            self.status = format!("project folder not saved: {error}");
            return;
        }
        if self.persist_local_conversation_catalog() {
            self.invalidate_project_memory_cache();
            self.local_conversation_project_directory = self
                .local_conversation_catalog
                .entry(self.local_conversation_id)
                .and_then(|entry| entry.project_directory.clone())
                .unwrap_or_default();
            self.status = if self.local_conversation_project_directory.is_empty() {
                "project folder cleared for this chat".to_owned()
            } else {
                format!(
                    "project folder saved for this chat: {}",
                    self.local_conversation_project_directory
                )
            };
        }
    }

    /// In-flight project reads cannot remain eligible after revocation.
    pub(crate) fn invalidate_project_memory_cache(&mut self) {
        self.openviking_project_cache.invalidate();
    }

    pub(crate) fn change_project_memory_permission(
        &mut self,
        source: LocalConversationId,
        allow: bool,
    ) {
        if self.local_conversation_busy() {
            self.status =
                "finish the active turn before changing project memory sharing".to_owned();
            return;
        }
        let path = self
            .journal_path
            .with_file_name(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME);
        if let Err(error) = openviking_project_ui::change_permission_durable(
            &path,
            &self.local_conversation_catalog,
            &mut self.project_memory_grants,
            source,
            self.local_conversation_id,
            allow,
        ) {
            self.status = format!("project memory permission not saved: {error}");
            return;
        }
        self.invalidate_project_memory_cache();
        self.status = if allow {
            "Project checkpoint sharing granted for this source chat only".to_owned()
        } else {
            "Project checkpoint sharing revoked for this source chat".to_owned()
        };
    }

    pub(crate) fn archive_current_local_conversation(&mut self) {
        if self.local_conversation_busy() {
            self.status = "finish or stop the active local turn before archiving this conversation"
                .to_owned();
            return;
        }
        let archived = self.local_conversation_id;
        if let Err(error) =
            self.local_conversation_catalog
                .set_archived(archived, true, unix_now_ms())
        {
            self.status = format!("failed to archive local conversation: {error}");
            return;
        }
        let next = self
            .local_conversation_catalog
            .first_unarchived()
            .unwrap_or_else(|| {
                let id = LocalConversationId::new();
                self.local_conversation_catalog.create(id, unix_now_ms());
                id
            });
        if let Err(error) = self
            .local_conversation_catalog
            .set_active(next, unix_now_ms())
        {
            self.status = format!("failed to archive local conversation: {error}");
            return;
        }
        if !self.persist_local_conversation_catalog() {
            return;
        }
        self.apply_local_conversation(next);
        self.status = "local conversation archived".to_owned();
    }

    pub(crate) fn select_historical_conversation(
        &mut self,
        local_conversation_id: LocalConversationId,
    ) {
        self.selected_remote_catalog_id = None;
        if self.selected_historical_conversation == Some(local_conversation_id)
            && self.loaded_historical_conversation == Some(local_conversation_id)
        {
            return;
        }

        self.selected_historical_conversation = Some(local_conversation_id);
        self.loaded_historical_conversation = None;
        self.historical_messages.clear();
        self.historical_load_pending = Some(local_conversation_id);
        self.reader_restore_pending = true;
        self.reader_search_hit = None;

        let Some(sender) = &self.persist_tx else {
            self.historical_load_pending = None;
            self.status = "cannot load historical conversation: persistence unavailable".to_owned();
            return;
        };
        let command = if self
            .live_mirrored_conversations
            .contains(&local_conversation_id)
        {
            PersistCommand::LoadLiveConversation {
                local_conversation_id,
            }
        } else {
            PersistCommand::LoadHistoricalConversation {
                local_conversation_id,
            }
        };
        if let Err(error) = sender.send(command) {
            self.historical_load_pending = None;
            self.status = format!("failed to queue conversation snapshot load: {error}");
        } else if self
            .live_mirrored_conversations
            .contains(&local_conversation_id)
        {
            self.status = "loading validated live mirror snapshot…".to_owned();
        } else {
            self.status = "loading verified historical snapshot…".to_owned();
        }
    }
}
