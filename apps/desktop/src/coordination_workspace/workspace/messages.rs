//! messages methods for human-controlled coordination containers.
use super::super::*;

impl Workspace {
    /// Source identity is resolved by the application from its active conversation.
    /// A model may choose a target but cannot provide or override its sender identity.
    pub fn send(
        &mut self,
        conversation: &str,
        to: u64,
        kind: &str,
        body: &str,
        now: u64,
    ) -> Result<(u64, String), String> {
        if !kind_ok(kind) {
            return Err("unknown coordination message kind".to_owned());
        }
        if body.is_empty() || body.len() > MAX_MESSAGE || body.contains('\0') {
            return Err("coordination message must contain 1–8192 bytes".to_owned());
        }
        let (container, from) = self
            .containers
            .iter()
            .find_map(|c| {
                c.members
                    .iter()
                    .find(|m| m.conversation == conversation)
                    .map(|m| (c.id, m.id))
            })
            .ok_or("current chat is not a coordination worker")?;
        let id = self.allocate();
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .unwrap();
        if !c.members.iter().any(|m| m.id == to) {
            return Err("target worker is outside this container".to_owned());
        }
        if to == from {
            return Err("workers cannot message themselves".to_owned());
        }
        let route = Self::route_for(c, from, to);
        let authorized = route.as_ref().is_some_and(|(l, _)| {
            if kind == "directive" {
                l.directives
            } else {
                l.reports
            }
        });
        let recent = route.as_ref().map_or(0, |_| {
            c.messages
                .iter()
                .filter(|m| {
                    m.from == from
                        && m.to == to
                        && m.delivered_ms
                            .is_some_and(|at| now.saturating_sub(at) < 60_000)
                })
                .count()
        });
        // A fresh send cannot overtake an older automatically rate-limited
        // message merely because the delivery window has reopened. The
        // existing queue pump will release the oldest eligible message first.
        let rate_backlog = route.as_ref().is_some_and(|(l, _)| {
            l.mode == "rate_limit"
                && c.messages.iter().any(|message| {
                    message.from == from
                        && message.to == to
                        && message.status == "queued"
                        && message.note.starts_with("rate limit reached")
                })
        });
        let (status, note) = match route.as_ref() {
            None => ("blocked", "no authorized authority or message edge"),
            Some(_) if !authorized => ("blocked", "message kind not authorized on this edge"),
            Some((l, _)) if l.mode == "block" => ("blocked", "edge policy blocks delivery"),
            Some((l, _)) if l.mode == "queue" => ("queued", "awaiting manual queue release"),
            Some((l, _)) if l.mode == "approve" => ("approval", "awaiting human approval"),
            Some((l, _)) if l.mode == "rate_limit" && rate_backlog => {
                ("queued", "rate limit reached; queued behind older messages")
            }
            Some((l, _))
                if l.mode == "rate_limit"
                    && (l.per_minute == 0 || recent >= l.per_minute as usize) =>
            {
                (
                    "queued",
                    "rate limit reached; awaiting capacity or manual release",
                )
            }
            Some((_, true)) => ("delivered", "accepted by automatic report-only return path"),
            Some(_) => ("delivered", "accepted by configured edge policy"),
        };
        c.messages.push(Message {
            id,
            from,
            to,
            kind: kind.to_owned(),
            body: body.to_owned(),
            status: status.to_owned(),
            created_ms: now,
            delivered_ms: (status == "delivered").then_some(now),
            consumed_ms: None,
            note: note.to_owned(),
        });
        Ok((id, status.to_owned()))
    }
    pub fn decide(&mut self, container: u64, id: u64, allow: bool, now: u64) -> Result<(), String> {
        let c = self
            .containers
            .iter_mut()
            .find(|c| c.id == container)
            .ok_or("container not found")?;
        let record = c
            .messages
            .iter()
            .find(|m| m.id == id)
            .ok_or("message not found")?
            .clone();
        if !matches!(record.status.as_str(), "queued" | "approval") {
            return Err("message is not pending".to_owned());
        }
        // Recheck current authority at delivery, so revoked power cannot be
        // smuggled through a formerly eligible queue.
        let permitted = Self::route_for(c, record.from, record.to).is_some_and(|(l, _)| {
            l.mode != "block"
                && if record.kind == "directive" {
                    l.directives
                } else {
                    l.reports
                }
        });
        let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
        if allow && permitted {
            m.status = "delivered".to_owned();
            m.delivered_ms = Some(now);
            m.note = "released by human operator".to_owned();
        } else {
            m.status = "rejected".to_owned();
            m.note = "human veto or revoked authority".to_owned();
        }
        Ok(())
    }
    /// Release rate-limited messages once their per-edge window has capacity.
    /// Manual queues and approval-gated messages never auto-release. Revoked
    /// permissions fail closed, retaining the rejected message in history.
    pub fn advance_rate_queues(&mut self, now: u64) -> usize {
        let mut changed = 0_usize;
        for c in &mut self.containers {
            let mut ids = c
                .messages
                .iter()
                .filter(|m| m.status == "queued" && m.note.starts_with("rate limit reached"))
                .map(|m| m.id)
                .collect::<Vec<_>>();
            // IDs are allocated in send order, independent of archive row order.
            ids.sort_unstable();
            for id in ids {
                let Some(record) = c.messages.iter().find(|m| m.id == id).cloned() else {
                    continue;
                };
                let link = Self::route_for(c, record.from, record.to);
                let valid = link.as_ref().is_some_and(|(l, _)| {
                    l.mode == "rate_limit"
                        && if record.kind == "directive" {
                            l.directives
                        } else {
                            l.reports
                        }
                });
                if !valid {
                    let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
                    m.status = "rejected".to_owned();
                    m.note = "queued authority was revoked".to_owned();
                    changed += 1;
                    continue;
                }
                let limit = link.unwrap().0.per_minute as usize;
                let within_window = c
                    .messages
                    .iter()
                    .filter(|m| {
                        m.from == record.from
                            && m.to == record.to
                            && m.delivered_ms
                                .is_some_and(|time| now.saturating_sub(time) < 60_000)
                    })
                    .count();
                if within_window < limit {
                    let m = c.messages.iter_mut().find(|m| m.id == id).unwrap();
                    m.status = "delivered".to_owned();
                    m.delivered_ms = Some(now);
                    m.note = "rate window opened; delivered in FIFO order".to_owned();
                    changed += 1;
                }
            }
        }
        changed
    }

