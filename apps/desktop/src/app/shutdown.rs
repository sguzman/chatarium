//! Desktop shutdown helpers mechanically relocated from the original crate root.
use super::super::*;

impl Drop for ChatariumApp {
    fn drop(&mut self) {
        if let Some(sender) = self.mirror_controller_tx.take() {
            let _ = sender.send(MirrorControllerCommand::Shutdown);
        }
        if let Some(worker) = self.mirror_controller_worker.take() {
            let _ = worker.join();
        }
        if let Some(sender) = self.persist_tx.take() {
            if let Some(active) = self.active_remote_turn.take() {
                let payload = remote_turn_payload(
                    active.turn_id,
                    &active.request_id,
                    None,
                    (!active.cumulative_text.is_empty()).then_some(active.cumulative_text.as_str()),
                    Some("Chatarium closed while the remote turn was active"),
                );
                let _ = sender.send(PersistCommand::AppendTurnEvent {
                    turn_id: active.turn_id,
                    kind: EventKind::TransportInterrupted,
                    payload,
                });
            } else if let Some(pending) = self.pending_remote_turn.take() {
                let payload = remote_turn_payload(
                    pending.turn_id,
                    &pending.request_id,
                    Some(&pending.model),
                    None,
                    Some("Chatarium closed after dispatch evidence but before provider outcome"),
                );
                let _ = sender.send(PersistCommand::AppendTurnEvent {
                    turn_id: pending.turn_id,
                    kind: EventKind::TransportInterrupted,
                    payload,
                });
            }
            let _ = sender.send(PersistCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
