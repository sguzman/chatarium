//! Feature-specific remote completion handling.
//! The outer dispatcher has no per-notice epilogue, so a handler return
//! is equivalent to the original outer-loop continue.
use super::*;

impl ChatariumApp {
    pub(super) fn apply_remote_notice(&mut self, notice: PersistNotice) {
        match notice {
            PersistNotice::HistoricalConversationLoaded {
                local_conversation_id,
                imported_sequence,
                messages,
            } => {
                if self.selected_historical_conversation == Some(local_conversation_id) {
                    self.historical_messages =
                        historical_display_messages(messages, imported_sequence);
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.historical_load_pending = None;
                    self.status = format!(
                        "verified historical snapshot loaded from import event #{imported_sequence}"
                    );
                }
            }
            PersistNotice::HistoricalConversationLoadFailed {
                local_conversation_id,
                error,
            } => {
                if self.selected_historical_conversation == Some(local_conversation_id) {
                    self.historical_load_pending = None;
                    self.loaded_historical_conversation = None;
                    self.historical_messages.clear();
                    self.status = format!("historical snapshot load failed: {error}");
                }
            }
            PersistNotice::LiveConversationLoaded {
                local_conversation_id,
                snapshot_sequence,
                truncated_before,
                messages,
            } => {
                if self.selected_historical_conversation == Some(local_conversation_id) {
                    self.historical_messages =
                        remote_display_messages(messages, snapshot_sequence);
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.historical_load_pending = None;
                    self.live_mirror_truncated_before = truncated_before;
                    self.status = format!(
                        "validated live mirror loaded from remote snapshot event #{snapshot_sequence}"
                    );
                }
            }
            PersistNotice::LiveConversationLoadFailed {
                local_conversation_id,
                error,
            } => {
                if self.selected_historical_conversation == Some(local_conversation_id) {
                    self.historical_load_pending = None;
                    self.loaded_historical_conversation = None;
                    self.historical_messages.clear();
                    self.status = format!("live mirror snapshot load failed: {error}");
                }
            }
            PersistNotice::HistoricalLiveMirrorPromoted {
                local_conversation_id,
                snapshot_sequence,
                appended_events,
                truncated_before,
                messages,
                projection_error,
            } => {
                self.events.extend(appended_events);
                self.refresh_live_mirror_catalog();
                if self.live_mirror_pending == Some(local_conversation_id) {
                    self.live_mirror_pending = None;
                }

                match self.pending_history_fetch_proof.take() {
                    Some(pending)
                        if pending.local_conversation_id == Some(local_conversation_id) =>
                    {
                        self.mirror_status = format!(
                            "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                            if truncated_before {
                                "MIRRORED PARTIAL"
                            } else {
                                "MIRRORED COMPLETE"
                            },
                            browser_proof_label(&pending.proof),
                            pending.http_status,
                            snapshot_sequence,
                        );
                    }
                    Some(pending) => {
                        self.mirror_status = format!(
                            "FAILED · durable event #{} has no matching browser proof (pending remote id {})",
                            snapshot_sequence, pending.remote_conversation_id,
                        );
                    }
                    None => {
                        self.mirror_status = format!(
                            "MIRRORED · durable event #{} committed without a current browser proof chain",
                            snapshot_sequence,
                        );
                    }
                }

                if self.selected_historical_conversation == Some(local_conversation_id) {
                    self.historical_load_pending = None;
                    self.loaded_historical_conversation = Some(local_conversation_id);
                    self.live_mirror_truncated_before = truncated_before;
                    if let Some(error) = projection_error {
                        self.historical_messages.clear();
                        self.status = format!(
                            "live mirror is durable, but visible transcript projection is blocked: {error}"
                        );
                    } else {
                        self.historical_messages =
                            remote_display_messages(messages, snapshot_sequence);
                        self.status = format!(
                            "ChatGPT live mirror durable at event #{snapshot_sequence}"
                        );
                    }
                } else {
                    self.status =
                        format!("ChatGPT live mirror durable at event #{snapshot_sequence}");
                }
            }
            PersistNotice::HistoricalLiveMirrorPromotionFailed {
                local_conversation_id,
                error,
            } => {
                if self.live_mirror_pending == Some(local_conversation_id) {
                    self.live_mirror_pending = None;
                }
                self.pending_history_fetch_proof = None;
                self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                self.status = format!("live mirror promotion failed: {error}");
            }
            PersistNotice::DiscoveredLiveMirrorPromoted {
                local_conversation_id,
                remote_conversation_id,
                snapshot_sequence,
                appended_events,
                truncated_before,
                messages,
                projection_error,
            } => {
                diagnostics::info(
                    "mirror",
                    format!(
                        "durable mirror notice remote={} local={local_conversation_id} event=#{snapshot_sequence} partial={truncated_before}",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                self.events.extend(appended_events);
                self.refresh_live_mirror_catalog();
                if self.remote_discovery_pending.as_deref()
                    == Some(remote_conversation_id.as_str())
                {
                    self.remote_discovery_pending = None;
                }
                self.selected_remote_catalog_id = None;
                self.selected_historical_conversation = Some(local_conversation_id);
                self.loaded_historical_conversation = Some(local_conversation_id);
                self.historical_load_pending = None;
                self.live_mirror_truncated_before = truncated_before;

                self.remote_mirror_failures.remove(&remote_conversation_id);
                self.remote_mirror_retry_after
                    .remove(&remote_conversation_id);
                match self.pending_history_fetch_proof.take() {
                    Some(pending)
                        if pending.local_conversation_id.is_none()
                            && pending.remote_conversation_id == remote_conversation_id =>
                    {
                        self.mirror_status = format!(
                            "{} · {} · HTTP {} · parser=yes · semantic=exact-id · durable=yes · event=#{}",
                            if truncated_before {
                                "MIRRORED PARTIAL"
                            } else {
                                "MIRRORED COMPLETE"
                            },
                            browser_proof_label(&pending.proof),
                            pending.http_status,
                            snapshot_sequence,
                        );
                    }
                    Some(pending) => {
                        self.mirror_status = format!(
                            "FAILED · durable event #{} did not match pending proof for remote id {}",
                            snapshot_sequence, pending.remote_conversation_id,
                        );
                    }
                    None => {
                        self.mirror_status = format!(
                            "MIRRORED · durable event #{} committed without a current browser proof chain",
                            snapshot_sequence,
                        );
                    }
                }

                if let Some(error) = projection_error {
                    self.historical_messages.clear();
                    self.status = format!(
                        "live mirror is durable, but visible transcript projection is blocked: {error}"
                    );
                } else {
                    self.historical_messages =
                        remote_display_messages(messages, snapshot_sequence);
                    self.status = format!(
                        "remote ChatGPT conversation mirrored locally at event #{snapshot_sequence}"
                    );
                }
            }
            PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                remote_conversation_id,
                error,
            } => {
                diagnostics::error(
                    "mirror",
                    format!(
                        "durable mirror commit failed remote={}: {error}",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                if self.remote_discovery_pending.as_deref()
                    == Some(remote_conversation_id.as_str())
                {
                    self.remote_discovery_pending = None;
                }
                self.pending_history_fetch_proof = None;
                self.remote_mirror_failures
                    .insert(remote_conversation_id.clone(), error.clone());
                self.mirror_status = format!("FAILED · durable mirror commit: {error}");
                self.status = format!("remote mirror creation failed: {error}");
            }
            PersistNotice::MirrorQueueUpdated { appended_events } => {
                self.events.extend(appended_events);
                self.refresh_live_mirror_catalog();
                self.status = "production mirror queue state durably updated".to_owned();
            }
            PersistNotice::RemoteHealthUpdated { event, controller } => {
                self.events.push(event);
                self.remote_health = controller;
                self.status = format!(
                    "remote health: {} · mirror intent: {}",
                    self.remote_health.state.as_str(),
                    self.remote_health.intent.as_str()
                );
            }
            _ => unreachable!("unexpected remote notice"),
        }
    }
}
