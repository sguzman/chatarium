//! Existing single-writer command loop; extracted without changing ordering.
use super::*;

pub(super) fn persistence_worker(
    mut store: chatarium_store::effective_event_store::EffectiveEventStore<JsonlEventStore>,
    data_dir: PathBuf,
    commands: Receiver<PersistCommand>,
    completion_tx: Sender<PersistCommand>,
    notices: Sender<PersistNotice>,
) {
    diagnostics::info(
        "persist",
        format!(
            "persistence worker ready: {} existing events",
            store.events().len()
        ),
    );
    let checkpoint_outbox_path = data_dir.join(openviking_checkpoint_outbox::FILE_NAME);
    if let Err(error) = reconcile_checkpoint_outbox(&store, &checkpoint_outbox_path) {
        let _ = notices.send(PersistNotice::Failed {
            operation: "automatic memory checkpoint startup reconciliation",
            revision: None,
            request_id: None,
            turn_id: None,
            error,
        });
    }
    let mut openviking_write_active = false;
    // Production writes require a FRESH synthetic collision attestation for
    // each Ready checkpoint, performed off the journal persistence thread.
    // Nothing is marked Dispatched until the matching attestation succeeds.
    let mut openviking_preflight_active = false;
    let mut openviking_preflight_verified: Option<(String, String, u64)> = None;
    let mut openviking_preflight_verified_at: Option<Instant> = None;
    let mut openviking_preflight_retry_after: Option<Instant> = None;
    if let Ok(mut outbox) = openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path) {
        if let Err(error) = outbox.recover_interrupted_dispatches_durable(&checkpoint_outbox_path) {
            let _ = notices.send(PersistNotice::Failed {
                operation: "OpenViking interrupted write recovery",
                revision: None,
                request_id: None,
                turn_id: None,
                error,
            });
        }
    }
    // Recover only a small bounded set of crash-uncertain writes. Never
    // reissue create: the background worker performs exact read-only checks,
    // and ONLY the persistence worker may transition an entry to Confirmed.
    if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() != Ok("0") {
        if let Ok(outbox) = openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path) {
            let uncertain = outbox
                .entries()
                .iter()
                .filter(|entry| entry.state == openviking_checkpoint_outbox::State::Uncertain)
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            if !uncertain.is_empty() {
                let path = checkpoint_outbox_path.clone();
                let tx = completion_tx.clone();
                if let Err(error) = thread::Builder::new()
                    .name("chatarium-openviking-recover-read".to_owned())
                    .spawn(move || {
                        for entry in uncertain {
                            let result = openviking_memory_delivery::verify_uncertain_read_only(
                                &path,
                                &entry.conversation_id,
                                &entry.turn_id,
                                entry.completed_sequence,
                            );
                            if tx
                                .send(PersistCommand::RecordOpenVikingUncertainReadback {
                                    conversation_id: entry.conversation_id,
                                    turn_id: entry.turn_id,
                                    completed_sequence: entry.completed_sequence,
                                    result,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "OpenViking read-only crash recovery worker",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error: error.to_string(),
                    });
                }
            }
        }
    }
    while let Ok(command) = commands.recv() {
        match command {
            PersistCommand::ProjectNativeSearchIndex { reply } => {
                // Project against the validated effective journal owned by
                // this worker. Never transfer or clone archive payloads into
                // the GUI just to build a sidebar/reader search index.
                let index = Arc::new(
                    native_conversation_search::NativeConversationSearchIndex::build(store.events()),
                );
                let _ = reply.send(index);
            }
            PersistCommand::ProjectInlineMcpStatus {
                conversation_id,
                reply,
            } => {
                // The effective writer is already validated. Full journal
                // audit replays stay on this worker, never in an egui frame.
                // Send the actual high-water mark so the GUI cannot display
                // a result for a stale or not-yet-applied notice snapshot.
                let watermark = store.events().last().map(|event| event.sequence);
                let status = project_inline_mcp_status(store.events(), conversation_id);
                let _ = reply.send((watermark, status));
            }
            PersistCommand::RecordOpenVikingUncertainReadback {
                conversation_id,
                turn_id,
                completed_sequence,
                result,
            } => {
                // Mismatch, missing file, or network failure proves nothing.
                // In all such cases leave the durable state Uncertain.
                if result != Ok(true) {
                    continue;
                }
                let confirmed = openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)
                    .and_then(|mut outbox| {
                        outbox.transition_durable(
                            &checkpoint_outbox_path,
                            &conversation_id,
                            &turn_id,
                            completed_sequence,
                            openviking_checkpoint_outbox::State::Uncertain,
                            openviking_checkpoint_outbox::State::Confirmed,
                        )
                    });
                match confirmed {
                    Ok(true) => {
                        let _ = notices.send(PersistNotice::OpenVikingMemoryWriteFinished {
                            conversation_id,
                            confirmed: true,
                        });
                    }
                    Ok(false) => {}
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking read-only state confirmation",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RetrieveConfirmedProjectOpenViking {
                conversation_id,
                generation,
                catalog,
                grants,
                query,
            } => {
                let outbox_path = checkpoint_outbox_path.clone();
                let sender = notices.clone();
                let spawned = thread::Builder::new()
                    .name("chatarium-openviking-project-retrieve".to_owned())
                    .spawn(move || {
                        let result =
                            openviking_memory_delivery::retrieve_confirmed_for_project_query(
                                &outbox_path,
                                &catalog,
                                &grants,
                                conversation_id,
                                &query,
                            );
                        let _ = sender.send(PersistNotice::ProjectOpenVikingRetrievalFinished {
                            conversation_id,
                            generation,
                            result,
                        });
                    });
                if let Err(error) = spawned {
                    let _ = notices.send(PersistNotice::ProjectOpenVikingRetrievalFinished {
                        conversation_id,
                        generation,
                        result: Err(format!("cannot launch project memory read worker: {error}")),
                    });
                }
            }
            PersistCommand::RetrieveConfirmedOpenViking {
                conversation_id,
                query,
            } => {
                let outbox_path = checkpoint_outbox_path.clone();
                let sender = notices.clone();
                let spawned = thread::Builder::new()
                    .name("chatarium-openviking-retrieve".to_owned())
                    .spawn(move || {
                        let result =
                            openviking_memory_delivery::retrieve_confirmed_for_conversation_query(
                                &outbox_path,
                                &conversation_id.to_string(),
                                &query,
                            );
                        let _ = sender.send(PersistNotice::OpenVikingRetrievalFinished {
                            conversation_id,
                            result,
                        });
                    });
                if let Err(error) = spawned {
                    let _ = notices.send(PersistNotice::OpenVikingRetrievalFinished {
                        conversation_id,
                        result: Err(format!("cannot launch OpenViking read worker: {error}")),
                    });
                }
            }
            PersistCommand::OpenVikingWritePreflightFinished { selected, result } => {
                openviking_preflight_active = false;
                match result {
                    Ok(()) => {
                        openviking_preflight_retry_after = None;
                        openviking_preflight_verified_at = Some(Instant::now());
                        openviking_preflight_verified = Some(selected);
                        // Return to the same persistence-worker dispatch path,
                        // which checks that the attested Ready entry still matches.
                        let _ = completion_tx.send(PersistCommand::DispatchOpenVikingReadyWrite);
                    }
                    Err(error) => {
                        openviking_preflight_verified = None;
                        openviking_preflight_verified_at = None;
                        openviking_preflight_retry_after =
                            Some(Instant::now() + Duration::from_secs(1800));
                        // Report failure once; do not consume or mark uncertain
                        // any queued checkpoint when no production write ran.
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking write safety preflight (checkpoint remains Ready)",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchOpenVikingReadyWrite => {
                if openviking_write_active || openviking_preflight_active {
                    continue;
                }
                // Only the oldest Ready checkpoint may be attested and sent.
                // Never preflight a queue with no pending write.
                let next_ready = (|| -> Result<Option<(String, String, u64)>, String> {
                    let outbox =
                        openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)?;
                    Ok(outbox.ready_entries().next().map(|entry| {
                        (
                            entry.conversation_id.clone(),
                            entry.turn_id.clone(),
                            entry.completed_sequence,
                        )
                    }))
                })();
                let selected = match next_ready {
                    Ok(Some(selected)) => selected,
                    Ok(None) => continue,
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking write safety queue inspection",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                        continue;
                    }
                };
                if !openviking_write_preflight::matches_ready(
                    &selected,
                    openviking_preflight_verified.as_ref(),
                ) && !openviking_write_preflight::recently_attested(
                    openviking_preflight_verified_at,
                    Instant::now(),
                ) {
                    // Expired proof is not transferable, even to the same
                    // queued checkpoint after a long period of inactivity.
                    openviking_preflight_verified = None;
                    openviking_preflight_verified_at = None;
                    if openviking_preflight_retry_after.is_some_and(|until| Instant::now() < until)
                    {
                        continue;
                    }
                    openviking_preflight_active = true;
                    let tx = completion_tx.clone();
                    let spawned = thread::Builder::new()
                        .name("chatarium-openviking-write-preflight".to_owned())
                        .spawn(move || {
                            let result =
                                openviking_write_preflight::verify_before_checkpoint_write();
                            let _ = tx.send(PersistCommand::OpenVikingWritePreflightFinished {
                                selected,
                                result,
                            });
                        });
                    if let Err(error) = spawned {
                        openviking_preflight_active = false;
                        openviking_preflight_retry_after =
                            Some(Instant::now() + Duration::from_secs(1800));
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking write safety preflight launch",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error: error.to_string(),
                        });
                    }
                    continue;
                }
                // Single-use queue identity proof. The backend compatibility
                // lease may cover other queued checkpoints for five minutes.
                // Their queue identity and source bytes are separately checked.
                openviking_preflight_verified = None;
                let claim = (|| -> Result<Option<(String, String, u64, String)>, String> {
                    let mut outbox =
                        openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)?;
                    let Some(entry) = outbox.ready_entries().next().cloned() else {
                        return Ok(None);
                    };
                    // Recheck at the exact durable claim, not just before
                    // opening the queue: a different process may have changed
                    // its head after the proof was matched.
                    if (
                        entry.conversation_id.as_str(),
                        entry.turn_id.as_str(),
                        entry.completed_sequence,
                    ) != (selected.0.as_str(), selected.1.as_str(), selected.2)
                    {
                        return Ok(None);
                    }
                    let summary_path = openviking_checkpoint_outbox::Outbox::summary_path(
                        &checkpoint_outbox_path,
                        &entry.conversation_id,
                        &entry.turn_id,
                    )?;
                    let content = std::fs::read_to_string(&summary_path).map_err(|error| {
                        format!("cannot read pending OpenViking summary: {error}")
                    })?;
                    let expected_header = format!(
                        "# Chatarium memory checkpoint\n\nConversation: {}\nTurn: {}\nSource journal sequence: {}\n\n",
                        entry.conversation_id, entry.turn_id, entry.completed_sequence,
                    );
                    if content.is_empty()
                        || content.len() > 16384
                        || !content.starts_with(&expected_header)
                    {
                        return Err(
                            "OpenViking summary file fails source or size validation".to_owned()
                        );
                    }
                    outbox.transition_durable(
                        &checkpoint_outbox_path,
                        &entry.conversation_id,
                        &entry.turn_id,
                        entry.completed_sequence,
                        openviking_checkpoint_outbox::State::Ready,
                        openviking_checkpoint_outbox::State::Dispatched,
                    )?;
                    Ok(Some((
                        entry.conversation_id,
                        entry.turn_id,
                        entry.completed_sequence,
                        content,
                    )))
                })();
                match claim {
                    Ok(Some((conversation_id, turn_id, completed_sequence, content))) => {
                        // Persisted claim precedes any network write. A crash
                        // afterwards produces "uncertain", never an automatic
                        // duplicate create.
                        openviking_write_active = true;
                        let tx = completion_tx.clone();
                        let owner = conversation_id.clone();
                        let turn = turn_id.clone();
                        let spawned = thread::Builder::new()
                            .name("chatarium-openviking-write".to_owned())
                            .spawn(move || {
                                let result = openviking_memory_delivery::deliver_create_once(
                                    &owner, &turn, &content,
                                );
                                let _ = tx.send(PersistCommand::RecordOpenVikingWriteOutcome {
                                    conversation_id: owner,
                                    turn_id: turn,
                                    completed_sequence,
                                    result,
                                });
                            });
                        if let Err(error) = spawned {
                            openviking_write_active = false;
                            if let Ok(mut outbox) =
                                openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)
                            {
                                let _ = outbox.transition_durable(
                                    &checkpoint_outbox_path,
                                    &conversation_id,
                                    &turn_id,
                                    completed_sequence,
                                    openviking_checkpoint_outbox::State::Dispatched,
                                    openviking_checkpoint_outbox::State::Uncertain,
                                );
                            }
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "OpenViking memory write spawn",
                                revision: None,
                                request_id: None,
                                turn_id: None,
                                error: error.to_string(),
                            });
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking memory write claim",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordOpenVikingWriteOutcome {
                conversation_id,
                turn_id,
                completed_sequence,
                result,
            } => {
                openviking_write_active = false;
                let confirmed = result.is_ok();
                let next = if confirmed {
                    openviking_checkpoint_outbox::State::Confirmed
                } else {
                    openviking_checkpoint_outbox::State::Uncertain
                };
                let persisted = openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)
                    .and_then(|mut outbox| {
                        outbox.transition_durable(
                            &checkpoint_outbox_path,
                            &conversation_id,
                            &turn_id,
                            completed_sequence,
                            openviking_checkpoint_outbox::State::Dispatched,
                            next,
                        )
                    });
                match persisted {
                    Ok(_) => {
                        let _ = notices.send(PersistNotice::OpenVikingMemoryWriteFinished {
                            conversation_id,
                            confirmed,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking memory write state persistence",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
                if std::env::var("CHATARIUM_OPENVIKING_AUTOCAPTURE").as_deref() != Ok("0") {
                    let _ = completion_tx.send(PersistCommand::DispatchOpenVikingReadyWrite);
                }
            }
            PersistCommand::SaveOpenVikingMemorySummary {
                conversation_id,
                turn_id,
                completed_sequence,
                summary,
            } => {
                let result = (|| -> Result<bool, String> {
                    let mut outbox =
                        openviking_checkpoint_outbox::Outbox::load(&checkpoint_outbox_path)?;
                    let skipped = summary.trim() == "NO_MEMORY";
                    outbox.save_summary_durable(
                        &checkpoint_outbox_path,
                        &conversation_id.to_string(),
                        &turn_id.to_string(),
                        completed_sequence,
                        &summary,
                    )?;
                    Ok(skipped)
                })();
                match result {
                    Ok(skipped) => {
                        let _ = notices.send(PersistNotice::OpenVikingMemorySummarySaved {
                            conversation_id,
                            turn_id,
                            skipped,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "OpenViking summary persistence",
                            revision: None,
                            request_id: None,
                            turn_id: Some(turn_id),
                            error,
                        });
                    }
                }
            }
            PersistCommand::QueueMemoryCheckpoint {
                conversation_id,
                turn_id,
            } => {
                let source = store.events().iter().rev().find(|event| {
                    event.kind == EventKind::AssistantCompletionObserved
                        && event.scope == Some(local_turn_scope(turn_id))
                });
                let owner_verified =
                    checkpoint_owner_for_turn(store.events(), turn_id) == Some(conversation_id);
                let result = (|| -> Result<Option<usize>, String> {
                    if !owner_verified {
                        return Err(
                            "checkpoint turn is not uniquely owned by the requesting conversation"
                                .to_owned(),
                        );
                    }
                    let event =
                        source.ok_or("checkpoint requested before durable model completion")?;
                    if !checkpoint_completion_eligible(event) {
                        return Ok(None);
                    }
                    let path = &checkpoint_outbox_path;
                    let mut outbox = openviking_checkpoint_outbox::Outbox::load(path)?;
                    if outbox.started_after_sequence().is_none() {
                        return Err(
                            "checkpoint outbox has no durable activation boundary".to_owned()
                        );
                    }
                    outbox.enqueue_durable(
                        path,
                        &conversation_id.to_string(),
                        &turn_id.to_string(),
                        event.sequence,
                    )?;
                    Ok(Some(outbox.pending_count()))
                })();
                match result {
                    Ok(Some(pending_count)) => {
                        let _ = notices.send(PersistNotice::MemoryCheckpointQueued {
                            conversation_id,
                            pending_count,
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "automatic memory checkpoint outbox",
                            revision: None,
                            request_id: None,
                            turn_id: Some(turn_id),
                            error,
                        });
                    }
                }
            }
            PersistCommand::SaveMcpAgentDefaults { defaults } => {
                if let Err(error) =
                    defaults.save_atomic(&data_dir.join(mcp_agent_defaults::FILE_NAME))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "MCP agent defaults save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveHostShellSettings { settings } => {
                if let Err(error) = settings.save_atomic(&data_dir.join(host_shell_mcp::FILE_NAME))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "host shell settings save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveInferenceSettings { store: settings } => {
                if let Err(error) =
                    settings.save_atomic(&data_dir.join("local-inference-settings.json"))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "inference settings save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveBehaviorProfiles { store: profiles } => {
                if let Err(error) = profiles.save_atomic(&data_dir.join("behavior-profiles.json")) {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "behavior profile save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::InitializeLocalOrchestrationTopology {
                conversation_id,
                container_id,
                root_session_id,
            } => {
                match append_local_orchestration_topology_checked(
                    &mut store,
                    conversation_id,
                    container_id,
                    root_session_id,
                ) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::OrchestrationTopologyInitialized {
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "orchestration topology initialization",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindCurrentSessionRouteEndpoint {
                conversation_id,
                session_id,
                endpoint_id,
            } => {
                match append_current_session_route_endpoint_checked(
                    &mut store,
                    conversation_id,
                    session_id,
                    endpoint_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::RouteEndpointBound { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "route addressability binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ProposeLocalSessionRoute {
                route_id,
                source_conversation_id,
                destination_conversation_id,
            } => {
                match append_local_session_route_proposal_checked(
                    &mut store,
                    route_id,
                    source_conversation_id,
                    destination_conversation_id,
                ) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::LocalRoutePolicyEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route proposal",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AttachLocalSessionRoutePayload {
                payload_id,
                route_id,
                text,
            } => match append_local_route_payload_checked(&mut store, payload_id, route_id, text) {
                Ok(event) => {
                    let _ =
                        notices.send(PersistNotice::LocalRoutePayloadAttached { route_id, event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "local route payload attachment",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            },
            PersistCommand::DecideLocalSessionRoute { route_id, decision } => {
                match append_local_route_user_decision_checked(&mut store, route_id, decision) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::LocalRoutePolicyEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchLocalSessionRoute { route_id } => {
                match append_local_route_dispatch_and_delivery_checked(&mut store, route_id) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::LocalRouteDispatchUpdated {
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route dispatch/delivery",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideLocalRouteContext {
                route_id,
                destination_conversation_id,
                decision,
            } => {
                match append_local_route_context_decision_checked(
                    &mut store,
                    route_id,
                    destination_conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LocalRouteContextDecisionUpdated {
                            route_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local route context decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RegisterToolProvider { provider_id, name } => {
                match append_tool_provider_registration_checked(&mut store, provider_id, name) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::ToolProviderUpdated { provider_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool provider registration",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindToolProviderEndpoint {
                provider_id,
                endpoint_id,
            } => {
                match append_tool_provider_endpoint_checked(&mut store, provider_id, endpoint_id) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::ToolProviderUpdated { provider_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool provider endpoint binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ConfigureToolProviderTransport {
                provider_id,
                config,
            } => match append_tool_transport_config_checked(&mut store, provider_id, &config) {
                Ok(event) => {
                    let _ = notices.send(PersistNotice::ToolProviderUpdated { provider_id, event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "tool transport configuration",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            },
            PersistCommand::DecideToolProviderActivation {
                provider_id,
                configured_sequence,
                decision,
            } => {
                match local_tool_provider_control::append_activation_with_inspection(
                    &mut store,
                    provider_id,
                    configured_sequence,
                    decision,
                ) {
                    Ok(event) => {
                        let _ =
                            notices.send(PersistNotice::ToolProviderUpdated { provider_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool provider activation",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ProposeAgentMcpCall {
                turn_id,
                source_conversation_id,
                proposal,
                policy,
            } => {
                // A standing authorization comes ONLY from persisted user
                // settings; do not trust a transient UI field or model text.
                let checked = if policy == mcp_agent_defaults::ToolPolicy::Automatic {
                    let saved = mcp_agent_defaults::AgentDefaults::load(
                        &data_dir.join(mcp_agent_defaults::FILE_NAME),
                    );
                    match saved {
                        Ok(config)
                            if mcp_agent_defaults::tool_policy(
                                &config.policy_rules,
                                &proposal.provider,
                                &proposal.tool,
                            ) == Ok(mcp_agent_defaults::ToolPolicy::Automatic) =>
                        {
                            append_agent_mcp_call_checked(
                                &mut store,
                                source_conversation_id,
                                &proposal,
                                policy,
                            )
                        }
                        _ => {
                            Err("no persisted user authorization for automatic MCP tool".to_owned())
                        }
                    }
                } else {
                    append_agent_mcp_call_checked(
                        &mut store,
                        source_conversation_id,
                        &proposal,
                        policy,
                    )
                };
                match checked {
                    Ok((call_id, route_id, appended_events)) => {
                        let _ = notices.send(PersistNotice::AgentMcpCallProposed {
                            turn_id,
                            call_id,
                            route_id,
                            policy,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "agent MCP call proposal",
                            revision: None,
                            request_id: None,
                            turn_id: Some(turn_id),
                            error,
                        });
                    }
                }
            }
            PersistCommand::ProposeToolCall {
                call_id,
                route_id,
                source_conversation_id,
                provider_id,
                operation,
                arguments_text,
            } => {
                match append_tool_call_proposal_checked(
                    &mut store,
                    call_id,
                    route_id,
                    source_conversation_id,
                    provider_id,
                    operation,
                    arguments_text,
                    RoutePolicy::RequireApproval,
                ) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::ToolCallProposed {
                            call_id,
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool call proposal",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideToolCallRoute { route_id, decision } => {
                match append_tool_call_route_user_decision_checked(&mut store, route_id, decision) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::ToolCallRoutePolicyUpdated { route_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool route decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::CheckSandboxHostReadiness => {
                let notices_from_probe = notices.clone();
                let launched = thread::Builder::new()
                    .name("chatarium-mcp-host-check".to_owned())
                    .spawn(move || {
                        let result =
                            std::panic::catch_unwind(local_stdio_runner::probe_confined_stdio_host)
                                .unwrap_or_else(|_| {
                                    Err("Linux sandbox host probe panicked".to_owned())
                                });
                        let _ =
                            notices_from_probe.send(PersistNotice::SandboxHostChecked { result });
                    });
                if let Err(error) = launched {
                    let _ = notices.send(PersistNotice::SandboxHostChecked {
                        result: Err(format!("cannot start sandbox host probe: {error}")),
                    });
                }
            }
            PersistCommand::ExecuteApprovedStdioTool { call_id } => {
                // Do the read-only call and executable checks first. The real
                // namespace probe runs off the journal worker, *before* the
                // irreversible RouteDispatched event is recorded.
                let prepared = preview_activated_stdio_tool_invocation(store.events(), call_id)
                    .and_then(|preview| {
                        local_stdio_runner::preflight_confined_stdio_launch(
                            preview.invocation.executable.as_str(),
                            &preview.invocation.argv,
                        )
                    });
                match prepared {
                    Ok(()) => {
                        let completion_tx = completion_tx.clone();
                        let failure_notices = notices.clone();
                        let launched = thread::Builder::new()
                            .name(format!("chatarium-mcp-probe-{}", call_id.get()))
                            .spawn(move || {
                                let result = std::panic::catch_unwind(
                                    local_stdio_runner::probe_confined_stdio_host,
                                )
                                .unwrap_or_else(|_| {
                                    Err("Linux sandbox host-readiness probe panicked; no dispatch was consumed".to_owned())
                                });
                                match result {
                                    Ok(()) => {
                                        let _ = completion_tx.send(
                                            PersistCommand::ReserveApprovedStdioToolAfterProbe {
                                                call_id,
                                            },
                                        );
                                    }
                                    Err(error) => {
                                        let _ = failure_notices.send(PersistNotice::Failed {
                                            operation: "tool isolated stdio host preflight",
                                            revision: None,
                                            request_id: None,
                                            turn_id: None,
                                            error,
                                        });
                                    }
                                }
                            });
                        if let Err(error) = launched {
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "tool isolated stdio host preflight",
                                revision: None,
                                request_id: None,
                                turn_id: None,
                                error: format!("could not start host-readiness probe: {error}; no dispatch was consumed"),
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool isolated stdio preflight",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ReserveApprovedStdioToolAfterProbe { call_id } => {
                // Probe success is never a cached permit: revalidate all live
                // policy before recording the one-shot dispatch.
                let prepared = preview_activated_stdio_tool_invocation(store.events(), call_id)
                    .and_then(|preview| {
                        local_stdio_runner::preflight_confined_stdio_launch(
                            preview.invocation.executable.as_str(),
                            &preview.invocation.argv,
                        )
                    })
                    .and_then(|()| reserve_activated_stdio_tool_dispatch(&mut store, call_id));
                match prepared {
                    Ok(reservation) => {
                        if let Some(event) = store.events().last().cloned() {
                            let _ = notices
                                .send(PersistNotice::StdioToolDispatchReserved { call_id, event });
                        }
                        let completion_tx = completion_tx.clone();
                        let failure_notices = notices.clone();
                        let launched = thread::Builder::new()
                            .name(format!("chatarium-mcp-{}", call_id.get()))
                            .spawn(move || {
                                let observed = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| {
                                        local_stdio_runner::run_reserved_stdio_tool_call(reservation)
                                    }),
                                );
                                match observed {
                                    Ok(observation) => {
                                        // Journal completion stays serialized with other writes.
                                        let _ = completion_tx.send(
                                            PersistCommand::RecordExternalStdioOutcome {
                                                observation,
                                            },
                                        );
                                    }
                                    Err(_) => {
                                        // A panic can leave process effects uncertain.
                                        let _ = failure_notices.send(PersistNotice::Failed {
                                            operation: "tool isolated stdio runner panicked (unresolved)",
                                            revision: None,
                                            request_id: None,
                                            turn_id: None,
                                            error: "sandbox worker panicked; dispatch remains unresolved; no automatic retry".to_owned(),
                                        });
                                    }
                                }
                            });
                        if let Err(error) = launched {
                            // Spawn failure after reservation is unresolved, not retryable.
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "tool isolated stdio worker launch (unresolved)",
                                revision: None,
                                request_id: None,
                                turn_id: None,
                                error: format!(
                                    "sandbox worker launch failed: {error}; no automatic retry"
                                ),
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool isolated stdio preflight",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordExternalStdioOutcome { observation } => {
                let call_id = observation.call_id;
                match append_tool_call_outcome_checked(
                    &mut store,
                    observation.call_id,
                    observation.route_id,
                    observation.kind,
                    observation.text,
                ) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::StdioToolDispatchFinished { call_id, event });
                    }
                    Err(error) => {
                        // Unpersisted observations leave their routes unresolved.
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool isolated stdio outcome append (unresolved)",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchLocalHttpMcp { route_id } => {
                match reserve_local_http_mcp_checked(&mut store, route_id) {
                    Ok((call_id, conversation, provider, operation, args, event)) => {
                        let _ = notices
                            .send(PersistNotice::StdioToolDispatchReserved { call_id, event });
                        let completions = completion_tx.clone();
                        let failures = notices.clone();
                        let launched = thread::Builder::new()
                            .name(format!("chatarium-local-mcp-{}", call_id.get()))
                            .spawn(move || {
                                let id = call_id.get().to_string();
                                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    local_http_mcp::invoke(&conversation, &id, &provider, &operation, &args)
                                }));
                                match result {
                                    Ok(result) => {
                                        let _ = completions.send(PersistCommand::RecordLocalHttpMcpOutcome {
                                            call_id, route_id, operation, result,
                                        });
                                    }
                                    Err(_) => {
                                        let _ = failures.send(PersistNotice::Failed {
                                            operation: "localhost MCP worker panic (unresolved)",
                                            revision: None, request_id: None, turn_id: None,
                                            error: "local MCP worker panicked after durable dispatch; no retry".to_owned(),
                                        });
                                    }
                                }
                            });
                        if let Err(error) = launched {
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "localhost MCP worker spawn (unresolved)",
                                revision: None,
                                request_id: None,
                                turn_id: None,
                                error: format!(
                                    "reserved localhost MCP call could not start: {error}"
                                ),
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "localhost MCP preflight",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordLocalHttpMcpOutcome {
                call_id,
                route_id,
                operation,
                result,
            } => {
                let (kind, text) = match result {
                    Ok(output) => {
                        match chatarium_protocol::tool_envelope::format_legacy_tool_success(
                            &format!("call_{}", call_id.get()),
                            &operation,
                            output,
                        ) {
                            Ok(value) => (ToolCallOutcomeKind::Result, value),
                            Err(error) => (
                                ToolCallOutcomeKind::Error,
                                format!("localhost MCP result formatting rejected: {error:?}"),
                            ),
                        }
                    }
                    Err(error) => (ToolCallOutcomeKind::Error, error),
                };
                match append_tool_call_outcome_checked(&mut store, call_id, route_id, kind, text) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::StdioToolDispatchFinished { call_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "localhost MCP outcome journal append (unresolved)",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchHostShell { route_id } => {
                let prepared =
                    host_shell_mcp::Settings::load(&data_dir.join(host_shell_mcp::FILE_NAME))
                        .and_then(|settings| {
                            let (call_id, args, event) = reserve_builtin_host_shell_checked(
                                &mut store, route_id, &settings,
                            )?;
                            Ok((call_id, args, event, settings))
                        });
                match prepared {
                    Ok((call_id, args, event, settings)) => {
                        let _ =
                            notices.send(PersistNotice::HostShellDispatched { route_id, event });
                        let completions = completion_tx.clone();
                        let failures = notices.clone();
                        let spawn = thread::Builder::new()
                            .name(format!("chatarium-host-shell-{}", call_id.get()))
                            .spawn(move || {
                                let result = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| {
                                        host_shell_mcp::execute(&settings, &args)
                                    }),
                                );
                                match result {
                                    Ok(result) => {
                                        let _ = completions.send(
                                            PersistCommand::RecordHostShellOutcome {
                                                call_id,
                                                route_id,
                                                result,
                                            },
                                        );
                                    }
                                    Err(_) => {
                                        let _ = failures.send(PersistNotice::Failed {
                                            operation: "host shell panicked (unresolved)",
                                            revision: None,
                                            request_id: None,
                                            turn_id: None,
                                            error: "shell worker panicked after durable dispatch; no retry".to_owned(),
                                        });
                                    }
                                }
                            });
                        if let Err(error) = spawn {
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "host shell worker spawn (unresolved)",
                                revision: None,
                                request_id: None,
                                turn_id: None,
                                error: format!(
                                    "host shell was reserved but could not spawn: {error}"
                                ),
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "host shell preflight",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordHostShellOutcome {
                call_id,
                route_id,
                result,
            } => {
                let (kind, text) = match result {
                    Ok(output) => {
                        let formatted =
                            chatarium_protocol::tool_envelope::format_legacy_tool_success(
                                &format!("call_{}", call_id.get()),
                                host_shell_mcp::OPERATION,
                                output,
                            );
                        match formatted {
                            Ok(formatted) => (ToolCallOutcomeKind::Result, formatted),
                            Err(error) => (
                                ToolCallOutcomeKind::Error,
                                format!("host shell result formatting failed: {error:?}"),
                            ),
                        }
                    }
                    Err(error) => (ToolCallOutcomeKind::Error, error),
                };
                match append_tool_call_outcome_checked(&mut store, call_id, route_id, kind, text) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::StdioToolDispatchFinished { call_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "host shell outcome journal append (unresolved)",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchBuiltinHello { route_id } => {
                match append_builtin_hello_checked(&mut store, route_id) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::BuiltinHelloDispatched {
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool builtin dispatch",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideToolResultContext {
                call_id,
                conversation_id,
                decision,
            } => {
                match append_tool_result_context_decision_checked(
                    &mut store,
                    call_id,
                    conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::ToolResultContextDecisionUpdated {
                            call_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "tool result context decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordLocalMemory {
                memory_id,
                source_conversation_id,
                text,
            } => {
                match append_local_memory_artifact_checked(
                    &mut store,
                    memory_id,
                    source_conversation_id,
                    text,
                ) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::LocalMemoryArtifactRecorded { memory_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local memory artifact",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideLocalMemoryContext {
                memory_id,
                destination_conversation_id,
                decision,
            } => {
                match append_local_memory_context_decision_checked(
                    &mut store,
                    memory_id,
                    destination_conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LocalMemoryContextDecisionUpdated {
                            memory_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local memory context decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::SupersedeLocalMemory {
                predecessor_memory_id,
                successor_memory_id,
            } => {
                match append_local_memory_supersession_checked(
                    &mut store,
                    predecessor_memory_id,
                    successor_memory_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LocalMemorySupersessionRecorded {
                            predecessor_memory_id,
                            successor_memory_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local memory supersession",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AddLocalMemoryLabel { memory_id, label } => {
                match append_local_memory_label_change_checked(&mut store, memory_id, label, true) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::LocalMemoryLabelUpdated { memory_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local memory label add",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RemoveLocalMemoryLabel { memory_id, label } => {
                match append_local_memory_label_change_checked(&mut store, memory_id, label, false)
                {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::LocalMemoryLabelUpdated { memory_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "local memory label remove",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindLocalConversationWorker {
                conversation_id,
                worker_id,
            } => {
                match append_local_worker_binding_checked(&mut store, conversation_id, worker_id) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "lifecycle worker binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindCurrentSessionWorker {
                conversation_id,
                worker_id,
            } => {
                match append_current_session_worker_binding_checked(
                    &mut store,
                    conversation_id,
                    worker_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::SupervisionEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "supervision worker-session binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DesignateCurrentSessionController { conversation_id } => {
                match append_current_session_controller_designation_checked(
                    &mut store,
                    conversation_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::SupervisionEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "supervision controller designation",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::BindControllerToLocalWorker {
                controller_conversation_id,
                worker_conversation_id,
            } => {
                match append_local_controller_worker_binding_checked(
                    &mut store,
                    controller_conversation_id,
                    worker_conversation_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::SupervisionEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "supervision controller-worker binding",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::CreateControllerContinuationLease {
                controller_conversation_id,
                worker_conversation_id,
                allowance,
            } => {
                match append_controller_continuation_lease_checked(
                    &mut store,
                    controller_conversation_id,
                    worker_conversation_id,
                    allowance,
                ) {
                    Ok((worker_id, event)) => {
                        let _ = notices.send(PersistNotice::ControllerContinuationLeaseCreated {
                            worker_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller control continuation lease",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ProposeControllerWorkerControl {
                controller_conversation_id,
                worker_conversation_id,
                action,
            } => {
                match append_controller_worker_control_proposal_checked(
                    &mut store,
                    controller_conversation_id,
                    worker_conversation_id,
                    action,
                ) {
                    Ok((control_id, route_id, appended_events)) => {
                        let _ = notices.send(PersistNotice::ControllerControlProposed {
                            control_id,
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller control proposal",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideControllerWorkerControlRoute { route_id, decision } => {
                match append_controller_control_route_decision_checked(
                    &mut store, route_id, decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::ControllerControlRoutePolicyUpdated {
                            route_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller control route decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DispatchControllerWorkerControlRoute { route_id } => {
                match append_controller_control_dispatch_checked(&mut store, route_id) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::ControllerControlDispatched {
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller control dispatch",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideControllerWorkerResultContext {
                route_id,
                controller_conversation_id,
                decision,
            } => {
                match append_controller_worker_result_context_decision_checked(
                    &mut store,
                    route_id,
                    controller_conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(
                            PersistNotice::ControllerWorkerResultContextDecisionUpdated {
                                route_id,
                                event,
                            },
                        );
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller result context decision",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::StartControllerCoordination {
                controller_conversation_id,
            } => {
                match append_controller_coordination_start_checked(
                    &mut store,
                    controller_conversation_id,
                ) {
                    Ok((turn_id, event)) => {
                        let _ = notices
                            .send(PersistNotice::ControllerCoordinationStarted { turn_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller coordination start",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::DecideControllerCoordinationResultContext {
                coordination_turn_id,
                controller_conversation_id,
                decision,
            } => {
                match append_controller_coordination_result_context_decision_checked(
                    &mut store,
                    coordination_turn_id,
                    controller_conversation_id,
                    decision,
                ) {
                    Ok(event) => {
                        let _ = notices.send(
                            PersistNotice::ControllerCoordinationResultContextDecisionUpdated {
                                turn_id: coordination_turn_id,
                                event,
                            },
                        );
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller coordination result context decision",
                            revision: None,
                            request_id: None,
                            turn_id: Some(coordination_turn_id),
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordControllerCoordinationSuggestion {
                controller_conversation_id,
                coordination_turn_id,
                basis_result_route_id,
                action,
            } => {
                match append_controller_coordination_suggestion_checked(
                    &mut store,
                    controller_conversation_id,
                    coordination_turn_id,
                    basis_result_route_id,
                    action,
                ) {
                    Ok((suggestion_id, event)) => {
                        let _ =
                            notices.send(PersistNotice::ControllerCoordinationSuggestionRecorded {
                                suggestion_id,
                                event,
                            });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller coordination suggestion",
                            revision: None,
                            request_id: None,
                            turn_id: Some(coordination_turn_id),
                            error,
                        });
                    }
                }
            }
            PersistCommand::PromoteControllerCoordinationSuggestion {
                controller_conversation_id,
                suggestion_id,
            } => {
                match append_controller_coordination_suggestion_promotion_checked(
                    &mut store,
                    controller_conversation_id,
                    suggestion_id,
                ) {
                    Ok((control_id, route_id, appended_events)) => {
                        let _ =
                            notices.send(PersistNotice::ControllerCoordinationSuggestionPromoted {
                                suggestion_id,
                                control_id,
                                route_id,
                                appended_events,
                            });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "controller coordination suggestion promotion",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AcknowledgeWorkerControl {
                worker_conversation_id,
                route_id,
            } => {
                match append_worker_control_acknowledgement_checked(
                    &mut store,
                    worker_conversation_id,
                    route_id,
                ) {
                    Ok(event) => {
                        let _ = notices
                            .send(PersistNotice::WorkerControlAcknowledged { route_id, event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "worker control acknowledgement",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::RecordWorkerControlStatusResult {
                worker_conversation_id,
                route_id,
            } => {
                match append_worker_control_status_result_checked(
                    &mut store,
                    worker_conversation_id,
                    route_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::WorkerControlStatusResultRecorded {
                            route_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "worker control status result",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::ApplyWorkerControl {
                worker_conversation_id,
                route_id,
            } => {
                match append_worker_control_action_checked(
                    &mut store,
                    worker_conversation_id,
                    route_id,
                ) {
                    Ok(appended_events) => {
                        let _ = notices.send(PersistNotice::WorkerControlActionAdvanced {
                            route_id,
                            appended_events,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "worker control action",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::StartWorkerContinuationExecution {
                worker_conversation_id,
                route_id,
            } => {
                match append_worker_continuation_execution_start_checked(
                    &mut store,
                    worker_conversation_id,
                    route_id,
                ) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::WorkerContinuationExecutionStarted {
                            route_id,
                            event,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "worker continuation execution start",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::AssignWorkerGoal { worker_id, goal_id } => {
                match append_worker_goal_checked(&mut store, worker_id, goal_id) {
                    Ok(event) => {
                        let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "lifecycle goal assignment",
                            revision: None,
                            request_id: None,
                            turn_id: None,
                            error,
                        });
                    }
                }
            }
            PersistCommand::TransitionWorker {
                worker_id,
                goal_id,
                action,
            } => match append_worker_transition_checked(&mut store, worker_id, goal_id, action) {
                Ok(event) => {
                    let _ = notices.send(PersistNotice::LifecycleEventAppended { event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "lifecycle transition",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            },
            PersistCommand::SaveLocalConversationCatalog { catalog } => {
                if let Err(error) = catalog.save_atomic(&data_dir.join("local-conversations.json"))
                {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "local conversation catalog save",
                        revision: None,
                        request_id: None,
                        turn_id: None,
                        error,
                    });
                }
            }
            PersistCommand::SaveDraft {
                conversation_id,
                revision,
                text,
            } => {
                match store.append_scoped(
                    Some(local_conversation_scope(conversation_id)),
                    EventKind::DraftChanged,
                    text,
                ) {
                    Ok(_) => {
                        if let Some(event) = store.events().last().cloned() {
                            let _ = notices.send(PersistNotice::DraftSaved {
                                conversation_id,
                                revision,
                                event,
                            });
                        }
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::Failed {
                            operation: "draft save",
                            revision: Some(revision),
                            request_id: None,
                            turn_id: None,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::CommitMessage {
                request_id,
                message,
                attachment,
                one_shot_memory_ids,
                one_shot_memory_snapshot_after_sequence,
            } => match commit_message_with_attachment_and_one_shot_memory_checked(
                &mut store,
                &message,
                attachment.as_ref(),
                &one_shot_memory_ids,
                one_shot_memory_snapshot_after_sequence,
            ) {
                Ok((event, one_shot_memory_event)) => {
                    let _ = notices.send(PersistNotice::MessageCommitted {
                        request_id,
                        message,
                        event,
                        one_shot_memory_event,
                    });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "message commit",
                        revision: None,
                        request_id: Some(request_id),
                        turn_id: Some(message.turn_id),
                        error,
                    });
                }
            },
            PersistCommand::AppendAgentActivity {
                conversation_id,
                turn_id,
                frame,
            } => match append_siwc_agent_activity_checked(
                &mut store,
                conversation_id,
                turn_id,
                &frame,
            ) {
                Ok(event) => {
                    let _ = notices.send(PersistNotice::AgentActivityAppended { event });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "agent activity journal",
                        revision: None,
                        request_id: None,
                        turn_id: Some(turn_id),
                        error,
                    });
                }
            },
            PersistCommand::AppendTurnEvent {
                turn_id,
                kind,
                payload,
            } => match store.append_scoped(Some(local_turn_scope(turn_id)), kind, payload) {
                Ok(_) => {
                    let event = store.events().last().cloned();
                    let terminal_turn_event = matches!(
                        kind,
                        EventKind::AssistantCompletionObserved
                            | EventKind::RemoteFailureObserved
                            | EventKind::TransportInterrupted
                    );
                    let continuation_result = if terminal_turn_event {
                        append_worker_continuation_result_if_terminal(&mut store, turn_id)
                    } else {
                        Ok(None)
                    };
                    let coordination_result = if terminal_turn_event {
                        append_controller_coordination_result_if_terminal(&mut store, turn_id)
                    } else {
                        Ok(None)
                    };

                    if let Some(event) = event {
                        let _ = notices.send(PersistNotice::TurnEventAppended {
                            turn_id,
                            kind,
                            event,
                        });
                    }
                    match continuation_result {
                        Ok(Some((route_id, event))) => {
                            let _ = notices.send(
                                PersistNotice::WorkerContinuationExecutionResultRecorded {
                                    route_id,
                                    event,
                                },
                            );
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "worker continuation execution result",
                                revision: None,
                                request_id: None,
                                turn_id: Some(turn_id),
                                error,
                            });
                        }
                    }
                    match coordination_result {
                        Ok(Some(event)) => {
                            let _ =
                                notices.send(PersistNotice::ControllerCoordinationResultRecorded {
                                    turn_id,
                                    event,
                                });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = notices.send(PersistNotice::Failed {
                                operation: "controller coordination result",
                                revision: None,
                                request_id: None,
                                turn_id: Some(turn_id),
                                error,
                            });
                        }
                    }
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::Failed {
                        operation: "turn evidence append",
                        revision: None,
                        request_id: None,
                        turn_id: Some(turn_id),
                        error: error.to_string(),
                    });
                }
            },
            PersistCommand::LoadHistoricalConversation {
                local_conversation_id,
            } => {
                let result = latest_historical_conversation_catalog(store.events()).and_then(
                    |catalog| {
                        let entry = catalog
                            .into_iter()
                            .find(|entry| entry.local_conversation_id == local_conversation_id)
                            .ok_or_else(|| {
                                format!(
                                    "historical conversation {local_conversation_id} is not present in the durable catalog"
                                )
                            })?;
                        let imported_sequence = entry.imported_sequence;
                        let messages = load_historical_active_transcript(&data_dir, &entry)?;
                        Ok((imported_sequence, messages))
                    },
                );

                match result {
                    Ok((imported_sequence, messages)) => {
                        let _ = notices.send(PersistNotice::HistoricalConversationLoaded {
                            local_conversation_id,
                            imported_sequence,
                            messages,
                        });
                    }
                    Err(error) => {
                        let _ = notices.send(PersistNotice::HistoricalConversationLoadFailed {
                            local_conversation_id,
                            error,
                        });
                    }
                }
            }
            PersistCommand::LoadLiveConversation {
                local_conversation_id,
            } => match latest_live_transcript(store.events(), local_conversation_id, None) {
                Ok((snapshot_sequence, projection)) => {
                    let _ = notices.send(PersistNotice::LiveConversationLoaded {
                        local_conversation_id,
                        snapshot_sequence,
                        truncated_before: projection.truncated_before,
                        messages: projection.messages,
                    });
                }
                Err(error) => {
                    let _ = notices.send(PersistNotice::LiveConversationLoadFailed {
                        local_conversation_id,
                        error,
                    });
                }
            },
            PersistCommand::PromoteHistoricalLiveMirror {
                local_conversation_id,
                expected_remote_conversation_id,
                body,
            } => {
                diagnostics::info(
                    "persist",
                    format!(
                        "validating imported mirror remote={} local={local_conversation_id}",
                        diagnostics::short_id(&expected_remote_conversation_id)
                    ),
                );
                let before = store.events().len();
                match promote_historical_live_mirror_body(
                    &mut store,
                    local_conversation_id,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
                        diagnostics::info(
                            "persist",
                            format!(
                                "imported mirror durable: local={local_conversation_id} event=#{} appended-events={}",
                                result.snapshot.sequence,
                                appended_events.len()
                            ),
                        );
                        let projection = latest_live_transcript(
                            store.events(),
                            local_conversation_id,
                            Some(result.snapshot.sequence),
                        );
                        let (truncated_before, messages, projection_error) = match projection {
                            Ok((_, projection)) => {
                                (projection.truncated_before, projection.messages, None)
                            }
                            Err(error) => (false, Vec::new(), Some(error)),
                        };
                        let _ = notices.send(PersistNotice::HistoricalLiveMirrorPromoted {
                            local_conversation_id,
                            snapshot_sequence: result.snapshot.sequence,
                            appended_events,
                            truncated_before,
                            messages,
                            projection_error,
                        });
                    }
                    Err(error) => {
                        diagnostics::error(
                            "persist",
                            format!(
                                "imported mirror validation/persist failed local={local_conversation_id}: {error}"
                            ),
                        );
                        let _ = notices.send(PersistNotice::HistoricalLiveMirrorPromotionFailed {
                            local_conversation_id,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::PromoteDiscoveredLiveMirror {
                expected_remote_conversation_id,
                body,
            } => {
                diagnostics::info(
                    "persist",
                    format!(
                        "validating discovered mirror remote={}",
                        diagnostics::short_id(&expected_remote_conversation_id)
                    ),
                );
                let before = store.events().len();
                match promote_discovered_live_mirror_body(
                    &mut store,
                    expected_remote_conversation_id.as_str(),
                    &body,
                ) {
                    Ok(result) => {
                        let appended_events = store.events()[before..].to_vec();
                        diagnostics::info(
                            "persist",
                            format!(
                                "discovered mirror durable: remote={} local={} event=#{} appended-events={}",
                                diagnostics::short_id(result.remote_conversation_id.as_str()),
                                result.local_conversation_id,
                                result.snapshot.sequence,
                                appended_events.len()
                            ),
                        );
                        let projection = latest_live_transcript(
                            store.events(),
                            result.local_conversation_id,
                            Some(result.snapshot.sequence),
                        );
                        let (truncated_before, messages, projection_error) = match projection {
                            Ok((_, projection)) => {
                                (projection.truncated_before, projection.messages, None)
                            }
                            Err(error) => (false, Vec::new(), Some(error)),
                        };
                        let _ = notices.send(PersistNotice::DiscoveredLiveMirrorPromoted {
                            local_conversation_id: result.local_conversation_id,
                            remote_conversation_id: result
                                .remote_conversation_id
                                .as_str()
                                .to_owned(),
                            snapshot_sequence: result.snapshot.sequence,
                            appended_events,
                            truncated_before,
                            messages,
                            projection_error,
                        });
                    }
                    Err(error) => {
                        diagnostics::error(
                            "persist",
                            format!(
                                "discovered mirror validation/persist failed remote={}: {error}",
                                diagnostics::short_id(&expected_remote_conversation_id)
                            ),
                        );
                        let _ = notices.send(PersistNotice::DiscoveredLiveMirrorPromotionFailed {
                            remote_conversation_id: expected_remote_conversation_id,
                            error: error.to_string(),
                        });
                    }
                }
            }
            PersistCommand::MirrorQueuePrepare {
                remote_conversation_id,
                catalog_index,
                reply,
            } => {
                let result = record_remote_mirror_queue_item_queued(
                    &mut store,
                    &remote_conversation_id,
                    catalog_index,
                )
                .and_then(|_| {
                    record_remote_mirror_queue_capture_started(&mut store, &remote_conversation_id)
                })
                .map(|_| ())
                .map_err(|error| error.to_string());
                if result.is_ok() {
                    let before = store.events().len().saturating_sub(2);
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated {
                        appended_events: store.events()[before..].to_vec(),
                    });
                }
                let _ = reply.send(result);
            }
            PersistCommand::MirrorQueueCapture {
                remote_conversation_id,
                body,
                reply,
            } => {
                let before = store.events().len();
                let result = match promote_discovered_live_mirror_body(
                    &mut store,
                    &remote_conversation_id,
                    &body,
                ) {
                    Ok(promotion) => match latest_live_transcript(
                        store.events(),
                        promotion.local_conversation_id,
                        Some(promotion.snapshot.sequence),
                    ) {
                        Ok((_, projection)) => {
                            let mirror_state = if projection.truncated_before {
                                "partial"
                            } else {
                                "fully_mirrored"
                            };
                            match record_remote_mirror_queue_completed(
                                &mut store,
                                &remote_conversation_id,
                                mirror_state,
                            ) {
                                Ok(_) => Ok(MirrorPersisted {
                                    snapshot_sequence: promotion.snapshot.sequence,
                                    mirror_state,
                                }),
                                Err(error) => Err(error.to_string()),
                            }
                        }
                        Err(error) => {
                            let _ = record_remote_mirror_queue_failed(
                                &mut store,
                                &remote_conversation_id,
                                "structural",
                            );
                            Err(error)
                        }
                    },
                    Err(error) => {
                        let _ = record_remote_mirror_queue_failed(
                            &mut store,
                            &remote_conversation_id,
                            "structural",
                        );
                        Err(error.to_string())
                    }
                };
                let appended_events = store.events()[before..].to_vec();
                if !appended_events.is_empty() {
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated { appended_events });
                }
                let _ = reply.send(result);
            }
            PersistCommand::MirrorQueueFailure {
                remote_conversation_id,
                failure_class,
                reply,
            } => {
                let before = store.events().len();
                let result = match failure_class {
                    MirrorFailureClass::Transient => record_remote_mirror_queue_failed(
                        &mut store,
                        &remote_conversation_id,
                        "transient",
                    ),
                    MirrorFailureClass::RateLimited => {
                        record_remote_mirror_queue_rate_limited(&mut store, &remote_conversation_id)
                    }
                    MirrorFailureClass::Structural => record_remote_mirror_queue_failed(
                        &mut store,
                        &remote_conversation_id,
                        "structural",
                    ),
                }
                .map(|_| ())
                .map_err(|error| error.to_string());
                if result.is_ok() {
                    let _ = notices.send(PersistNotice::MirrorQueueUpdated {
                        appended_events: store.events()[before..].to_vec(),
                    });
                }
                let _ = reply.send(result);
            }
            PersistCommand::RecordRemoteHealthSignal {
                signal,
                now_ms,
                reply,
            } => {
                let mut controller =
                    match RemoteHealthController::from_events(store.events(), now_ms) {
                        Ok(controller) => controller,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                let before = store.events().len();
                let result =
                    record_remote_health_signal(&mut store, &mut controller, signal, now_ms)
                        .map_err(|error| error.to_string());
                if result.is_ok() {
                    if let Some(event) = store.events()[before..].last().cloned() {
                        let _ = notices.send(PersistNotice::RemoteHealthUpdated {
                            event,
                            controller: controller.clone(),
                        });
                    }
                }
                let _ = reply.send(result.map(|_| controller));
            }
            PersistCommand::RecordRemoteHealthIntent {
                intent,
                now_ms,
                reply,
            } => {
                let mut controller =
                    match RemoteHealthController::from_events(store.events(), now_ms) {
                        Ok(controller) => controller,
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    };
                let before = store.events().len();
                let result =
                    record_remote_health_intent(&mut store, &mut controller, intent, now_ms)
                        .map_err(|error| error.to_string());
                if result.is_ok() {
                    if let Some(event) = store.events()[before..].last().cloned() {
                        let _ = notices.send(PersistNotice::RemoteHealthUpdated {
                            event,
                            controller: controller.clone(),
                        });
                    }
                }
                let _ = reply.send(result.map(|_| controller));
            }
            PersistCommand::Shutdown => {
                diagnostics::info("persist", "shutdown requested");
                break;
            }
        }
    }
}
