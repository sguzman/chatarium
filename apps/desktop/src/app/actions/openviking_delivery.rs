//! Behavior-preserving extraction of openviking_delivery methods from the desktop application.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn draft_state(&self) -> &'static str {
        if self.persist_tx.is_none() {
            "NOT DURABLE"
        } else if self.saved_revision >= self.draft_revision {
            "durable"
        } else {
            "saving…"
        }
    }

    /// Flush ready summaries even without model sign-in. The persistence
    /// worker alone claims each write. Uncertain writes are never replayed.
    pub(crate) fn maybe_dispatch_ready_openviking_write(&mut self) {
        if self.persist_tx.is_none()
            || std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0")
            || self
                .openviking_write_last_attempt
                .is_some_and(|when| when.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.openviking_write_last_attempt = Some(Instant::now());
        let path = self
            .journal_path
            .with_file_name(openviking_checkpoint_outbox::FILE_NAME);
        let outbox = match openviking_checkpoint_outbox::Outbox::load(&path) {
            Ok(outbox) => outbox,
            Err(error) => {
                self.openviking_checkpoint_status = format!("Queue unavailable: {error}");
                return;
            }
        };
        self.openviking_checkpoint_status =
            if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0") {
                "Automatic checkpoints disabled".to_owned()
            } else {
                outbox
                    .entries()
                    .iter()
                    .filter(|entry| entry.conversation_id == self.local_conversation_id.to_string())
                    .max_by_key(|entry| entry.completed_sequence)
                    .map(|entry| match entry.state {
                        openviking_checkpoint_outbox::State::PendingSummary => {
                            "Summary pending; waiting for idle model inference"
                        }
                        openviking_checkpoint_outbox::State::Ready => {
                            "Summary saved; awaiting safe write preflight"
                        }
                        openviking_checkpoint_outbox::State::Dispatched => {
                            "Write dispatched; exact verification pending"
                        }
                        openviking_checkpoint_outbox::State::Confirmed => {
                            "Checkpoint confirmed by exact read-back"
                        }
                        openviking_checkpoint_outbox::State::Uncertain => {
                            "Write uncertain; no automatic retry"
                        }
                        openviking_checkpoint_outbox::State::Skipped => {
                            "Checkpoint skipped; no durable memory"
                        }
                    })
                    .unwrap_or("No checkpoint yet; waiting for an eligible completed turn")
                    .to_owned()
            };
        if outbox.ready_entries().next().is_some() {
            if let Some(sender) = &self.persist_tx {
                let _ = sender.send(PersistCommand::DispatchOpenVikingReadyWrite);
            }
        }
    }

    /// One bounded, low-priority model summarization at a time, never during
    /// an active human/worker turn. The original transcript remains untouched.
    pub(crate) fn maybe_generate_openviking_summary(&mut self) {
        if self.persist_tx.is_none()
            || std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() == Ok("0")
            || self.openviking_summary_in_flight.is_some()
            || !self.remote_connected()
            || self.local_conversation_busy()
            || self.capability_probe.running()
            || self.sign_in_pending
            || self.selected_remote_catalog_id.is_some()
            || self.selected_historical_conversation.is_some()
            || self
                .openviking_summary_last_attempt
                .is_some_and(|when| when.elapsed() < Duration::from_secs(90))
        {
            return;
        }
        self.openviking_summary_last_attempt = Some(Instant::now());
        let path = self
            .journal_path
            .with_file_name(openviking_checkpoint_outbox::FILE_NAME);
        let outbox = match openviking_checkpoint_outbox::Outbox::load(&path) {
            Ok(value) => value,
            Err(error) => {
                self.status = format!("OpenViking memory queue cannot be read: {error}");
                return;
            }
        };
        for entry in outbox.pending_summaries() {
            let key = format!(
                "openviking-summary:{}:{}",
                entry.conversation_id, entry.turn_id
            );
            if self.openviking_summary_blocked.contains(&key) {
                continue;
            }
            let Ok(conversation_id) = entry.conversation_id.parse::<LocalConversationId>() else {
                continue;
            };
            let Ok(turn_id) = entry.turn_id.parse::<LocalTurnId>() else {
                continue;
            };
            if self
                .local_conversation_catalog
                .entry(conversation_id)
                .is_none()
            {
                continue;
            }
            let model = if conversation_id == self.local_conversation_id {
                self.selected_model.clone()
            } else {
                self.inference_settings
                    .for_conversation(conversation_id)
                    .model
            };
            let Some(model) = model else {
                continue;
            };
            if !self
                .remote_models
                .iter()
                .any(|available| available.slug == model)
            {
                continue;
            }
            let messages = projected_local_display_messages(&self.events, conversation_id);
            let Some(source) =
                openviking_memory_summary::bounded_excerpt(&messages, entry.completed_sequence)
            else {
                continue;
            };
            // Keep the rolling memory chain ordered: do not summarize a
            // later turn while an older checkpoint is still being generated
            // or awaiting a definitive external write for this conversation.
            if outbox.entries().iter().any(|older| {
                older.conversation_id == entry.conversation_id
                    && older.completed_sequence < entry.completed_sequence
                    && matches!(
                        older.state,
                        openviking_checkpoint_outbox::State::PendingSummary
                            | openviking_checkpoint_outbox::State::Ready
                            | openviking_checkpoint_outbox::State::Dispatched
                    )
                    && !self.openviking_summary_blocked.contains(&format!(
                        "openviking-summary:{}:{}",
                        older.conversation_id, older.turn_id
                    ))
            }) {
                continue;
            }
            let prior = match openviking_memory_summary::newest_confirmed_prior(
                &outbox,
                &path,
                &entry.conversation_id,
                entry.completed_sequence,
            ) {
                Ok(prior) => prior,
                Err(error) => {
                    self.openviking_summary_blocked.insert(key);
                    self.status = format!("OpenViking prior summary rejected: {error}");
                    continue;
                }
            };
            let input = openviking_memory_summary::request_input_with_prior(
                &entry.conversation_id,
                &entry.turn_id,
                entry.completed_sequence,
                &source,
                prior.as_ref(),
            );
            let request = siwc_bridge::BridgeCommand::StreamResponse {
                request_id: key.clone(),
                model,
                input,
                instructions: Some(openviking_memory_summary::GUIDANCE.to_owned()),
                request_patch: serde_json::json!({}),
            };
            if let Err(error) = self.remote.send(request) {
                self.openviking_summary_blocked.insert(key);
                self.status = format!("OpenViking summary model request not queued: {error}");
                return;
            }
            self.openviking_summary_in_flight = Some(OpenVikingSummaryAttempt {
                request_id: key,
                conversation_id,
                turn_id,
                completed_sequence: entry.completed_sequence,
                had_confirmed_prior: prior.is_some(),
            });
            return;
        }
    }
}
