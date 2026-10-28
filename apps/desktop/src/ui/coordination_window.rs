//! The optional coordination workspace window and navigation actions.
//! The existing durable-save behavior is preserved; deeper threading changes
//! are outside this mechanical move.
use super::*;

impl ChatariumApp {
    pub(super) fn render_coordination_window(&mut self, ctx: &egui::Context) {
        if self.coordination_window_open {
            let mut open = true;
            let work_area = ctx.available_rect().size();
            let max_window_size = egui::vec2(
                (work_area.x - 24.0).max(260.0),
                (work_area.y - 32.0).max(240.0),
            );
            let preferred_window_size =
                egui::vec2(max_window_size.x.min(690.0), max_window_size.y.min(540.0));
            egui::Window::new("Chatarium coordination / message center")
                .open(&mut open)
                .default_size(preferred_window_size)
                .max_size(max_window_size)
                .resizable(true)
                .constrain(true)
                .show(ctx, |ui| {
                    let conversations = self
                        .local_conversation_catalog
                        .entries()
                        .iter()
                        .map(|entry| {
                            let rgb = entry.accent_rgb();
                            (
                                entry.id.to_string(),
                                entry
                                    .title
                                    .clone()
                                    .filter(|s| !s.trim().is_empty())
                                    .or_else(|| entry.project_directory.clone())
                                    .unwrap_or_else(|| {
                                        format!("Chat {}", &entry.id.to_string()[..8])
                                    }),
                                egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                            )
                        })
                        .collect::<Vec<_>>();
                    let previous = self.coordination_workspace.clone();
                    let changed = egui::ScrollArea::vertical()
                        .id_salt("chatarium-coordination-scroll")
                        .max_height((max_window_size.y - 65.0).max(140.0))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            coordination_workspace::render(
                                ui,
                                &mut self.coordination_workspace,
                                &mut self.coordination_ui,
                                &conversations,
                                &self.local_conversation_id.to_string(),
                            )
                        })
                        .inner;
                    if changed {
                        if let Err(error) = self
                            .coordination_workspace
                            .save_atomic(&self.coordination_store_path())
                        {
                            self.coordination_workspace = previous;
                            self.coordination_ui.notice =
                                format!("coordination changes not saved: {error}");
                        }
                    }
                });
            self.coordination_window_open = open;
        }
        if let Some(target) = self.coordination_ui.navigate_to_conversation.take() {
            match target.parse::<LocalConversationId>() {
                Ok(target_id) => {
                    self.activate_local_conversation(target_id);
                    if self.local_conversation_id == target_id {
                        self.coordination_window_open = false;
                        ctx.request_repaint();
                    }
                }
                Err(_) => {
                    self.status =
                        "Coordination message refers to an invalid local chat ID".to_owned()
                }
            }
        }
    }
}
