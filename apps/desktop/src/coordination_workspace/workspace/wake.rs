//! wake methods for human-controlled coordination containers.
use super::super::*;

impl Workspace {
    /// Called only by a trusted operator control, never by model-generated
    /// coordination.send content. Grant a finite *total* number of claims.
    pub fn grant_wake_lease(&mut self, conversation: &str, allowance: u32) -> Result<(), String> {
        if allowance == 0 || allowance > MAX_WAKE_ALLOWANCE {
            return Err("wake allowance must be between 1 and 8".to_owned());
        }
        if self.member_for(conversation).is_none() {
            return Err("wake lease owner is not a coordination worker".to_owned());
        }
        if let Some(lease) = self
            .wake_leases
            .iter_mut()
            .find(|lease| lease.conversation == conversation)
        {
            if lease.active_message_id.is_some()
                || lease.claimed_message_ids.len() + allowance as usize > MAX_WAKE_CLAIMS
            {
                return Err("wake claim is active or history has reached its bound".to_owned());
            }
            lease.remaining = allowance;
        } else {
            self.wake_leases.push(WakeLease {
                conversation: conversation.to_owned(),
                remaining: allowance,
                active_message_id: None,
                claimed_message_ids: Vec::new(),
            });
        }
        Ok(())
    }

    /// Stop future wake claims. An already claimed message stays in history
    /// and can never be silently replayed after stop or restart.
    pub fn stop_wake_lease(&mut self, conversation: &str) -> Result<(), String> {
        if self.member_for(conversation).is_none() {
            return Err("wake lease owner is not a coordination worker".to_owned());
        }
        if let Some(lease) = self
            .wake_leases
            .iter_mut()
            .find(|lease| lease.conversation == conversation)
        {
            lease.remaining = 0;
            lease.active_message_id = None;
        }
        Ok(())
    }

    /// Dispatch-time revalidation: the claim must remain active and its
    /// original sender must still hold directive authority over the recipient.
    /// A queued/approval route is eligible only after explicit release.
    pub fn validate_wake_claim(&self, conversation: &str, message_id: u64) -> Result<(), String> {
        let lease = self
            .wake_leases
            .iter()
            .find(|lease| lease.conversation == conversation)
            .ok_or("no wake lease")?;
        if lease.active_message_id != Some(message_id) {
            return Err("wake claim is not active".to_owned());
        }
        let (container_id, worker) = self
            .member_for(conversation)
            .ok_or("wake claim owner is not a worker")?;
        let c = self
            .containers
            .iter()
            .find(|c| c.id == container_id)
            .ok_or("wake claim container missing")?;
        let message = c
            .messages
            .iter()
            .find(|message| message.id == message_id && message.to == worker)
            .ok_or("wake claim message missing")?;
        // Admission changes an already dispatched directive to "consumed".
        // Preserve its proof for same-turn tool continuations without making
        // other consumed messages eligible for fresh wake claims.
        let admitted = message.status == "consumed"
            && message.consumed_ms.is_some()
            && message.note == "admitted to recipient model request; preserved in transcript";
        if (message.status != "delivered" && !admitted) || message.kind != "directive" {
            return Err("wake claim message is not a delivered/admitted directive".to_owned());
        }
        let authorized = Self::route_for(c, message.from, worker).is_some_and(|(edge, _)| {
            edge.directives
                && match edge.mode.as_str() {
                    "allow" | "rate_limit" => true,
                    "queue" | "approve" => message.note == "released by human operator" || admitted,
                    _ => false,
                }
        });
        if !authorized {
            return Err("wake claim sender authority was revoked or needs approval".to_owned());
        }
        Ok(())
    }

    /// A read-only scheduling hint; claim and network dispatch recheck policy.
    pub fn next_eligible_wake_id(&self, conversation: &str) -> Option<u64> {
        let lease = self
            .wake_leases
            .iter()
            .find(|lease| lease.conversation == conversation)?;
        if lease.remaining == 0
            || lease.active_message_id.is_some()
            || lease.claimed_message_ids.len() >= MAX_WAKE_CLAIMS
        {
            return None;
        }
        let (container_id, worker) = self.member_for(conversation)?;
        self.containers
            .iter()
            .find(|container| container.id == container_id)?
            .messages
            .iter()
            .filter(|message| {
                message.to == worker
                    && message.status == "delivered"
                    && !lease.claimed_message_ids.contains(&message.id)
                    && self.wake_message_is_authorized(conversation, message.id)
            })
            .map(|message| message.id)
            .min()
    }

