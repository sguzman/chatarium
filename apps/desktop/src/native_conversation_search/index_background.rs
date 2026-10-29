//! Nonblocking native search index refresh for the GUI.
//! The only full event replay happens against the verified effective journal
//! owned by the persistence worker, never on the egui/render thread.
use super::*;
use crate::PersistCommand;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

#[derive(Clone, Default)]
struct PendingIndex {
    receiver: Option<Arc<Mutex<Receiver<Arc<NativeConversationSearchIndex>>>>>,
}

/// Return the currently attested native projection. The initial index is
/// seeded by the background bootstrap; later native/audit appends request a
/// new immutable projection from the sole authoritative persistence worker.
///
/// A worker result is not trusted merely because it arrived: the existing
/// prime_prebuilt_index check must match the GUI's *entire* revision key.
/// Older/future projections can never be displayed as this snapshot.
pub fn background_index(
    ctx: &egui::Context,
    events: &[EventEnvelope],
    commands: Option<&Sender<PersistCommand>>,
) -> Result<Option<Arc<NativeConversationSearchIndex>>, String> {
    if let Some(index) = index::ready_index_without_build(ctx, events) {
        return Ok(Some(index));
    }

    let id = egui::Id::new("chatarium-native-search-background-refresh");
    let mut pending = ctx
        .data_mut(|data| data.get_temp::<PendingIndex>(id))
        .unwrap_or_default();
    if let Some(receiver) = &pending.receiver {
        match receiver.try_lock().ok().map(|locked| locked.try_recv()) {
            Some(Ok(index)) => {
                pending.receiver = None;
                if index::prime_prebuilt_index(ctx, events, Arc::clone(&index)).is_ok() {
                    ctx.data_mut(|data| data.insert_temp(id, pending));
                    return Ok(Some(index));
                }
                // Newer (not yet notified) or older worker revision. Drop
                // instead of rendering/staging text against the wrong owner.
            }
            Some(Err(TryRecvError::Disconnected)) => {
                pending.receiver = None;
                ctx.data_mut(|data| data.insert_temp(id, pending));
                return Err(
                    "native index worker disconnected before returning a validated view"
                        .to_owned(),
                );
            }
            _ => {}
        }
    }
    if pending.receiver.is_none() {
        let Some(commands) = commands else {
            ctx.data_mut(|data| data.insert_temp(id, pending));
            return Err("native index worker is unavailable".to_owned());
        };
        let (reply, receiver) = mpsc::channel();
        if commands
            .send(PersistCommand::ProjectNativeSearchIndex { reply })
            .is_err()
        {
            ctx.data_mut(|data| data.insert_temp(id, pending));
            return Err("native index request could not reach the persistence worker".to_owned());
        }
        pending.receiver = Some(Arc::new(Mutex::new(receiver)));
    }
    ctx.data_mut(|data| data.insert_temp(id, pending));
    ctx.request_repaint_after(std::time::Duration::from_millis(40));
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
    use chatarium_store::{EventStore, MemoryEventStore};

    fn complete(
        commands: &Receiver<PersistCommand>,
        events: &[EventEnvelope],
    ) {
        let command = commands.try_recv().expect("background request");
        match command {
            PersistCommand::ProjectNativeSearchIndex { reply } => {
                reply.send(Arc::new(NativeConversationSearchIndex::build(events)))
                    .unwrap();
            }
            _ => panic!("unexpected persistence command"),
        }
    }

    #[test]
    fn refresh_rejects_future_index_and_never_uses_stale_native_corpus() {
        let ctx = egui::Context::default();
        let (tx, queue) = mpsc::channel();
        let mut store = MemoryEventStore::default();
        store
            .append(EventKind::DraftChanged, "first".to_owned())
            .unwrap();
        assert!(background_index(&ctx, store.events(), Some(&tx))
            .unwrap()
            .is_none());
        complete(&queue, store.events());
        let initial = background_index(&ctx, store.events(), Some(&tx))
            .unwrap()
            .unwrap();
        assert_eq!(initial.messages.len(), 0);
        assert!(Arc::ptr_eq(
            &initial,
            &background_index(&ctx, store.events(), Some(&tx))
                .unwrap()
                .unwrap()
        ));
        let owner = LocalConversationId::new();
        let authored = AuthoredUserMessage::new(
            owner,
            LocalTurnId::new(),
            LocalMessageId::new(),
            "fresh visible native user message",
        );
        chatarium_store::authored::commit_user_message(&mut store, &authored).unwrap();
        assert!(background_index(&ctx, store.events(), Some(&tx))
            .unwrap()
            .is_none());
        complete(&queue, store.events());
        let updated = background_index(&ctx, store.events(), Some(&tx))
            .unwrap()
            .unwrap();
        assert_eq!(updated.messages(owner).len(), 1);
        assert!(!Arc::ptr_eq(&initial, &updated));

        // A reply built from an unrelated later journal cannot be shown
        // against the current GUI view even when it has the same owner.
        let mut future = store.events().to_vec();
        future.push(EventEnvelope {
            sequence: future.last().unwrap().sequence + 1,
            at_unix_ms: 0,
            scope: None,
            kind: EventKind::DraftChanged,
            payload: "future".to_owned(),
        });
        let new_ctx = egui::Context::default();
        assert!(background_index(&new_ctx, store.events(), Some(&tx))
            .unwrap()
            .is_none());
        complete(&queue, &future);
        assert!(background_index(&new_ctx, store.events(), Some(&tx))
            .unwrap()
            .is_none());
        complete(&queue, store.events());
        assert!(background_index(&new_ctx, store.events(), Some(&tx))
            .unwrap()
            .is_some());
    }
}
