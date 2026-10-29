//! Read-only inline MCP card projection from the validated persistence worker.
//! A render frame only sends a command/polls a channel; it never scans or
//! duplicates the authoritative event buffer.
use super::*;
use std::sync::mpsc::{self, Receiver, TryRecvError};

type ProjectionReply = (Option<u64>, Option<InlineMcpStatus>);

#[derive(Clone)]
struct Pending {
    conversation_id: LocalConversationId,
    response: Arc<Mutex<Receiver<ProjectionReply>>>,
}

#[derive(Clone)]
struct Ready {
    conversation_id: LocalConversationId,
    watermark: Option<u64>,
    status: Option<InlineMcpStatus>,
}

#[derive(Clone, Default)]
struct Cache {
    ready: Option<Ready>,
    pending: Option<Pending>,
}

pub(super) enum InlineStatus {
    Pending,
    Ready(Option<InlineMcpStatus>),
    Unavailable(String),
}

/// The authoritative worker owns the current effective journal. Every reply
/// carries its actual final sequence. Never admit a card calculated against
/// a different GUI snapshot (even if the user changed chats while pending).
/// Older requests are drained before a new one is submitted.
pub(super) fn inline_status(
    ctx: &egui::Context,
    command_tx: Option<&Sender<PersistCommand>>,
    conversation_id: LocalConversationId,
    last_sequence: Option<u64>,
) -> InlineStatus {
    let key = egui::Id::new("chatarium-inline-mcp-projection-worker");
    let mut cache = ctx
        .data_mut(|data| data.get_temp::<Cache>(key))
        .unwrap_or_default();

    if let Some(pending) = &cache.pending {
        let polled = pending
            .response
            .try_lock()
            .ok()
            .map(|receiver| receiver.try_recv());
        match polled {
            Some(Ok((watermark, status))) => {
                cache.ready = Some(Ready {
                    conversation_id: pending.conversation_id,
                    watermark,
                    status,
                });
                cache.pending = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                cache.pending = None;
                // The request is lost, never substitute an older card.
                cache.ready = None;
                ctx.data_mut(|data| data.insert_temp(key, cache));
                return InlineStatus::Unavailable(
                    "MCP status worker disconnected; no unverified card displayed".to_owned(),
                );
            }
            _ => {}
        }
    }

    if let Some(ready) = &cache.ready {
        if ready.conversation_id == conversation_id && ready.watermark == last_sequence {
            let status = ready.status.clone();
            ctx.data_mut(|data| data.insert_temp(key, cache));
            return InlineStatus::Ready(status);
        }
    }
    if cache.pending.is_none() {
        let Some(commands) = command_tx else {
            ctx.data_mut(|data| data.insert_temp(key, cache));
            return InlineStatus::Unavailable(
                "MCP status projection unavailable without journal writer".to_owned(),
            );
        };
        let (reply, receiver) = mpsc::channel();
        if commands
            .send(PersistCommand::ProjectInlineMcpStatus {
                conversation_id,
                reply,
            })
            .is_err()
        {
            ctx.data_mut(|data| data.insert_temp(key, cache));
            return InlineStatus::Unavailable(
                "MCP status worker is no longer available".to_owned(),
            );
        }
        cache.pending = Some(Pending {
            conversation_id,
            response: Arc::new(Mutex::new(receiver)),
        });
    }
    ctx.data_mut(|data| data.insert_temp(key, cache));
    ctx.request_repaint_after(Duration::from_millis(40));
    InlineStatus::Pending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_receiver() -> (Sender<PersistCommand>, Receiver<PersistCommand>) {
        mpsc::channel()
    }

    fn respond(queue: &Receiver<PersistCommand>, watermark: Option<u64>) -> LocalConversationId {
        match queue.try_recv().expect("projection command sent") {
            PersistCommand::ProjectInlineMcpStatus {
                conversation_id,
                reply,
            } => {
                reply.send((watermark, None)).unwrap();
                conversation_id
            }
            _ => panic!("unexpected persistence command"),
        }
    }

    #[test]
    fn cached_card_is_scoped_to_exact_conversation_and_journal_high_water() {
        let ctx = egui::Context::default();
        let (tx, queue) = command_receiver();
        let first = LocalConversationId::new();
        let other = LocalConversationId::new();

        assert!(matches!(
            inline_status(&ctx, Some(&tx), first, Some(5)),
            InlineStatus::Pending
        ));
        assert_eq!(respond(&queue, Some(5)), first);
        assert!(matches!(
            inline_status(&ctx, Some(&tx), first, Some(5)),
            InlineStatus::Ready(None)
        ));
        assert!(queue.try_recv().is_err(), "cache hit must not re-replay");

        // A new GUI journal snapshot cannot reuse an older result.
        assert!(matches!(
            inline_status(&ctx, Some(&tx), first, Some(6)),
            InlineStatus::Pending
        ));
        assert_eq!(respond(&queue, Some(5)), first);
        assert!(matches!(
            inline_status(&ctx, Some(&tx), first, Some(6)),
            InlineStatus::Pending
        ));
        assert_eq!(respond(&queue, Some(6)), first);
        assert!(matches!(
            inline_status(&ctx, Some(&tx), first, Some(6)),
            InlineStatus::Ready(None)
        ));

        // A conversation switch must also invalidate the current card.
        assert!(matches!(
            inline_status(&ctx, Some(&tx), other, Some(6)),
            InlineStatus::Pending
        ));
        assert_eq!(respond(&queue, Some(6)), other);
        assert!(matches!(
            inline_status(&ctx, Some(&tx), other, Some(6)),
            InlineStatus::Ready(None)
        ));
    }

    #[test]
    fn disconnected_worker_never_falls_back_to_stale_status() {
        let ctx = egui::Context::default();
        let (tx, queue) = command_receiver();
        let owner = LocalConversationId::new();
        assert!(matches!(
            inline_status(&ctx, Some(&tx), owner, None),
            InlineStatus::Pending
        ));
        drop(queue);
        assert!(matches!(
            inline_status(&ctx, Some(&tx), owner, None),
            InlineStatus::Unavailable(_)
        ));
    }
}
