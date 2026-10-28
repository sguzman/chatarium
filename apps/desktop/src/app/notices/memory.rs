//! Feature-specific memory completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_memory_notice(&mut self, notice: PersistNotice) {
        match notice {
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
            _ => unreachable!("unexpected memory notice"),
        }
    }
}
