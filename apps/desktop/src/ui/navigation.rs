//! Native chat keyboard navigation, evaluated before transcript projection.
use super::*;

impl ChatariumApp {
    pub(super) fn consume_frame_navigation(&mut self, ctx: &egui::Context) {
        // Consume global native chat navigation before resolving conversation
        // title, visible transcript, search projection or reader state. Doing
        // it after projection rendered the old chat despite a successful
        // catalog activation, until an unrelated event such as a resize.
        let (new_chat_shortcut, cycle_shortcut) =
            ctx.input_mut(conversation_keybindings::consume_native_conversation_shortcuts);
        if new_chat_shortcut {
            let before = self.local_conversation_id;
            self.create_local_conversation();
            if self.local_conversation_id != before {
                self.local_conversation_search_query.clear();
                self.local_conversation_search_selection = None;
                ctx.memory_mut(|memory| {
                    memory.request_focus(egui::Id::new("chatarium-message-composer"))
                });
                ctx.request_repaint();
            }
        } else if let Some(direction) = cycle_shortcut {
            let native_selected = self.selected_historical_conversation.is_none()
                && self.selected_remote_catalog_id.is_none();
            let current = native_selected.then_some(self.local_conversation_id);
            if let Some(target) = conversation_navigation::cycle_target(
                &self.local_conversation_catalog.entries(),
                current,
                direction,
            ) {
                let can_switch = !self.local_conversation_busy();
                let trace_cycle =
                    std::env::var("CHATARIUM_QA_SHORTCUT_TRACE").as_deref() == Ok("1");
                if trace_cycle {
                    eprintln!(
                        "CHATARIUM_QA_CYCLE_BEFORE busy={} unsaved_draft={} pending_commit={} pending_remote={} active_remote={} same_target={}",
                        !can_switch,
                        self.saved_revision < self.draft_revision,
                        self.commit_in_flight.is_some(),
                        self.pending_remote_turn.is_some(),
                        self.active_remote_turn.is_some(),
                        self.local_conversation_id == target
                    );
                }
                let before = self.local_conversation_id;
                self.activate_local_conversation(target);
                let selected = self.local_conversation_id == target;
                if can_switch && selected {
                    // A hidden search filter must not hide the cycled-to row.
                    self.local_conversation_search_query.clear();
                    self.local_conversation_search_selection = None;
                }
                if before != self.local_conversation_id {
                    ctx.request_repaint();
                }
                if trace_cycle {
                    eprintln!("CHATARIUM_QA_CYCLE_AFTER selected={selected}");
                }
            }
        }
    }
}
