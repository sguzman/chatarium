//! Desktop controller helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn controller_record_health_signal(
    notices: &Sender<MirrorControllerNotice>,
    signal: RemoteHealthSignal,
    health: &mut RemoteHealthController,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    notices
        .send(MirrorControllerNotice::HealthSignal {
            signal,
            now_ms: unix_now_ms(),
            reply: reply_tx,
        })
        .map_err(|error| error.to_string())?;
    *health = reply_rx.recv().map_err(|error| error.to_string())??;
    Ok(())
}

pub(crate) fn controller_record_health_intent(
    notices: &Sender<MirrorControllerNotice>,
    intent: MirrorIntent,
    health: &mut RemoteHealthController,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    notices
        .send(MirrorControllerNotice::HealthIntent {
            intent,
            now_ms: unix_now_ms(),
            reply: reply_tx,
        })
        .map_err(|error| error.to_string())?;
    *health = reply_rx.recv().map_err(|error| error.to_string())??;
    Ok(())
}

pub(crate) fn controller_check_remote_health(
    provider: &mut account_bridge::BrowserBridgeProvider,
    notices: &Sender<MirrorControllerNotice>,
    health: &mut RemoteHealthController,
) -> Result<bool, String> {
    let signal = match provider.probe_authentication() {
        Ok(observation) if observation.security_challenge => RemoteHealthSignal::ServerChallenge {
            http_status: observation.http_status,
        },
        Ok(observation)
            if matches!(
                observation.evidence,
                chatarium_core::authenticated_session::SessionAuthenticationEvidence::Authenticated
            ) =>
        {
            RemoteHealthSignal::Healthy {
                http_status: observation.http_status,
            }
        }
        Ok(_) => RemoteHealthSignal::AuthenticationRequired,
        Err(error) => remote_health_signal_from_error(&error),
    };
    controller_record_health_signal(notices, signal, health)?;
    Ok(health.state == chatarium_store::remote_health::RemoteHealthState::Healthy)
}

