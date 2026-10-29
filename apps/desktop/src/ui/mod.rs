//! Existing egui application frame; extracted without changing behavior.
//! Split into smaller UI panels in roadmap item R13.
use super::*;

mod composer;
mod coordination_window;
mod frame_lifecycle;
mod header;
mod inline_mcp_background;
mod navigation;
mod panels;
mod reader;
mod sidebar;

impl eframe::App for ChatariumApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgb(23, 24, 29).to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_frame_updates(ctx);
        // Visibility preferences are local-only and never mutate the durable
        // audit. Cache them in egui state to avoid disk reads each frame.
        let display_preferences_path = self.journal_path.with_file_name("display-preferences.json");
        let display_preferences_id = egui::Id::new("chatarium-display-preferences");
        let mut display_preferences = ctx.data_mut(|data| {
            data.get_temp::<display_preferences::DisplayPreferences>(display_preferences_id)
                .unwrap_or_else(|| {
                    display_preferences::DisplayPreferences::load(&display_preferences_path)
                })
        });
        ctx.data_mut(|data| data.insert_temp(display_preferences_id, display_preferences.clone()));

        self.consume_frame_navigation(ctx);

        // Search and the active chat reader share the same cached native
        // message corpus. Avoid an extra full-journal scan and a clone of
        // every visible transcript message on each egui redraw.
        let native_search_index = native_conversation_search::cached_index(ctx, &self.events);
        let local_display_messages = native_search_index.messages(self.local_conversation_id);
        let local_conversation_title = self
            .local_conversation_catalog
            .entry(self.local_conversation_id)
            .and_then(|entry| entry.title.clone())
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| derived_conversation_title(local_display_messages));
        let sidebar = self.render_sidebar(
            ctx,
            &native_search_index,
            local_display_messages,
            &local_conversation_title,
            &mut display_preferences,
            &display_preferences_path,
            display_preferences_id,
        );
        self.render_frame_panels(
            ctx,
            sidebar,
            local_display_messages,
            &local_conversation_title,
            &native_search_index,
            &display_preferences,
        );
        if self.saved_revision < self.draft_revision
            || self.commit_in_flight.is_some()
            || self.sign_in_pending
            || self.pending_remote_turn.is_some()
            || self.active_remote_turn.is_some()
            || self.historical_load_pending.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