    /// IDs admitted to the recipient's next model context. Inspection alone never consumes.
    /// Use the same size limits as identity_context so a truncated inbox stays pending.
    pub fn context_delivered_ids(&self, conversation: &str) -> Vec<u64> {
        let Some((container, worker)) = self.member_for(conversation) else {
            return Vec::new();
        };
        let Some(c) = self
            .containers
            .iter()
            .find(|container_record| container_record.id == container)
        else {
            return Vec::new();
        };
        let mut length = 0_usize;
        let mut ids = Vec::new();
        let candidates = c
            .messages
            .iter()
            .filter(|message| message.to == worker && message.status == "delivered")
            .rev()
            .take(12)
            .collect::<Vec<_>>();
        for message in candidates.into_iter().rev() {
            let sender = c
                .members
                .iter()
                .find(|member| member.id == message.from)
                .map(|member| member.name.as_str())
                .unwrap_or("Unknown");
            let bytes = format!(
                "INBOX #{} [{}] FROM {} (#{}) at {}: {}\\n",
                message.id, message.kind, sender, message.from, message.created_ms, message.body
            )
            .len();
            if length + bytes > 6_000 {
                break;
            }
            length += bytes;
            ids.push(message.id);
        }
        ids
    }

    /// Transition messages admitted to an actual model dispatch to durable consumed history.
    /// Only delivered messages owned by this conversation can transition.
    pub fn consume_ids(&mut self, conversation: &str, ids: &[u64], now: u64) -> usize {
        let Some((container, worker)) = self.member_for(conversation) else {
            return 0;
        };
        let Some(c) = self
            .containers
            .iter_mut()
            .find(|record| record.id == container)
        else {
            return 0;
        };
        let mut changed = 0;
        for message in &mut c.messages {
            if message.to == worker && message.status == "delivered" && ids.contains(&message.id) {
                message.status = "consumed".to_owned();
                message.consumed_ms = Some(now);
                message.note =
                    "admitted to recipient model request; preserved in transcript".to_owned();
                changed += 1;
            }
        }
        changed
    }

    /// Explicit human deletion of a recipient inbox payload or queued item.
    /// A content-free tombstone retains its identity and delivery timestamp for
    /// rate limiting and audit. Consumed transcript messages are not inbox deletable.
    pub fn delete_message(&mut self, container: u64, id: u64) -> Result<(), String> {
        let record = self
            .containers
            .iter_mut()
            .find(|record| record.id == container)
            .ok_or("container not found")?;
        let index = record
            .messages
            .iter()
            .position(|message| message.id == id)
            .ok_or("message not found")?;
        if record.messages[index].status == "consumed" {
            return Err("consumed transcript records cannot be deleted from the inbox".to_owned());
        }
        let message = &mut record.messages[index];
        message.status = "deleted".to_owned();
        // Erase the payload while retaining a minimal tombstone so rate limits
        // cannot be bypassed by deleting delivered inbox entries.
        message.body.clear();
        message.note = "deleted from inbox by human".to_owned();
        Ok(())
    }
}

impl Workspace {
    pub fn inbox_counts(&self, conversation: &str) -> (usize, usize) {
        let Some((cid, worker)) = self.member_for(conversation) else {
            return (0, 0);
        };
        let Some(c) = self.containers.iter().find(|container| container.id == cid) else {
            return (0, 0);
        };
        let pending = c
            .messages
            .iter()
            .filter(|m| m.to == worker && m.status == "delivered")
            .count();
        let queued = c
            .messages
            .iter()
            .filter(|m| m.to == worker && matches!(m.status.as_str(), "queued" | "approval"))
            .count();
        (pending, queued)
    }

    pub fn consumed_for(&self, conversation: &str) -> Vec<ConsumedDisplay> {
        let Some((cid, worker)) = self.member_for(conversation) else {
            return Vec::new();
        };
        let Some(c) = self.containers.iter().find(|container| container.id == cid) else {
            return Vec::new();
        };
        let mut list = c
            .messages
            .iter()
            .filter(|m| m.to == worker && m.status == "consumed")
            .map(|m| ConsumedDisplay {
                id: m.id,
                from: c
                    .members
                    .iter()
                    .find(|w| w.id == m.from)
                    .map(|w| format!("{} (#{})", w.name, w.id))
                    .unwrap_or_else(|| format!("Worker #{}", m.from)),
                kind: m.kind.clone(),
                body: m.body.clone(),
                consumed_ms: m
                    .consumed_ms
                    .unwrap_or(m.delivered_ms.unwrap_or(m.created_ms)),
            })
            .collect::<Vec<_>>();
        list.sort_by_key(|message| (message.consumed_ms, message.id));
        list
    }
}
