//! Behavior-preserving extraction of mirror methods from the desktop application.
use super::super::super::*;

impl ChatariumApp {
    pub(crate) fn ensure_mirror_controller(&mut self) -> bool {
        if self.mirror_controller_worker.is_some() {
            return true;
        }
        let Some(provider) = self.account_bridge_provider.clone() else {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail = Some("browser bridge unavailable".to_owned());
            return false;
        };
        let catalog = self.remote_conversation_catalog.clone();
        let events = self.events.clone();
        let (command_tx, command_rx) = mpsc::channel();
        let (notice_tx, notice_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("chatarium-mirror-controller".to_owned())
            .spawn(move || {
                mirror_controller_worker(provider, catalog, events, None, command_rx, notice_tx)
            });
        match worker {
            Ok(worker) => {
                self.mirror_controller_tx = Some(command_tx);
                self.mirror_controller_notice_rx = Some(notice_rx);
                self.mirror_controller_worker = Some(worker);
                true
            }
            Err(error) => {
                self.mirror_controller_state = MirrorControllerState::Failed;
                self.mirror_controller_detail = Some(format!(
                    "failed to start production mirror controller: {error}"
                ));
                false
            }
        }
    }

    pub(crate) fn start_mirror_controller(&mut self) {
        if self.ensure_mirror_controller() {
            self.send_mirror_controller_command(MirrorControllerCommand::Start);
        }
    }

    pub(crate) fn recheck_remote_health(&mut self) {
        if self.ensure_mirror_controller() {
            self.send_mirror_controller_command(MirrorControllerCommand::RecheckHealth);
        }
    }

    pub(crate) fn send_mirror_controller_command(&mut self, command: MirrorControllerCommand) {
        let Some(sender) = &self.mirror_controller_tx else {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail = Some("mirror controller is not initialized".to_owned());
            return;
        };
        if sender.send(command).is_err() {
            self.mirror_controller_state = MirrorControllerState::Failed;
            self.mirror_controller_detail =
                Some("mirror controller stopped unexpectedly".to_owned());
        }
    }

    pub(crate) fn process_mirror_controller_notices(&mut self) {
        let notices = self
            .mirror_controller_notice_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for notice in notices {
            match notice {
                MirrorControllerNotice::State {
                    state,
                    current_catalog_index,
                    detail,
                } => {
                    self.mirror_controller_state = state;
                    self.mirror_controller_current_item = current_catalog_index;
                    self.mirror_controller_detail = detail;
                }
                MirrorControllerNotice::Prepare {
                    remote_conversation_id,
                    catalog_index,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueuePrepare {
                            remote_conversation_id,
                            catalog_index,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::Capture {
                    remote_conversation_id,
                    body,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueueCapture {
                            remote_conversation_id,
                            body,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::Failure {
                    remote_conversation_id,
                    failure_class,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    if sender
                        .send(PersistCommand::MirrorQueueFailure {
                            remote_conversation_id,
                            failure_class,
                            reply,
                        })
                        .is_err()
                    {
                        self.mirror_controller_state = MirrorControllerState::Failed;
                        self.mirror_controller_detail =
                            Some("persistence worker stopped".to_owned());
                    }
                }
                MirrorControllerNotice::HealthSignal {
                    signal,
                    now_ms,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    let _ = sender.send(PersistCommand::RecordRemoteHealthSignal {
                        signal,
                        now_ms,
                        reply,
                    });
                }
                MirrorControllerNotice::HealthIntent {
                    intent,
                    now_ms,
                    reply,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        let _ = reply.send(Err("persistence worker unavailable".to_owned()));
                        continue;
                    };
                    let _ = sender.send(PersistCommand::RecordRemoteHealthIntent {
                        intent,
                        now_ms,
                        reply,
                    });
                }
            }
        }
    }

    pub(crate) fn start_history_discovery(&mut self, repaint: &egui::Context) {
        if self.history_list_pending {
            diagnostics::debug(
                "history",
                "discovery request ignored: one is already running",
            );
            return;
        }
        self.history_discovery_started = true;

        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.account_bridge_status = "listener unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        let durable_history_items_at_start = self.remote_conversation_catalog.len();
        self.history_list_pending = true;
        self.history_bridge_proven = false;
        self.account_bridge_status = "listener ready · checking Edge extension…".to_owned();
        diagnostics::info(
            "history",
            "discovery started: probing authenticated Edge bridge",
        );

        let spawn = thread::Builder::new()
            .name("chatarium-history-discovery".to_owned())
            .spawn(move || {
                use chatarium_core::authenticated_session::SessionAuthenticationEvidence;

                diagnostics::info("history", "authentication probe dispatched");
                match provider.probe_authentication() {
                    Ok(observation) => {
                        let authenticated = matches!(
                            observation.evidence,
                            SessionAuthenticationEvidence::Authenticated
                        );
                        diagnostics::info(
                            "history",
                            format!(
                                "authentication probe complete: authenticated={authenticated} HTTP={}",
                                observation.http_status
                            ),
                        );
                        let _ =
                            notices.send(LiveMirrorFetchNotice::HistoryAuthenticationObserved {
                                observation,
                            });
                        if !authenticated {
                            repaint.request_repaint();
                            return;
                        }
                    }
                    Err(error) => {
                        diagnostics::error("history", format!("authentication probe failed: {error}"));
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryProbeFailed { error });
                        repaint.request_repaint();
                        return;
                    }
                }

                diagnostics::info(
                    "history",
                    "authenticated; starting frozen 0.3 bounded CDP reload discovery",
                );
                match provider.discover_history_surfaces() {
                    Ok(observation) => {
                        let unique_items = observation
                            .candidates
                            .iter()
                            .flat_map(|candidate| candidate.items.iter().map(|item| item.id.as_str()))
                            .collect::<HashSet<_>>()
                            .len();
                        diagnostics::info(
                            "history",
                            format!(
                                "discovery complete: candidates={} unique-items={} responses={} backend-200={}",
                                observation.candidates.len(),
                                unique_items,
                                observation.proof.responses_seen,
                                observation.proof.backend_http_200_seen,
                            ),
                        );
                        for candidate in &observation.candidates {
                            diagnostics::debug(
                                "history",
                                format!(
                                    "candidate surface={} kind={} conversations={} observations={} cursor-count={} truncated={}",
                                    candidate.path,
                                    candidate.surface_kind,
                                    candidate.conversation_count,
                                    candidate.observations,
                                    candidate.cursor_count,
                                    candidate.traversal_truncated,
                                ),
                            );
                        }
                        if !should_run_fresh_tab_history_recovery(
                            unique_items,
                            durable_history_items_at_start,
                        ) {
                            let _ = notices.send(
                                LiveMirrorFetchNotice::HistoryDiscoveryLoaded { observation },
                            );
                        } else {
                            diagnostics::warn(
                                "history",
                                "frozen 0.3 discovery observed zero conversations with no durable cache; starting isolated fresh-tab recovery",
                            );
                            match provider.discover_history_surfaces_fresh_tab() {
                                Ok(fallback) => {
                                    let fallback_unique_items = fallback
                                        .candidates
                                        .iter()
                                        .flat_map(|candidate| {
                                            candidate.items.iter().map(|item| item.id.as_str())
                                        })
                                        .collect::<HashSet<_>>()
                                        .len();
                                    diagnostics::info(
                                        "history",
                                        format!(
                                            "fresh-tab recovery complete: candidates={} unique-items={} responses={} backend-200={}",
                                            fallback.candidates.len(),
                                            fallback_unique_items,
                                            fallback.proof.responses_seen,
                                            fallback.proof.backend_http_200_seen,
                                        ),
                                    );
                                    let _ = notices.send(
                                        LiveMirrorFetchNotice::HistoryFreshTabDiscoveryLoaded {
                                            primary: observation,
                                            fallback,
                                        },
                                    );
                                }
                                Err(error) => {
                                    diagnostics::error(
                                        "history",
                                        format!("fresh-tab recovery failed: {error}"),
                                    );
                                    let _ = notices.send(
                                        LiveMirrorFetchNotice::HistoryFreshTabDiscoveryFailed {
                                            primary: observation,
                                            error: error.to_string(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        diagnostics::error("history", format!("discovery failed: {error}"));
                        let _ = notices.send(LiveMirrorFetchNotice::HistoryDiscoveryFailed {
                            error: error.to_string(),
                        });
                    }
                }
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
            diagnostics::error(
                "history",
                format!("failed to start discovery worker: {error}"),
            );
            self.history_list_pending = false;
            self.account_bridge_status =
                format!("listener ready · failed to start browser check: {error}");
        }
    }

    pub(crate) fn open_discovered_remote_conversation(
        &mut self,
        remote_conversation_id: String,
        repaint: &egui::Context,
    ) {
        if self.remote_discovery_pending.is_some() || self.live_mirror_pending.is_some() {
            return;
        }
        if let Some(retry_after) = self
            .remote_mirror_retry_after
            .get(&remote_conversation_id)
            .copied()
        {
            let now = Instant::now();
            if retry_after > now {
                let remaining = retry_after.saturating_duration_since(now).as_secs().max(1);
                diagnostics::warn(
                    "mirror",
                    format!(
                        "rate-limit cooldown active for {}; retry in about {remaining}s",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                self.mirror_status =
                    format!("RATE LIMITED · retry available in about {remaining}s");
                self.status = "remote mirror is cooling down after ChatGPT HTTP 429".to_owned();
                return;
            }
            self.remote_mirror_retry_after
                .remove(&remote_conversation_id);
        }
        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.status = "cannot open remote chat: history bridge unavailable".to_owned();
            return;
        };

        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        diagnostics::info(
            "mirror",
            format!(
                "mirror requested for discovered conversation {}",
                diagnostics::short_id(&remote_conversation_id)
            ),
        );
        self.remote_mirror_failures.remove(&remote_conversation_id);
        self.remote_discovery_pending = Some(remote_conversation_id.clone());
        self.pending_history_fetch_proof = None;
        self.mirror_status =
            "FETCHING · opening temporary ChatGPT tab and waiting for first-party conversation response…"
                .to_owned();
        self.status =
            "mirroring remote ChatGPT conversation through first-party browser navigation…"
                .to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-remote-history-open".to_owned())
            .spawn(move || {
                diagnostics::info(
                    "mirror",
                    format!(
                        "browser capture started for {}",
                        diagnostics::short_id(&remote_conversation_id)
                    ),
                );
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(observation) => {
                        diagnostics::info(
                            "mirror",
                            format!(
                                "browser capture complete for {}: HTTP={} exact-response={} debugger={} network={}",
                                diagnostics::short_id(&remote_conversation_id),
                                observation.http_status,
                                observation.proof.exact_response_seen,
                                observation.proof.debugger_attached,
                                observation.proof.network_enabled,
                            ),
                        );
                        LiveMirrorFetchNotice::DiscoveredFetched {
                            remote_conversation_id,
                            body: observation.body,
                            proof: observation.proof,
                            http_status: observation.http_status,
                        }
                    }
                    Err(error) => {
                        diagnostics::error(
                            "mirror",
                            format!(
                                "browser capture failed for {}: {error}",
                                diagnostics::short_id(&remote_conversation_id)
                            ),
                        );
                        LiveMirrorFetchNotice::DiscoveredFailed {
                            remote_conversation_id,
                            error: error.to_string(),
                        }
                    }
                };
                let _ = notices.send(notice);
                repaint.request_repaint();
            });

        if let Err(error) = spawn {
            diagnostics::error("mirror", format!("failed to start mirror worker: {error}"));
            self.remote_discovery_pending = None;
            self.status = format!("failed to start remote conversation fetch: {error}");
        }
    }

    pub(crate) fn refresh_live_mirror_catalog(&mut self) {
        match latest_live_mirror_catalog(&self.events) {
            Ok(catalog) => {
                self.live_mirrored_conversations = catalog
                    .iter()
                    .map(|entry| entry.local_conversation_id)
                    .collect();
                self.live_mirror_catalog = catalog;
                self.remote_catalog_view = build_remote_catalog_view(
                    &self.remote_conversation_catalog,
                    &self.events,
                    &self.live_mirror_catalog,
                )
                .unwrap_or_default();
                self.local_archive_search_index =
                    build_local_archive_search_index(&self.remote_catalog_view, &self.events);
            }
            Err(error) => {
                self.status = format!("live mirror replay warning: {error}");
            }
        }
    }

    pub(crate) fn sync_historical_conversation(
        &mut self,
        local_conversation_id: LocalConversationId,
        repaint: &egui::Context,
    ) {
        if self.live_mirror_pending.is_some() {
            return;
        }
        let Some(entry) = self
            .historical_catalog
            .iter()
            .find(|entry| entry.local_conversation_id == local_conversation_id)
        else {
            self.status = "cannot sync: imported conversation is missing".to_owned();
            return;
        };
        let Some(mut provider) = self.account_bridge_provider.clone() else {
            self.status = format!("cannot sync from ChatGPT: {}", self.account_bridge_status);
            return;
        };
        let Some(sender) = self.persist_tx.clone() else {
            self.status = "cannot sync from ChatGPT: persistence unavailable".to_owned();
            return;
        };

        let remote_conversation_id = entry.remote_conversation_id.clone();
        let notices = self.live_mirror_fetch_tx.clone();
        let repaint = repaint.clone();
        self.live_mirror_pending = Some(local_conversation_id);
        self.pending_history_fetch_proof = None;
        self.mirror_status =
            "FETCHING · opening temporary ChatGPT tab and waiting for first-party conversation response…"
                .to_owned();
        self.status =
            "mirroring imported ChatGPT conversation through first-party navigation…".to_owned();

        let spawn = thread::Builder::new()
            .name("chatarium-live-mirror-fetch".to_owned())
            .spawn(move || {
                let notice = match provider
                    .fetch_authenticated_conversation(remote_conversation_id.as_str())
                {
                    Ok(observation) => LiveMirrorFetchNotice::Fetched {
                        local_conversation_id,
                        remote_conversation_id,
                        body: observation.body,
                        proof: observation.proof,
                        http_status: observation.http_status,
                    },
                    Err(error) => LiveMirrorFetchNotice::Failed {
                        local_conversation_id,
                        error: error.to_string(),
                    },
                };
                let _ = notices.send(notice);
                repaint.request_repaint();
                drop(sender);
            });

        if let Err(error) = spawn {
            self.live_mirror_pending = None;
            self.status = format!("failed to start live mirror fetch worker: {error}");
        }
    }

    pub(crate) fn process_live_mirror_fetch_notices(&mut self) {
        let notices = self
            .live_mirror_fetch_rx
            .try_iter()
            .collect::<Vec<LiveMirrorFetchNotice>>();

        for notice in notices {
            match notice {
                LiveMirrorFetchNotice::HistoryAuthenticationObserved { observation } => {
                    use chatarium_core::authenticated_session::SessionAuthenticationEvidence;

                    self.history_bridge_proven = false;
                    match observation.evidence {
                        SessionAuthenticationEvidence::Authenticated => {
                            self.account_bridge_status = format!(
                                "PROOF PARTIAL: {} · auth=yes · HTTP {} · parser=pending · semantic=pending · durable=pending",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        SessionAuthenticationEvidence::Unauthenticated => {
                            self.history_list_pending = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=no · HTTP {}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        SessionAuthenticationEvidence::Unknown => {
                            self.history_list_pending = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=unknown · HTTP {}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                    }
                }
                LiveMirrorFetchNotice::HistoryProbeFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status = history_probe_failure_status(&error);
                }
                LiveMirrorFetchNotice::HistoryListLoaded { observation } => {
                    self.history_list_pending = false;
                    self.remote_conversation_total = Some(observation.page.total);
                    self.remote_conversation_catalog = observation.page.items;

                    let semantic = classify_history_list_semantics(
                        observation.page.total,
                        !self.historical_catalog.is_empty() || !self.live_mirror_catalog.is_empty(),
                    );
                    match semantic {
                        HistoryListSemanticVerdict::Contradiction => {
                            self.history_bridge_proven = false;
                            self.account_bridge_status = format!(
                                "PROOF FAILED: {} · auth=yes · HTTP {} · parser=yes · semantic=contradiction(remote total=0 while local history exists)",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        HistoryListSemanticVerdict::UnconfirmedZero => {
                            self.history_bridge_proven = false;
                            self.account_bridge_status = format!(
                                "PROOF PARTIAL: {} · auth=yes · HTTP {} · parser=yes · semantic=unconfirmed-zero · items=0 · total=0",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                            );
                        }
                        HistoryListSemanticVerdict::Valid => {
                            self.history_bridge_proven = true;
                            self.account_bridge_status = format!(
                                "PROOF: {} · auth=yes · HTTP {} · parser=yes · semantic=yes · items={} · total={}",
                                browser_proof_label(&observation.proof),
                                observation.http_status,
                                self.remote_conversation_catalog.len(),
                                self.remote_conversation_total.unwrap_or(0),
                            );
                        }
                    }
                }
                LiveMirrorFetchNotice::HistoryListFailed { error } => {
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status =
                        format!("PROOF FAILED after authenticated extension path: {error}");
                }
                LiveMirrorFetchNotice::HistoryDiscoveryLoaded { observation } => {
                    diagnostics::info(
                        "history",
                        format!(
                            "UI received discovery result: surfaces={} responses={} backend-200={}",
                            observation.candidates.len(),
                            observation.proof.responses_seen,
                            observation.proof.backend_http_200_seen
                        ),
                    );
                    self.history_list_pending = false;
                    self.remote_conversation_total = None;

                    let candidate_count = observation.candidates.len();
                    let previously_observed = self.remote_conversation_catalog.len();
                    let mut best_surface = None::<(String, u64)>;
                    for candidate in &observation.candidates {
                        if best_surface
                            .as_ref()
                            .is_none_or(|(_, count)| candidate.conversation_count > *count)
                        {
                            best_surface =
                                Some((candidate.path.clone(), candidate.conversation_count));
                        }
                    }
                    let (catalog, current_pass_observed) = merge_history_discovery_catalog(
                        std::mem::take(&mut self.remote_conversation_catalog),
                        &observation.candidates,
                    );
                    self.remote_conversation_catalog = catalog;
                    self.remote_catalog_view = build_remote_catalog_view(
                        &self.remote_conversation_catalog,
                        &self.events,
                        &self.live_mirror_catalog,
                    )
                    .unwrap_or_default();

                    let best_surface = best_surface
                        .map(|(path, count)| format!("{path} ({count})"))
                        .unwrap_or_else(|| "none".to_owned());
                    if current_pass_observed == 0 {
                        diagnostics::warn(
                            "history",
                            format!(
                                "discovery completed with zero current-pass items; retained-items={previously_observed}"
                            ),
                        );
                        self.history_bridge_proven = false;
                        self.account_bridge_status = format!(
                            "DISCOVERY INCOMPLETE: {} · candidates={} · current-pass-items=0 · retained-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&observation.proof),
                            candidate_count,
                            previously_observed,
                            best_surface,
                        );
                    } else {
                        diagnostics::info(
                            "history",
                            format!(
                                "history catalog ready: current-pass-items={current_pass_observed} accumulated-items={} best-surface={best_surface}",
                                self.remote_conversation_catalog.len()
                            ),
                        );
                        let cache_path = remote_history_cache_path(&self.journal_path);
                        match persist_remote_history_cache(
                            &cache_path,
                            &self.remote_conversation_catalog,
                        ) {
                            Ok(()) => diagnostics::info(
                                "history",
                                format!(
                                    "discovered history cache updated: {} items at {}",
                                    self.remote_conversation_catalog.len(),
                                    cache_path.display()
                                ),
                            ),
                            Err(error) => diagnostics::warn(
                                "history",
                                format!("failed to update discovered history cache: {error}"),
                            ),
                        }
                        self.history_bridge_proven = true;
                        self.account_bridge_status = format!(
                            "DISCOVERED: {} · candidates={} · current-pass-items={} · accumulated-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&observation.proof),
                            candidate_count,
                            current_pass_observed,
                            self.remote_conversation_catalog.len(),
                            best_surface,
                        );
                    }
                }
                LiveMirrorFetchNotice::HistoryFreshTabDiscoveryLoaded { primary, fallback } => {
                    diagnostics::info(
                        "history",
                        format!(
                            "UI received fresh-tab recovery: primary-responses={} fallback-surfaces={} fallback-responses={} fallback-backend-200={}",
                            primary.proof.responses_seen,
                            fallback.candidates.len(),
                            fallback.proof.responses_seen,
                            fallback.proof.backend_http_200_seen,
                        ),
                    );
                    self.history_list_pending = false;
                    self.remote_conversation_total = None;

                    let previously_observed = self.remote_conversation_catalog.len();
                    let candidate_count = fallback.candidates.len();
                    let mut best_surface = None::<(String, u64)>;
                    for candidate in &fallback.candidates {
                        if best_surface
                            .as_ref()
                            .is_none_or(|(_, count)| candidate.conversation_count > *count)
                        {
                            best_surface =
                                Some((candidate.path.clone(), candidate.conversation_count));
                        }
                    }
                    let (catalog, current_pass_observed) = merge_history_discovery_catalog(
                        std::mem::take(&mut self.remote_conversation_catalog),
                        &fallback.candidates,
                    );
                    self.remote_conversation_catalog = catalog;
                    self.remote_catalog_view = build_remote_catalog_view(
                        &self.remote_conversation_catalog,
                        &self.events,
                        &self.live_mirror_catalog,
                    )
                    .unwrap_or_default();

                    let best_surface = best_surface
                        .map(|(path, count)| format!("{path} ({count})"))
                        .unwrap_or_else(|| "none".to_owned());

                    if current_pass_observed == 0 {
                        diagnostics::warn(
                            "history",
                            format!(
                                "fresh-tab recovery completed with zero items; retained-items={previously_observed}"
                            ),
                        );
                        self.history_bridge_proven = false;
                        self.account_bridge_status = format!(
                            "DISCOVERY INCOMPLETE: primary={} · fresh-tab={} · candidates={} · current-pass-items=0 · retained-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&primary.proof),
                            fresh_tab_history_discovery_proof_label(&fallback.proof),
                            candidate_count,
                            previously_observed,
                            best_surface,
                        );
                    } else {
                        diagnostics::info(
                            "history",
                            format!(
                                "fresh-tab history catalog ready: current-pass-items={current_pass_observed} accumulated-items={} best-surface={best_surface}",
                                self.remote_conversation_catalog.len()
                            ),
                        );
                        let cache_path = remote_history_cache_path(&self.journal_path);
                        match persist_remote_history_cache(
                            &cache_path,
                            &self.remote_conversation_catalog,
                        ) {
                            Ok(()) => diagnostics::info(
                                "history",
                                format!(
                                    "discovered history cache updated: {} items at {}",
                                    self.remote_conversation_catalog.len(),
                                    cache_path.display()
                                ),
                            ),
                            Err(error) => diagnostics::warn(
                                "history",
                                format!("failed to update discovered history cache: {error}"),
                            ),
                        }
                        self.history_bridge_proven = true;
                        self.account_bridge_status = format!(
                            "DISCOVERED VIA FRESH TAB: primary={} · fresh-tab={} · candidates={} · current-pass-items={} · accumulated-items={} · best-surface={} · coverage=unknown",
                            history_discovery_proof_label(&primary.proof),
                            fresh_tab_history_discovery_proof_label(&fallback.proof),
                            candidate_count,
                            current_pass_observed,
                            self.remote_conversation_catalog.len(),
                            best_surface,
                        );
                    }
                }
                LiveMirrorFetchNotice::HistoryFreshTabDiscoveryFailed { primary, error } => {
                    diagnostics::error(
                        "history",
                        format!(
                            "UI received fresh-tab recovery failure after primary zero-result: {error}"
                        ),
                    );
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status = format!(
                        "DISCOVERY INCOMPLETE: primary={} · fresh-tab-recovery-failed={} · retained-items={} · coverage=unknown",
                        history_discovery_proof_label(&primary.proof),
                        error,
                        self.remote_conversation_catalog.len(),
                    );
                }
                LiveMirrorFetchNotice::HistoryDiscoveryFailed { error } => {
                    diagnostics::error(
                        "history",
                        format!("UI received discovery failure: {error}"),
                    );
                    self.history_list_pending = false;
                    self.history_bridge_proven = false;
                    self.account_bridge_status =
                        format!("PROOF FAILED during CDP history discovery: {error}");
                }
                LiveMirrorFetchNotice::DiscoveredFetched {
                    remote_conversation_id,
                    body,
                    proof,
                    http_status,
                } => {
                    diagnostics::info(
                        "mirror",
                        format!(
                            "exact response arrived remote={} HTTP={http_status}; queuing validation/persistence",
                            diagnostics::short_id(&remote_conversation_id)
                        ),
                    );
                    let Some(sender) = &self.persist_tx else {
                        self.remote_discovery_pending = None;
                        self.remote_mirror_failures.insert(
                            remote_conversation_id.clone(),
                            "local persistence unavailable".to_owned(),
                        );
                        self.mirror_status =
                            "FAILED · exact first-party response captured, but local persistence is unavailable"
                                .to_owned();
                        self.status = "remote conversation fetched, but persistence is unavailable"
                            .to_owned();
                        continue;
                    };
                    self.pending_history_fetch_proof = Some(PendingHistoryFetchProof {
                        local_conversation_id: None,
                        remote_conversation_id: remote_conversation_id.clone(),
                        proof,
                        http_status,
                    });
                    self.mirror_status =
                        "VALIDATED · exact remote response captured · persisting durable local mirror…"
                            .to_owned();
                    if let Err(error) = sender.send(PersistCommand::PromoteDiscoveredLiveMirror {
                        expected_remote_conversation_id: remote_conversation_id.clone(),
                        body,
                    }) {
                        self.pending_history_fetch_proof = None;
                        self.remote_discovery_pending = None;
                        self.remote_mirror_failures
                            .insert(remote_conversation_id, error.to_string());
                        self.mirror_status =
                            format!("FAILED · could not queue durable mirror: {error}");
                        self.status = format!("failed to queue remote mirror creation: {error}");
                    } else {
                        self.status =
                            "remote conversation validated; creating durable live mirror…"
                                .to_owned();
                    }
                }
                LiveMirrorFetchNotice::DiscoveredFailed {
                    remote_conversation_id,
                    error,
                } => {
                    diagnostics::error(
                        "mirror",
                        format!(
                            "mirror fetch failed remote={}: {error}",
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
                    if error.contains("HTTP 429") {
                        self.remote_mirror_retry_after.insert(
                            remote_conversation_id.clone(),
                            Instant::now() + Duration::from_secs(60),
                        );
                        self.mirror_status =
                            "RATE LIMITED · ChatGPT returned HTTP 429 · cooldown 60s".to_owned();
                    } else {
                        self.mirror_status = format!("FAILED · mirror fetch: {error}");
                    }
                    self.status = format!("remote ChatGPT conversation fetch failed: {error}");
                }
                LiveMirrorFetchNotice::Fetched {
                    local_conversation_id,
                    remote_conversation_id,
                    body,
                    proof,
                    http_status,
                } => {
                    let Some(sender) = &self.persist_tx else {
                        self.live_mirror_pending = None;
                        self.mirror_status =
                            "FAILED · exact first-party response captured, but local persistence is unavailable"
                                .to_owned();
                        self.status =
                            "live conversation fetched, but persistence is unavailable".to_owned();
                        continue;
                    };
                    self.pending_history_fetch_proof = Some(PendingHistoryFetchProof {
                        local_conversation_id: Some(local_conversation_id),
                        remote_conversation_id: remote_conversation_id.clone(),
                        proof,
                        http_status,
                    });
                    self.mirror_status =
                        "VALIDATED · exact remote response captured · persisting durable local mirror…"
                            .to_owned();
                    if let Err(error) = sender.send(PersistCommand::PromoteHistoricalLiveMirror {
                        local_conversation_id,
                        expected_remote_conversation_id: remote_conversation_id,
                        body,
                    }) {
                        self.pending_history_fetch_proof = None;
                        self.live_mirror_pending = None;
                        self.mirror_status =
                            format!("FAILED · could not queue durable mirror: {error}");
                        self.status = format!("failed to queue live mirror promotion: {error}");
                    } else {
                        self.status =
                            "live response validated by browser; persisting mirror evidence…"
                                .to_owned();
                    }
                }
                LiveMirrorFetchNotice::Failed {
                    local_conversation_id,
                    error,
                } => {
                    if self.live_mirror_pending == Some(local_conversation_id) {
                        self.live_mirror_pending = None;
                    }
                    self.pending_history_fetch_proof = None;
                    self.mirror_status = format!("FAILED · mirror fetch: {error}");
                    self.status = format!("live ChatGPT fetch failed: {error}");
                }
            }
        }
    }

}