    /// Reserve one eligible incoming directive; nothing is executed here.
    /// Caller must durably save the workspace before any inference dispatch.
    /// Active claims survive restart and must never be automatically replayed.
    pub fn claim_next_wake(&mut self, conversation: &str) -> Result<Option<u64>, String> {
        let Some(index) = self
            .wake_leases
            .iter()
            .position(|lease| lease.conversation == conversation)
        else {
            return Ok(None);
        };
        if self.wake_leases[index].claimed_message_ids.len() >= MAX_WAKE_CLAIMS {
            return Err("wake claim history has reached its bound".to_owned());
        }
        let next = self.next_eligible_wake_id(conversation);
        if let Some(message_id) = next {
            let lease = &mut self.wake_leases[index];
            lease.remaining -= 1;
            lease.active_message_id = Some(message_id);
            lease.claimed_message_ids.push(message_id);
        }
        Ok(next)
    }

    fn wake_message_is_authorized(&self, conversation: &str, message_id: u64) -> bool {
        let Some((container_id, worker)) = self.member_for(conversation) else {
            return false;
        };
        let Some(container) = self.containers.iter().find(|c| c.id == container_id) else {
            return false;
        };
        let Some(message) = container.messages.iter().find(|m| m.id == message_id) else {
            return false;
        };
        if message.to != worker || message.status != "delivered" || message.kind != "directive" {
            return false;
        }
        Self::route_for(container, message.from, worker).is_some_and(|(edge, _)| {
            edge.directives
                && match edge.mode.as_str() {
                    "allow" | "rate_limit" => true,
                    "queue" | "approve" => message.note == "released by human operator",
                    _ => false,
                }
        })
    }

    /// Close one claim after the external dispatcher has durably recorded its
    /// terminal outcome. Failure/stop does not refund allowance or replay work.
    pub fn finish_wake_claim(&mut self, conversation: &str, message_id: u64) -> Result<(), String> {
        let lease = self
            .wake_leases
            .iter_mut()
            .find(|lease| lease.conversation == conversation)
            .ok_or("no wake lease")?;
        if lease.active_message_id != Some(message_id) {
            return Err("wake claim is not active".to_owned());
        }
        lease.active_message_id = None;
        Ok(())
    }

    /// Persist a tentative claim before any outbound inference can begin.
    /// Copy-on-write prevents a failed disk save from consuming the budget
    /// only in memory. On crash after rename, replay sees the active claim
    /// and never automatically retries it.
    pub fn claim_next_wake_durable(
        &mut self,
        path: &Path,
        conversation: &str,
    ) -> Result<Option<WakeDispatchEnvelope>, String> {
        let mut next = self.clone();
        let Some(message_id) = next.claim_next_wake(conversation)? else {
            return Ok(None);
        };
        let envelope = next.wake_dispatch_envelope(conversation, message_id)?;
        next.save_atomic(path)?;
        *self = next;
        Ok(Some(envelope))
    }

    /// The exact, approved directive for the claimed message. Do not infer it
    /// from the ordinary 12-item inbox projection: old but eligible directives
    /// can fall outside that window when later messages arrive.
    pub fn wake_dispatch_envelope(
        &self,
        conversation: &str,
        message_id: u64,
    ) -> Result<WakeDispatchEnvelope, String> {
        self.validate_wake_claim(conversation, message_id)?;
        let (container_id, worker) = self
            .member_for(conversation)
            .ok_or("wake claim owner is not a worker")?;
        let container = self
            .containers
            .iter()
            .find(|container| container.id == container_id)
            .ok_or("wake claim container missing")?;
        let message = container
            .messages
            .iter()
            .find(|message| message.id == message_id && message.to == worker)
            .ok_or("wake claim message missing")?;
        let sender_name = container
            .members
            .iter()
            .find(|member| member.id == message.from)
            .ok_or("wake claim sender missing")?
            .name
            .clone();
        Ok(WakeDispatchEnvelope {
            recipient_conversation: conversation.to_owned(),
            message_id,
            sender_id: message.from,
            sender_name,
            directive_text: message.body.clone(),
        })
    }

    /// Operator-authorized budget mutation must cross the same persistence
    /// boundary as a claim. No model/tool message can call this implicitly.
    pub fn grant_wake_lease_durable(
        &mut self,
        path: &Path,
        conversation: &str,
        allowance: u32,
    ) -> Result<(), String> {
        let mut next = self.clone();
        next.grant_wake_lease(conversation, allowance)?;
        next.save_atomic(path)?;
        *self = next;
        Ok(())
    }

    /// A stop is durably persisted before the dispatcher may observe it.
    /// It cannot roll back an already transmitted remote operation.
    pub fn stop_wake_lease_durable(
        &mut self,
        path: &Path,
        conversation: &str,
    ) -> Result<(), String> {
        let mut next = self.clone();
        next.stop_wake_lease(conversation)?;
        next.save_atomic(path)?;
        *self = next;
        Ok(())
    }

    /// Only acknowledge completion of a claim once its non-authored remote
    /// turn has a separately durable terminal result. Never refund allowance.
    pub fn finish_wake_claim_durable(
        &mut self,
        path: &Path,
        conversation: &str,
        message_id: u64,
    ) -> Result<(), String> {
        let mut next = self.clone();
        next.finish_wake_claim(conversation, message_id)?;
        next.save_atomic(path)?;
        *self = next;
        Ok(())
    }
}
