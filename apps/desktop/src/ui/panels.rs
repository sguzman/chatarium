//! Sidebar action resolution and ordered native panel rendering.
use super::*;

impl ChatariumApp {
    pub(super) fn render_frame_panels(&mut self,
        ctx: &egui::Context,
        sidebar: sidebar::SidebarOutcome,
        local_display_messages: &[DisplayMessage],
        local_conversation_title: &String,
        native_search_index: &Arc<native_conversation_search::NativeConversationSearchIndex>,
        display_preferences: &display_preferences::DisplayPreferences,) {
        let historical_mode = sidebar.historical_mode;
        let remote_catalog_selected = sidebar.remote_catalog_selected;
        let selected_live_mirror = sidebar.selected_live_mirror;
        let selected_historical_id = sidebar.selected_historical_id;
        let header_conversation_title = sidebar.header_conversation_title;
        let mut mirror_remote_requested = sidebar.mirror_remote_requested;
        let mut sync_live_requested = sidebar.sync_live_requested;
        self.render_coordination_window(ctx);
        let rename_local_requested = sidebar.rename_local_requested;
        let save_project_directory_requested = sidebar.save_project_directory_requested;
        let project_memory_permission_request = sidebar.project_memory_permission_request;
        let archive_local_requested = sidebar.archive_local_requested;
        let open_reader_from_transcript_search = sidebar.open_reader_from_transcript_search;
        let refresh_history_requested = sidebar.refresh_history_requested;
        let mirror_start_requested = sidebar.mirror_start_requested;
        let mirror_recheck_requested = sidebar.mirror_recheck_requested;
        let mirror_pause_requested = sidebar.mirror_pause_requested;
        let mirror_resume_requested = sidebar.mirror_resume_requested;

        if rename_local_requested {
            self.rename_current_local_conversation();
        }
        if save_project_directory_requested {
            self.save_current_project_directory();
        }
        if let Some((source, allow)) = project_memory_permission_request {
            self.change_project_memory_permission(source, allow);
        }
        if archive_local_requested {
            self.archive_current_local_conversation();
        }
        if open_reader_from_transcript_search {
            self.reader_search_hit = Some(0);
        }
        if let Some(remote_conversation_id) = mirror_remote_requested.as_ref() {
            self.open_discovered_remote_conversation(remote_conversation_id.clone(), ctx);
        }
        if refresh_history_requested {
            self.start_history_discovery(ctx);
        }
        if mirror_start_requested {
            self.start_mirror_controller();
        } else if mirror_recheck_requested {
            self.recheck_remote_health();
        } else if mirror_pause_requested {
            self.send_mirror_controller_command(MirrorControllerCommand::Pause);
        } else if mirror_resume_requested {
            self.send_mirror_controller_command(MirrorControllerCommand::Resume);
        }

        self.render_conversation_header(
            ctx,
            historical_mode,
            &header_conversation_title,
            remote_catalog_selected,
            selected_live_mirror,
            &local_conversation_title,
            local_display_messages,
        );

        self.render_composer_panel(
            ctx,
            historical_mode,
            remote_catalog_selected,
            selected_live_mirror,
            selected_historical_id,
            &display_preferences,
            &mut mirror_remote_requested,
            &mut sync_live_requested,
        );

        if let Some(local_conversation_id) = sync_live_requested {
            self.sync_historical_conversation(local_conversation_id, ctx);
        }

        self.render_conversation_reader(
            ctx,
            historical_mode,
            selected_live_mirror,
            remote_catalog_selected,
            &display_preferences,
            local_display_messages,
            &native_search_index,
        );
    }
}
