//! Feature-specific openviking completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_openviking_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::ProjectOpenVikingRetrievalFinished {
                conversation_id,
                generation,
                result,
            } => {
                let result = self.openviking_project_cache.finish(
                    conversation_id,
                    generation,
                    result,
                    Instant::now(),
                    &self.local_conversation_catalog,
                    &self.project_memory_grants,
                );
                if let openviking_project_cache::Completion::Deferred(error) = result {
                    if conversation_id == self.local_conversation_id {
                        self.status = format!(
                            "Project memory retrieval unavailable; continuing without shared memory: {error}"
                        );
                    }
                }
            }
            PersistNotice::OpenVikingRetrievalFinished {
                conversation_id,
                result,
            } => {
                self.openviking_retrieval_in_flight.remove(&conversation_id);
                if self.openviking_retrieval_stale.remove(&conversation_id) {
                    // A checkpoint was confirmed while this read was in
                    // flight; its old snapshot cannot refresh the cache.
                    self.openviking_retrieval_cache.remove(&conversation_id);
                    self.openviking_retrieval_retry_after
                        .remove(&conversation_id);
                    return;
                }
                match result {
                    Ok(checkpoints) => {
                        self.openviking_retrieval_cache
                            .insert(conversation_id, checkpoints);
                        self.openviking_retrieval_retry_after
                            .remove(&conversation_id);
                    }
                    Err(error) => {
                        // The conversation remains usable. Retry after a
                        // bounded delay rather than on every UI frame.
                        self.openviking_retrieval_retry_after
                            .insert(conversation_id, Instant::now() + Duration::from_secs(300));
                        if conversation_id == self.local_conversation_id {
                            self.status = format!(
                                "OpenViking retrieval unavailable; continuing without memory: {error}"
                            );
                        }
                    }
                }
            }
            PersistNotice::OpenVikingMemoryWriteFinished {
                conversation_id,
                confirmed,
            } => {
                if confirmed {
                    self.invalidate_project_memory_cache();
                    // Invalidate any conversation cache affected by the
                    // newly confirmed immutable checkpoint.
                    let affected = self
                        .openviking_retrieval_cache
                        .keys()
                        .chain(self.openviking_retrieval_retry_after.keys())
                        .chain(self.openviking_retrieval_in_flight.iter())
                        .copied()
                        .filter(|id| id.to_string() == conversation_id)
                        .collect::<Vec<_>>();
                    for owner in affected {
                        self.openviking_retrieval_cache.remove(&owner);
                        self.openviking_retrieval_retry_after.remove(&owner);
                        if self.openviking_retrieval_in_flight.contains(&owner) {
                            self.openviking_retrieval_stale.insert(owner);
                        }
                    }
                }
                if self.local_conversation_id.to_string() == conversation_id {
                    self.status = if confirmed {
                        "OpenViking memory checkpoint saved and verified by read-back".to_owned()
                    } else {
                        "OpenViking checkpoint write uncertain; not automatically repeated"
                            .to_owned()
                    };
                    self.openviking_checkpoint_status = self.status.clone();
                }
            }
            PersistNotice::OpenVikingMemorySummarySaved {
                conversation_id,
                turn_id,
                skipped,
            } => {
                if self
                    .openviking_summary_in_flight
                    .as_ref()
                    .is_some_and(|attempt| {
                        attempt.conversation_id == conversation_id && attempt.turn_id == turn_id
                    })
                {
                    self.openviking_summary_in_flight = None;
                }
                if conversation_id == self.local_conversation_id {
                    self.status = if skipped {
                        "OpenViking checkpoint skipped: nothing durable to retain".to_owned()
                    } else {
                        "OpenViking summary durable locally; scheduling verified external write"
                            .to_owned()
                    };
                    self.openviking_checkpoint_status = self.status.clone();
                }
                if !skipped
                    && std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() != Ok("0")
                {
                    if let Some(sender) = &self.persist_tx {
                        let _ = sender.send(PersistCommand::DispatchOpenVikingReadyWrite);
                    }
                }
            }
            _ => unreachable!("unexpected openviking notice"),
        }
    }
}