pub(crate) fn remote_health_signal_from_error(
    error: &account_bridge::BrowserBridgeError,
) -> RemoteHealthSignal {
    match error {
        account_bridge::BrowserBridgeError::RateLimited(_) => {
            RemoteHealthSignal::RateLimited { http_status: 429 }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status == 429 => {
            RemoteHealthSignal::RateLimited {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status == 403 => {
            RemoteHealthSignal::ServerChallenge {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::HttpStatus(status) if *status >= 500 => {
            RemoteHealthSignal::BackendUnavailable {
                http_status: *status,
            }
        }
        account_bridge::BrowserBridgeError::Unauthenticated => {
            RemoteHealthSignal::AuthenticationRequired
        }
        account_bridge::BrowserBridgeError::Timeout
        | account_bridge::BrowserBridgeError::Unavailable(_) => RemoteHealthSignal::NetworkUnstable,
        _ => RemoteHealthSignal::Unknown,
    }
}

pub(crate) fn mirror_controller_worker(
    mut provider: account_bridge::BrowserBridgeProvider,
    catalog: Vec<ConversationListItem>,
    events: Vec<EventEnvelope>,
    max_items: Option<usize>,
    commands: Receiver<MirrorControllerCommand>,
    notices: Sender<MirrorControllerNotice>,
) {
    let identities = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    let selected = match derive_remote_mirror_queue(&identities, &events) {
        Ok(summary) => summary.eligible_items(max_items.unwrap_or(usize::MAX)),
        Err(error) => {
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Failed,
                current_catalog_index: None,
                detail: Some(format!("queue replay failed: {error}")),
            });
            return;
        }
    };

    let mut next_item = 0usize;
    let mut running = false;
    let mut paused = false;
    let mut health =
        RemoteHealthController::from_events(&events, unix_now_ms()).unwrap_or_default();
    let _ = notices.send(MirrorControllerNotice::State {
        state: MirrorControllerState::Stopped,
        current_catalog_index: None,
        detail: None,
    });

    loop {
        if !running {
            match commands.recv() {
                Ok(MirrorControllerCommand::Start | MirrorControllerCommand::Resume) => {
                    if let Err(error) = controller_record_health_intent(
                        &notices,
                        MirrorIntent::Enabled,
                        &mut health,
                    ) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: None,
                            detail: Some(format!("remote health intent was not durable: {error}")),
                        });
                        continue;
                    }
                    let now_ms = unix_now_ms();
                    if health.cooldown_active(now_ms) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::RateLimited,
                            current_catalog_index: None,
                            detail: Some("remote health cooldown active; explicit recheck is required after expiry".to_owned()),
                        });
                        continue;
                    }
                    if !health.capture_allowed(now_ms) {
                        match controller_check_remote_health(&mut provider, &notices, &mut health) {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(error) => {
                                let _ = notices.send(MirrorControllerNotice::State {
                                    state: MirrorControllerState::Failed,
                                    current_catalog_index: None,
                                    detail: Some(format!("remote health check failed: {error}")),
                                });
                                continue;
                            }
                        }
                    }
                    running = true;
                    paused = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Running,
                        current_catalog_index: None,
                        detail: Some("serial production mirror worker running".to_owned()),
                    });
                }
                Ok(MirrorControllerCommand::Pause) => {
                    paused = true;
                    let _ = controller_record_health_intent(
                        &notices,
                        MirrorIntent::ManuallyPaused,
                        &mut health,
                    );
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Paused,
                        current_catalog_index: None,
                        detail: Some("paused before next item".to_owned()),
                    });
                }
                Ok(MirrorControllerCommand::RecheckHealth) => {
                    if health.cooldown_active(unix_now_ms()) {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::RateLimited,
                            current_catalog_index: None,
                            detail: Some(
                                "remote health cooldown active; no request sent".to_owned(),
                            ),
                        });
                    } else if let Err(error) =
                        controller_check_remote_health(&mut provider, &notices, &mut health)
                    {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: None,
                            detail: Some(format!("remote health recheck failed: {error}")),
                        });
                    }
                }
                Ok(MirrorControllerCommand::Shutdown) | Err(_) => return,
            }
            continue;
        }

        while let Ok(command) = commands.try_recv() {
            match command {
                MirrorControllerCommand::Pause => {
                    paused = true;
                    let _ = controller_record_health_intent(
                        &notices,
                        MirrorIntent::ManuallyPaused,
                        &mut health,
                    );
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::Paused,
                        current_catalog_index: None,
                        detail: Some("paused before next item".to_owned()),
                    });
                }
                MirrorControllerCommand::Start | MirrorControllerCommand::Resume => {
                    paused = false;
                }
                MirrorControllerCommand::RecheckHealth => {}
                MirrorControllerCommand::Shutdown => return,
            }
        }
        if paused {
            running = false;
            continue;
        }

        let Some(item) = selected.get(next_item).cloned() else {
            running = false;
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Completed,
                current_catalog_index: None,
                detail: Some("no eligible mirror items remain".to_owned()),
            });
            continue;
        };
        next_item += 1;
        let _ = notices.send(MirrorControllerNotice::State {
            state: MirrorControllerState::Running,
            current_catalog_index: Some(item.catalog_index),
            detail: Some("capturing one item serially".to_owned()),
        });

        if let Err(error) =
            controller_prepare_item(&notices, &item.remote_conversation_id, item.catalog_index)
        {
            running = false;
            let _ = notices.send(MirrorControllerNotice::State {
                state: MirrorControllerState::Failed,
                current_catalog_index: Some(item.catalog_index),
                detail: Some(format!("queue preparation failed: {error}")),
            });
            continue;
        }

        match provider.fetch_authenticated_conversation(&item.remote_conversation_id) {
            Ok(fetched) => {
                match controller_capture_item(&notices, &item.remote_conversation_id, fetched.body)
                {
                    Ok(persisted) => {
                        diagnostics::info(
                            "mirror",
                            format!(
                                "production controller completed catalog item {} state={} snapshot=#{}",
                                item.catalog_index,
                                persisted.mirror_state,
                                persisted.snapshot_sequence
                            ),
                        );
                    }
                    Err(error) => {
                        let _ = notices.send(MirrorControllerNotice::State {
                            state: MirrorControllerState::Failed,
                            current_catalog_index: Some(item.catalog_index),
                            detail: Some(format!("local promotion failed: {error}")),
                        });
                    }
                }
            }
            Err(error) => {
                let _ = controller_record_health_signal(
                    &notices,
                    remote_health_signal_from_error(&error),
                    &mut health,
                );
                let class = mirror_failure_class(&error);
                let _ = controller_record_failure(&notices, &item.remote_conversation_id, class);
                if class == MirrorFailureClass::RateLimited {
                    running = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::RateLimited,
                        current_catalog_index: Some(item.catalog_index),
                        detail: Some("HTTP 429; worker stopped without retry loop".to_owned()),
                    });
                } else if matches!(error, account_bridge::BrowserBridgeError::Unauthenticated) {
                    running = false;
                    let _ = notices.send(MirrorControllerNotice::State {
                        state: MirrorControllerState::AuthenticationRequired,
                        current_catalog_index: Some(item.catalog_index),
                        detail: Some("authentication required; worker stopped".to_owned()),
                    });
                }
            }
        }
    }
}

pub(crate) fn controller_prepare_item(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    catalog_index: usize,
) -> Result<(), String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Prepare {
            remote_conversation_id: remote_conversation_id.to_owned(),
            catalog_index,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

pub(crate) fn controller_capture_item(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    body: Value,
) -> Result<MirrorPersisted, String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Capture {
            remote_conversation_id: remote_conversation_id.to_owned(),
            body,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

pub(crate) fn controller_record_failure(
    notices: &Sender<MirrorControllerNotice>,
    remote_conversation_id: &str,
    failure_class: MirrorFailureClass,
) -> Result<(), String> {
    let (reply, result) = controller_reply_channel();
    notices
        .send(MirrorControllerNotice::Failure {
            remote_conversation_id: remote_conversation_id.to_owned(),
            failure_class,
            reply,
        })
        .map_err(|_| "desktop persistence channel closed".to_owned())?;
    result
        .recv()
        .map_err(|_| "desktop persistence worker stopped".to_owned())?
}

pub(crate) fn controller_reply_channel<T>() -> (Sender<Result<T, String>>, Receiver<Result<T, String>>) {
    mpsc::channel()
}

pub(crate) fn mirror_failure_class(error: &account_bridge::BrowserBridgeError) -> MirrorFailureClass {
    match error {
        account_bridge::BrowserBridgeError::RateLimited(_) => MirrorFailureClass::RateLimited,
        account_bridge::BrowserBridgeError::Protocol(_)
        | account_bridge::BrowserBridgeError::UnsupportedRevision(_) => {
            MirrorFailureClass::Structural
        }
        _ => MirrorFailureClass::Transient,
    }
}

pub(crate) fn start_account_bridge() -> (
    Option<account_bridge::AccountBridgeRuntime>,
    Option<account_bridge::BrowserBridgeProvider>,
    String,
) {
    if !ACCOUNT_MIRRORING_ENABLED {
        return (
            None,
            None,
            "ChatGPT history mirroring paused (local-only)".to_owned(),
        );
    }
    match account_bridge::AccountBridgeRuntime::start() {
        Ok(runtime) => {
            let provider = runtime.provider();
            (
                Some(runtime),
                Some(provider),
                "listener ready · waiting for Edge extension".to_owned(),
            )
        }
        Err(error) => (
            None,
            None,
            format!("browser history bridge unavailable: {error}"),
        ),
    }
}

pub(crate) fn latest_live_mirror_catalog(
    events: &[EventEnvelope],
) -> Result<Vec<LiveMirrorCatalogEntry>, String> {
    let records = replay_remote_conversation_snapshot_audit(events)?;
    let mut catalog = Vec::<LiveMirrorCatalogEntry>::new();

    for record in records {
        let truncated_before = project_remote_active_transcript(&record.envelope)
            .map(|projection| projection.truncated_before)
            .unwrap_or(true);
        let entry = LiveMirrorCatalogEntry {
            local_conversation_id: record.local_conversation_id,
            remote_conversation_id: record.remote_conversation_id.as_str().to_owned(),
            title: if record.envelope.title.trim().is_empty() {
                "Untitled ChatGPT conversation".to_owned()
            } else {
                record.envelope.title.clone()
            },
            snapshot_sequence: record.imported_sequence,
            truncated_before,
        };
        if let Some(existing) = catalog
            .iter_mut()
            .find(|existing| existing.local_conversation_id == entry.local_conversation_id)
        {
            *existing = entry;
        } else {
            catalog.push(entry);
        }
    }

    catalog.sort_by(|left, right| right.snapshot_sequence.cmp(&left.snapshot_sequence));
    Ok(catalog)
}

pub(crate) fn build_remote_catalog_view(
    catalog: &[ConversationListItem],
    events: &[EventEnvelope],
    live_mirrors: &[LiveMirrorCatalogEntry],
) -> Result<Vec<RemoteCatalogViewEntry>, String> {
    let identities = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    let queue = derive_remote_mirror_queue(&identities, events)?;

    Ok(queue
        .items
        .into_iter()
        .filter_map(|queue_item| {
            let item = catalog.get(queue_item.catalog_index)?.clone();
            let mirror = live_mirrors
                .iter()
                .find(|mirror| mirror.remote_conversation_id == item.id);
            Some(RemoteCatalogViewEntry {
                catalog_index: queue_item.catalog_index,
                item,
                status: queue_item.status,
                local_conversation_id: mirror.map(|mirror| mirror.local_conversation_id),
            })
        })
        .collect())
}

pub(crate) fn build_local_archive_search_index(
    catalog_view: &[RemoteCatalogViewEntry],
    events: &[EventEnvelope],
) -> local_archive_search::LocalArchiveSearchIndex {
    let documents = catalog_view
        .iter()
        .map(|entry| {
            let state = match entry.status {
                RemoteMirrorQueueStatus::MirroredFully => {
                    local_archive_search::ArchiveMirrorState::Mirrored
                }
                RemoteMirrorQueueStatus::MirroredPartial => {
                    local_archive_search::ArchiveMirrorState::Partial
                }
                RemoteMirrorQueueStatus::RateLimited => {
                    local_archive_search::ArchiveMirrorState::RateLimited
                }
                RemoteMirrorQueueStatus::TransientFailure => {
                    local_archive_search::ArchiveMirrorState::TransientFailure
                }
                RemoteMirrorQueueStatus::StructuralFailure => {
                    local_archive_search::ArchiveMirrorState::StructuralFailure
                }
                RemoteMirrorQueueStatus::Discovered
                | RemoteMirrorQueueStatus::Queued
                | RemoteMirrorQueueStatus::Capturing => {
                    local_archive_search::ArchiveMirrorState::NotMirrored
                }
            };
            let visible_messages = entry
                .local_conversation_id
                .and_then(|local_id| latest_live_transcript(events, local_id, None).ok())
                .map(|(_, projection)| {
                    projection
                        .messages
                        .into_iter()
                        .map(|message| message.text)
                        .collect()
                })
                .unwrap_or_default();
            local_archive_search::ArchiveSearchDocument {
                catalog_index: entry.catalog_index,
                title: entry
                    .item
                    .title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| "Untitled ChatGPT conversation".to_owned()),
                state,
                visible_messages,
            }
        })
        .collect();
    local_archive_search::LocalArchiveSearchIndex::new(documents)
}

pub(crate) fn remote_catalog_state_label(status: RemoteMirrorQueueStatus) -> &'static str {
    match status {
        RemoteMirrorQueueStatus::MirroredFully => "MIRRORED LOCALLY",
        RemoteMirrorQueueStatus::MirroredPartial => "MIRRORED LOCALLY · PARTIAL",
        RemoteMirrorQueueStatus::RateLimited => "RATE LIMITED",
        RemoteMirrorQueueStatus::TransientFailure => "TRANSIENT FAILURE",
        RemoteMirrorQueueStatus::StructuralFailure => "STRUCTURAL FAILURE",
        RemoteMirrorQueueStatus::Queued | RemoteMirrorQueueStatus::Capturing => {
            "REMOTE · MIRRORING"
        }
        RemoteMirrorQueueStatus::Discovered => "REMOTE · NOT MIRRORED",
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RemoteQueueCounts {
    pub(crate) observed: usize,
    pub(crate) full: usize,
    pub(crate) partial: usize,
    pub(crate) pending: usize,
    pub(crate) transient: usize,
    pub(crate) rate_limited: usize,
    pub(crate) structural: usize,
}

pub(crate) fn remote_catalog_queue_counts(entries: &[RemoteCatalogViewEntry]) -> RemoteQueueCounts {
    let mut counts = RemoteQueueCounts {
        observed: entries.len(),
        ..RemoteQueueCounts::default()
    };
    for entry in entries {
        match entry.status {
            RemoteMirrorQueueStatus::MirroredFully => counts.full += 1,
            RemoteMirrorQueueStatus::MirroredPartial => counts.partial += 1,
            RemoteMirrorQueueStatus::TransientFailure => counts.transient += 1,
            RemoteMirrorQueueStatus::RateLimited => counts.rate_limited += 1,
            RemoteMirrorQueueStatus::StructuralFailure => counts.structural += 1,
            RemoteMirrorQueueStatus::Discovered
            | RemoteMirrorQueueStatus::Queued
            | RemoteMirrorQueueStatus::Capturing => counts.pending += 1,
        }
    }
    counts
}

pub(crate) fn mirror_controller_state_label(state: MirrorControllerState) -> &'static str {
    match state {
        MirrorControllerState::Stopped => "STOPPED",
        MirrorControllerState::Running => "RUNNING · CONCURRENCY=1",
        MirrorControllerState::Paused => "PAUSED",
        MirrorControllerState::RateLimited => "PAUSED · RATE LIMITED",
        MirrorControllerState::AuthenticationRequired => "PAUSED · AUTHENTICATION REQUIRED",
        MirrorControllerState::Completed => "COMPLETED",
        MirrorControllerState::Failed => "STOPPED · FAILURE",
    }
}

pub(crate) fn latest_live_transcript(
    events: &[EventEnvelope],
    local_conversation_id: LocalConversationId,
    exact_snapshot_sequence: Option<u64>,
) -> Result<(u64, RemoteTranscriptProjection), String> {
    let records = replay_remote_conversation_snapshot_audit(events)?;
    let record = match exact_snapshot_sequence {
        Some(sequence) => records.iter().find(|record| {
            record.local_conversation_id == local_conversation_id
                && record.imported_sequence == sequence
        }),
        None => records
            .iter()
            .rev()
            .find(|record| record.local_conversation_id == local_conversation_id),
    }
    .ok_or_else(|| {
        format!("no durable live mirror snapshot exists for conversation {local_conversation_id}")
    })?;

    let projection = project_remote_active_transcript(&record.envelope)?;
    Ok((record.imported_sequence, projection))
}

pub(crate) fn load_capability_probe_state(journal_path: &Path) -> capability_probes::ProbeRun {
    let data_dir = journal_path.parent().unwrap_or_else(|| Path::new("."));
    let report_path = data_dir.join("siwc-capability-probes.json");
    let contract_path = data_dir.join("local-inference-contract.json");
    match capability_probes::load_report(&report_path) {
        Ok(Some(mut run)) => {
            let contract_status = match local_inference_contract::save_contract(
                &contract_path,
                &run,
                unix_now_ms(),
            ) {
                Ok(()) => format!(
                    "contract {}",
                    local_inference_contract::contract_state(&run)
                ),
                Err(error) => format!("contract error: {error}"),
            };
            run.status = format!(
                "loaded {} saved capability probe results · {contract_status}",
                run.results.len()
            );
            run
        }
        Ok(None) => capability_probes::ProbeRun::default(),
        Err(error) => capability_probes::ProbeRun {
            status: format!("saved capability probe report ignored · {error}"),
            ..capability_probes::ProbeRun::default()
        },
    }
}

pub(crate) fn load_local_inference_contract_state(
    journal_path: &Path,
) -> Option<local_inference_contract::LoadedContract> {
    let data_dir = journal_path.parent().unwrap_or_else(|| Path::new("."));
    let contract_path = data_dir.join("local-inference-contract.json");
    match local_inference_contract::load_contract(&contract_path) {
        Ok(contract) => contract,
        Err(error) => {
            diagnostics::warn(
                "inference",
                format!("local inference contract ignored: {error}"),
            );
            None
        }
    }
}

pub(crate) fn probe_report_age(generated_unix_ms: u64) -> String {
    let elapsed = unix_now_ms().saturating_sub(generated_unix_ms);
    let seconds = elapsed / 1_000;
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

pub(crate) fn probe_result_detail(result: &capability_probes::ProbeResult) -> String {
    let mut detail = match (&result.code, result.status_code) {
        (Some(code), Some(status)) => format!(" · {code} · HTTP {status}"),
        (Some(code), None) => format!(" · {code}"),
        (None, Some(status)) => format!(" · HTTP {status}"),
        (None, None) => String::new(),
    };
    if let Some(param) = result.param.as_deref() {
        detail.push_str(&format!(" · param {param}"));
    }
    if let Some(request_id) = result.upstream_request_id.as_deref() {
        detail.push_str(&format!(" · request {request_id}"));
    }
    if let Some(response_shape) = result.response_shape.as_deref() {
        detail.push_str(&format!(" · shape {response_shape}"));
    }
    detail
}

pub(crate) fn capability_probe_block_reason(
    remote_connected: bool,
    model_selected: bool,
    pending_remote_turn: bool,
    active_remote_turn: bool,
    probe_running: bool,
) -> Option<&'static str> {
    if !remote_connected {
        return Some("ChatGPT plan connection is not ready");
    }
    if !model_selected {
        return Some("choose an account-visible model");
    }
    if pending_remote_turn || active_remote_turn {
        return Some("finish or stop the current response first");
    }
    if probe_running {
        return Some("capability probe suite is already running");
    }
    None
}

pub(crate) fn model_retry_available(connected: bool, request_pending: bool, models_empty: bool) -> bool {
    connected && !request_pending && models_empty
}

pub(crate) fn should_request_models(
    was_connected: bool,
    account_changed: bool,
    models_empty: bool,
    model_list_pending: bool,
) -> bool {
    (!was_connected || account_changed || models_empty) && !model_list_pending
}

pub(crate) fn session_identity_changed(
    previous: &siwc_bridge::SessionState,
    next: &siwc_bridge::SessionState,
) -> bool {
    previous.profile_id != next.profile_id
        || previous.email != next.email
        || previous.profile_label != next.profile_label
}

pub(crate) fn sign_in_pending_after_session(was_pending: bool, status: &str) -> bool {
    match status {
        "connecting" => true,
        "connected" => false,
        // RefreshSession replies can be stale while a new OAuth request waits
        // on keyring preflight. The sign-in command's terminal result, not a
        // disconnected snapshot, is authoritative for stopping the spinner.
        _ => was_pending,
    }
}

pub(crate) fn failed_bridge_command_finishes_sign_in(request_id: Option<&str>) -> bool {
    request_id == Some("sign-in")
}

pub(crate) fn remote_error_is_observed_failure(error: &siwc_bridge::BridgeError) -> bool {
    error.status.is_some()
        || error.code.starts_with("subscription_sharing_")
        || error.code.starts_with("chatpass_")
        || matches!(
            error.code.as_str(),
            "invalid_request"
                | "invalid_request_error"
                | "model_not_found"
                | "invalid_token"
                | "invalid_api_key"
                | "invalid_client"
                | "sign_in_required"
                | "sharing_not_enabled"
                | "refresh_not_ready"
                | "connection_busy"
                | "response_incomplete"
        )
}
