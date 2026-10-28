//! Behavior-preserving extraction of init methods from the desktop application.
use super::super::*;

impl ChatariumApp {
    /// Background startup phase: acquire journal ownership and validate the
    /// complete raw archive without blocking the GUI render/event thread.
    pub(crate) fn load_journal_for_bootstrap(
        journal_path: &std::path::Path,
    ) -> Result<crate::startup::LoadedJournal, String> {
        diagnostics::info(
            "startup",
            format!("opening durable journal at {}", journal_path.display()),
        );
        let raw = JsonlEventStore::open(journal_path)
            .map_err(|error| format!("failed to open journal: {error}"))?;
        let mut store = chatarium_store::effective_event_store::EffectiveEventStore::new(raw)
            .map_err(|error| format!("journal source quarantine validation failed: {error}"))?;
        // Recovery can replay many turns and append durable interruptions.
        // Do both the scan and the initial full snapshot off the GUI thread.
        let recovery = recover_interrupted_remote_turns(&mut store);
        let events = store.events().to_vec();
        let local_path = local_conversation_catalog_path(journal_path);
        let (catalog, conversation_id, status) =
            match load_local_conversation_workspace(&local_path, &events) {
                Ok((catalog, id)) => (catalog, id, "journal ready".to_owned()),
                Err(error) => {
                    let fallback = projected_local_conversation_id(&events)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let mut catalog = local_conversations::LocalConversationCatalog::default();
                    catalog.create(fallback, unix_now_ms());
                    (
                        catalog,
                        fallback,
                        format!("journal ready; local conversation catalog warning: {error}"),
                    )
                }
            };
        let draft = projected_working_draft(&events, conversation_id);
        // Build the full searchable native corpus on the bootstrap worker,
        // never in the first egui frame.
        let native_index = std::sync::Arc::new(
            native_conversation_search::NativeConversationSearchIndex::build(&events),
        );
        let remote_health = RemoteHealthController::from_events(&events, unix_now_ms())
            .unwrap_or_default();
        Ok(crate::startup::LoadedJournal {
            store,
            events,
            native_index,
            remote_health,
            recovery,
            workspace: crate::startup::LoadedWorkspace {
                catalog,
                conversation_id,
                status,
                draft,
            },
        })
    }

    /// UI-thread phase: consume the loaded, validated journal and construct
    /// the desktop. Never reopen the journal after the background handoff.
    pub(crate) fn from_loaded_journal(
        repaint: &egui::Context,
        journal_path: PathBuf,
        loaded: Result<crate::startup::LoadedJournal, String>,
    ) -> Self {
        let data_dir = journal_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        match loaded {
            Ok(crate::startup::LoadedJournal {
                store,
                events,
                native_index,
                remote_health,
                recovery,
                workspace,
            }) => {
                let excluded_count = store.excluded_count();
                if excluded_count > 0 {
                    diagnostics::info(
                        "startup",
                        format!(
                            "effective event replay excludes {excluded_count} source records; original journal preserved"
                        ),
                    );
                }
                diagnostics::info(
                    "startup",
                    format!("journal replay complete: {} events", events.len()),
                );
                let crate::startup::LoadedWorkspace {
                    catalog: local_conversation_catalog,
                    conversation_id: local_conversation_id,
                    status: mut startup_status,
                    draft,
                } = workspace;
                match recovery {
                    Ok(0) => {}
                    Ok(count) => {
                        startup_status = format!(
                            "{startup_status}; recovered {count} interrupted remote turn{}",
                            if count == 1 { "" } else { "s" }
                        );
                    }
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; remote-turn recovery warning: {error}");
                    }
                }
                let historical_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match latest_historical_conversation_catalog(&events) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status = format!(
                                "{startup_status}; historical archive replay warning: {error}"
                            );
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                let live_mirror_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match latest_live_mirror_catalog(&events) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; live mirror replay warning: {error}");
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                // Preserve cached account history on disk but never read or
                // index it in the native-only workspace.
                let remote_conversation_catalog = if ACCOUNT_MIRRORING_ENABLED {
                    match load_remote_history_cache(&remote_history_cache_path(&journal_path)) {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; remote history cache warning: {error}");
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                let remote_catalog_view = match build_remote_catalog_view(
                    &remote_conversation_catalog,
                    &events,
                    &live_mirror_catalog,
                ) {
                    Ok(view) => view,
                    Err(error) => {
                        startup_status =
                            format!("{startup_status}; local mirror view warning: {error}");
                        Vec::new()
                    }
                };
                diagnostics::info(
                    "startup",
                    format!(
                        "catalogs ready: historical={} live-mirrors={} discovered-history-cache={}",
                        historical_catalog.len(),
                        live_mirror_catalog.len(),
                        remote_conversation_catalog.len()
                    ),
                );
                let live_mirrored_conversations = live_mirror_catalog
                    .iter()
                    .map(|entry| entry.local_conversation_id)
                    .collect();
                let (account_bridge, account_bridge_provider, account_bridge_status) =
                    start_account_bridge();
                let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
                let (persist_tx, persist_rx) = mpsc::channel();
                let (notice_tx, notice_rx) = mpsc::channel();
                let local_archive_search_index = if ACCOUNT_MIRRORING_ENABLED {
                    build_local_archive_search_index(&remote_catalog_view, &events)
                } else {
                    local_archive_search::LocalArchiveSearchIndex::new(Vec::new())
                };
                let reader_state_path = local_reader_state_path(&journal_path);
                let reader_positions =
                    offline_reader::ReaderPositionStore::load(&reader_state_path);
                let inference_settings_path = local_inference_settings_path(&journal_path);
                let inference_settings =
                    match local_inference_settings::InferenceSettingsStore::load(
                        &inference_settings_path,
                    ) {
                        Ok(settings) => settings,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; inference settings warning: {error}");
                            local_inference_settings::InferenceSettingsStore::default()
                        }
                    };
                let mcp_agent_defaults = mcp_agent_defaults::AgentDefaults::load(
                    &data_dir.join(mcp_agent_defaults::FILE_NAME),
                )
                .unwrap_or_default();
                let host_shell_settings =
                    host_shell_mcp::Settings::load(&data_dir.join(host_shell_mcp::FILE_NAME))
                        .unwrap_or_default();
                let active_inference_settings = inference_settings
                    .for_conversation_with_mcp_defaults(local_conversation_id, &mcp_agent_defaults);
                let coordination_workspace = coordination_workspace::Workspace::load(
                    &data_dir.join(coordination_workspace::FILE_NAME),
                )
                .unwrap_or_default();
                let project_memory_grants =
                    match openviking_memory_delivery::ProjectMemoryGrants::load(
                        &data_dir.join(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME),
                    ) {
                        Ok(grants) => grants,
                        Err(error) => {
                            startup_status = format!(
                                "{startup_status}; project memory permission warning: {error}"
                            );
                            openviking_memory_delivery::ProjectMemoryGrants::default()
                        }
                    };
                let behavior_profile_path = local_behavior_profile_path(&journal_path);
                let behavior_profiles =
                    match behavior_profile::BehaviorProfileStore::load(&behavior_profile_path) {
                        Ok(profiles) => profiles,
                        Err(error) => {
                            startup_status =
                                format!("{startup_status}; behavior profile warning: {error}");
                            behavior_profile::BehaviorProfileStore::default()
                        }
                    };
                let active_behavior_profile =
                    behavior_profiles.for_conversation(local_conversation_id);
                let worker_data_dir = data_dir.clone();
                let completion_tx = persist_tx.clone();
                let worker = thread::Builder::new()
                    .name("chatarium-persistence".to_owned())
                    .spawn(move || {
                        persistence_worker(
                            store,
                            worker_data_dir,
                            persist_rx,
                            completion_tx,
                            notice_tx,
                        )
                    });

                match worker {
                    Ok(worker) => {
                        let app = Self {
                            draft,
                            attachment_path_draft: String::new(),
                            staged_attachment: None,
                            draft_revision: 0,
                            saved_revision: 0,
                            next_commit_request: 1,
                            commit_in_flight: None,
                            evidence: TurnEvidence::default(),
                            local_conversation_id,
                            local_conversation_rename: local_conversation_catalog
                                .entry(local_conversation_id)
                                .and_then(|entry| entry.title.clone())
                                .unwrap_or_default(),
                            local_conversation_project_directory: local_conversation_catalog
                                .entry(local_conversation_id)
                                .and_then(|entry| entry.project_directory.clone())
                                .unwrap_or_default(),
                            local_conversation_catalog,
                            project_memory_grants,
                            coordination_workspace,
                            coordination_pending_admissions: BTreeMap::new(),
                            coordination_ui: coordination_workspace::UiDrafts::default(),
                            coordination_window_open: false,
                            coordination_tool_budget: coordination_tool_budget::Budget::default(),
                            coordination_last_queue_check: Instant::now(),
                            coordination_auto_wake_targets: HashSet::new(),
                            coordination_auto_wake_count: 0,
                            coordination_pending_wake: None,
                            show_archived_local_conversations: false,
                            local_conversation_search_query: String::new(),
                            local_conversation_search_selection: None,
                            events,
                            historical_catalog,
                            selected_historical_conversation: None,
                            loaded_historical_conversation: None,
                            historical_messages: Vec::new(),
                            historical_load_pending: None,
                            live_mirrored_conversations,
                            live_mirror_catalog,
                            live_mirror_pending: None,
                            remote_discovery_pending: None,
                            remote_mirror_failures: BTreeMap::new(),
                            remote_mirror_retry_after: BTreeMap::new(),
                            mirror_status: "idle · no mirror in progress".to_owned(),
                            live_mirror_truncated_before: false,
                            remote_conversation_catalog,
                            remote_catalog_view,
                            local_archive_search_index,
                            archive_search_query: String::new(),
                            archive_search_mode:
                                local_archive_search::ArchiveSearchMode::AllLocalData,
                            archive_state_filter: local_archive_search::ArchiveStateFilter::All,
                            archive_search_selection: None,
                            reader_state_path,
                            reader_positions,
                            reader_restore_pending: true,
                            reader_last_saved_offset: 0.0,
                            reader_last_position_write: Instant::now(),
                            reader_search_query: String::new(),
                            reader_search_hit: None,
                            inference_settings,
                            mcp_agent_defaults,
                            host_shell_settings,
                            behavior_profiles,
                            conversation_behavior_profile: active_behavior_profile,
                            topology_command_pending: false,
                            route_addressability_command_pending: false,
                            route_policy_command_pending: false,
                            route_payload_command_pending: false,
                            route_payload_drafts: BTreeMap::new(),
                            route_dispatch_command_pending: false,
                            route_context_command_pending: false,
                            tool_command_pending: false,
                            last_sandbox_host_check: None,
                            tool_provider_name_draft: String::new(),
                            tool_selected_provider: None,
                            tool_stdio_executable_draft: String::new(),
                            tool_stdio_argv_draft: String::new(),
                            tool_stdio_operations_draft: String::new(),
                            tool_operation_draft: String::new(),
                            tool_arguments_draft: String::new(),
                            local_memory_command_pending: false,
                            local_memory_draft: String::new(),
                            staged_local_memory_origin: None,
                            local_memory_search_query: String::new(),
                            local_memory_label_filter: None,
                            local_memory_source_filter: None,
                            show_superseded_local_memory: false,
                            local_memory_label_drafts: BTreeMap::new(),
                            local_memory_one_shot_selections: BTreeMap::new(),
                            lifecycle_command_pending: false,
                            supervision_command_pending: false,
                            controller_control_command_pending: false,
                            controller_result_context_command_pending: false,
                            controller_coordination_command_pending: false,
                            controller_coordination_result_context_command_pending: false,
                            controller_coordination_suggestion_command_pending: false,
                            continuation_allowance_drafts: BTreeMap::new(),
                            worker_control_command_pending: false,
                            conversation_instructions: active_inference_settings.instructions,
                            conversation_developer_context: active_inference_settings
                                .developer_context,
                            conversation_mcp_enabled: active_inference_settings.mcp_enabled,
                            archive_backup_path: String::new(),
                            archive_maintenance_status: "archive maintenance idle".to_owned(),
                            archive_restore_confirmation_pending: false,
                            selected_remote_catalog_id: None,
                            remote_conversation_total: None,
                            history_discovery_started: false,
                            history_list_pending: false,
                            history_bridge_proven: false,
                            pending_history_fetch_proof: None,
                            journal_path: journal_path.clone(),
                            persist_tx: Some(persist_tx),
                            notice_rx: Some(notice_rx),
                            worker: Some(worker),
                            status: startup_status,
                            account_bridge,
                            account_bridge_provider,
                            account_bridge_status,
                            live_mirror_fetch_tx,
                            live_mirror_fetch_rx,
                            mirror_controller_tx: None,
                            mirror_controller_notice_rx: None,
                            mirror_controller_worker: None,
                            mirror_controller_state: MirrorControllerState::Stopped,
                            remote_health,
                            mirror_controller_current_item: None,
                            mirror_controller_detail: None,
                            remote: siwc_bridge::BridgeRuntime::start(repaint),
                            remote_session: siwc_bridge::SessionState::default(),
                            remote_models: Vec::new(),
                            capability_probe: load_capability_probe_state(&journal_path),
                            local_inference_contract: load_local_inference_contract_state(
                                &journal_path,
                            ),
                            pending_web_search_enable: None,
                            model_list_pending: false,
                            slow_warning_seconds: active_inference_settings.slow_warning_seconds,
                            selected_model: active_inference_settings.model,
                            remote_status: "starting sign-in runtime…".to_owned(),
                            network_health: network_health::NetworkHealth::default(),
                            remote_runtime_ready: false,
                            remote_runtime_failed: false,
                            sign_in_requested: false,
                            sign_in_pending: false,
                            pending_remote_turn: None,
                            active_remote_turn: None,
                            agent_mcp_flow: None,
                            openviking_checkpoint_status:
                                "Awaiting an eligible completed assistant turn".to_owned(),
                            openviking_summary_in_flight: None,
                            openviking_summary_last_attempt: None,
                            openviking_write_last_attempt: None,
                            openviking_summary_blocked: HashSet::new(),
                            openviking_retrieval_cache: BTreeMap::new(),
                            openviking_retrieval_in_flight: HashSet::new(),
                            openviking_retrieval_retry_after: BTreeMap::new(),
                            openviking_retrieval_stale: HashSet::new(),
                            openviking_query_tracker:
                                openviking_project_cache::QueryRefreshTracker::default(),
                            openviking_project_cache:
                                openviking_project_cache::RetrievalCache::default(),
                            commit_remote_intents: BTreeMap::new(),
                            delivery: delivery_telemetry::Timeline::default(),
                        };
                        // The index is built against the worker's exact
                        // effective snapshot. Prime the UI-owned clone before
                        // its first render to avoid a full first-frame scan.
                        if let Err(error) = native_conversation_search::prime_prebuilt_index(
                            repaint,
                            &app.events,
                            native_index,
                        ) {
                            diagnostics::error(
                                "startup",
                                format!("background search index handoff mismatch: {error}"),
                            );
                        }
                        app
                    }
                    Err(error) => Self::without_persistence(
                        repaint,
                        journal_path,
                        draft,
                        events,
                        format!("failed to start persistence worker: {error}"),
                    ),
                }
            }
            Err(error) => Self::without_persistence(
                repaint,
                journal_path,
                String::new(),
                Vec::new(),
                format!("journal startup unavailable: {error}"),
            ),
        }
    }

    pub(crate) fn without_persistence(
        repaint: &egui::Context,
        journal_path: PathBuf,
        draft: String,
        events: Vec<EventEnvelope>,
        status: String,
    ) -> Self {
        let live_mirror_catalog = if ACCOUNT_MIRRORING_ENABLED {
            latest_live_mirror_catalog(&events).unwrap_or_default()
        } else {
            Vec::new()
        };
        let live_mirrored_conversations = live_mirror_catalog
            .iter()
            .map(|entry| entry.local_conversation_id)
            .collect();
        let (account_bridge, account_bridge_provider, account_bridge_status) =
            start_account_bridge();
        let (live_mirror_fetch_tx, live_mirror_fetch_rx) = mpsc::channel();
        let remote_conversation_catalog = if ACCOUNT_MIRRORING_ENABLED {
            load_remote_history_cache(&remote_history_cache_path(&journal_path)).unwrap_or_default()
        } else {
            Vec::new()
        };
        let remote_catalog_view =
            build_remote_catalog_view(&remote_conversation_catalog, &events, &live_mirror_catalog)
                .unwrap_or_default();
        let local_archive_search_index = if ACCOUNT_MIRRORING_ENABLED {
            build_local_archive_search_index(&remote_catalog_view, &events)
        } else {
            local_archive_search::LocalArchiveSearchIndex::new(Vec::new())
        };
        let reader_state_path = local_reader_state_path(&journal_path);
        let reader_positions = offline_reader::ReaderPositionStore::load(&reader_state_path);
        let local_conversation_catalog_path = local_conversation_catalog_path(&journal_path);
        let (local_conversation_catalog, local_conversation_id) =
            load_local_conversation_workspace(&local_conversation_catalog_path, &events)
                .unwrap_or_else(|_| {
                    let fallback = projected_local_conversation_id(&events)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let mut catalog = local_conversations::LocalConversationCatalog::default();
                    catalog.create(fallback, unix_now_ms());
                    (catalog, fallback)
                });
        let draft = projected_working_draft(&events, local_conversation_id);
        let inference_settings_path = local_inference_settings_path(&journal_path);
        let inference_settings =
            local_inference_settings::InferenceSettingsStore::load(&inference_settings_path)
                .unwrap_or_default();
        let mcp_agent_defaults = mcp_agent_defaults::AgentDefaults::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(mcp_agent_defaults::FILE_NAME),
        )
        .unwrap_or_default();
        let coordination_workspace = coordination_workspace::Workspace::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(coordination_workspace::FILE_NAME),
        )
        .unwrap_or_default();
        let project_memory_grants = openviking_memory_delivery::ProjectMemoryGrants::load(
            &journal_path
                .with_file_name(openviking_memory_delivery::PROJECT_MEMORY_GRANTS_FILE_NAME),
        )
        .unwrap_or_default();
        let host_shell_settings = host_shell_mcp::Settings::load(
            &journal_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(host_shell_mcp::FILE_NAME),
        )
        .unwrap_or_default();
        let active_inference_settings = inference_settings
            .for_conversation_with_mcp_defaults(local_conversation_id, &mcp_agent_defaults);
        let behavior_profiles = behavior_profile::BehaviorProfileStore::load(
            &local_behavior_profile_path(&journal_path),
        )
        .unwrap_or_default();
        let active_behavior_profile = behavior_profiles.for_conversation(local_conversation_id);

        Self {
            draft,
            attachment_path_draft: String::new(),
            staged_attachment: None,
            draft_revision: 0,
            saved_revision: 0,
            next_commit_request: 1,
            commit_in_flight: None,
            evidence: TurnEvidence::default(),
            local_conversation_id,
            local_conversation_rename: local_conversation_catalog
                .entry(local_conversation_id)
                .and_then(|entry| entry.title.clone())
                .unwrap_or_default(),
            local_conversation_project_directory: local_conversation_catalog
                .entry(local_conversation_id)
                .and_then(|entry| entry.project_directory.clone())
                .unwrap_or_default(),
            local_conversation_catalog,
            project_memory_grants,
            coordination_workspace,
            coordination_pending_admissions: BTreeMap::new(),
            coordination_ui: coordination_workspace::UiDrafts::default(),
            coordination_window_open: false,
            coordination_tool_budget: coordination_tool_budget::Budget::default(),
            coordination_last_queue_check: Instant::now(),
            coordination_auto_wake_targets: HashSet::new(),
            coordination_auto_wake_count: 0,
            coordination_pending_wake: None,
            show_archived_local_conversations: false,
            local_conversation_search_query: String::new(),
            local_conversation_search_selection: None,
            historical_catalog: if ACCOUNT_MIRRORING_ENABLED {
                latest_historical_conversation_catalog(&events).unwrap_or_default()
            } else {
                Vec::new()
            },
            events: events.clone(),
            selected_historical_conversation: None,
            loaded_historical_conversation: None,
            historical_messages: Vec::new(),
            historical_load_pending: None,
            live_mirrored_conversations,
            live_mirror_catalog,
            live_mirror_pending: None,
            remote_discovery_pending: None,
            remote_mirror_failures: BTreeMap::new(),
            remote_mirror_retry_after: BTreeMap::new(),
            mirror_status: "idle · no mirror in progress".to_owned(),
            live_mirror_truncated_before: false,
            remote_conversation_catalog,
            remote_catalog_view,
            local_archive_search_index,
            archive_search_query: String::new(),
            archive_search_mode: local_archive_search::ArchiveSearchMode::AllLocalData,
            archive_state_filter: local_archive_search::ArchiveStateFilter::All,
            archive_search_selection: None,
            reader_state_path,
            reader_positions,
            reader_restore_pending: true,
            reader_last_saved_offset: 0.0,
            reader_last_position_write: Instant::now(),
            reader_search_query: String::new(),
            reader_search_hit: None,
            inference_settings,
            mcp_agent_defaults,
            host_shell_settings,
            behavior_profiles,
            conversation_behavior_profile: active_behavior_profile,
            topology_command_pending: false,
            route_addressability_command_pending: false,
            route_policy_command_pending: false,
            route_payload_command_pending: false,
            route_payload_drafts: BTreeMap::new(),
            route_dispatch_command_pending: false,
            route_context_command_pending: false,
            tool_command_pending: false,
            last_sandbox_host_check: None,
            tool_provider_name_draft: String::new(),
            tool_selected_provider: None,
            tool_stdio_executable_draft: String::new(),
            tool_stdio_argv_draft: String::new(),
            tool_stdio_operations_draft: String::new(),
            tool_operation_draft: String::new(),
            tool_arguments_draft: String::new(),
            local_memory_command_pending: false,
            local_memory_draft: String::new(),
            staged_local_memory_origin: None,
            local_memory_search_query: String::new(),
            local_memory_label_filter: None,
            local_memory_source_filter: None,
            show_superseded_local_memory: false,
            local_memory_label_drafts: BTreeMap::new(),
            local_memory_one_shot_selections: BTreeMap::new(),
            lifecycle_command_pending: false,
            supervision_command_pending: false,
            controller_control_command_pending: false,
            controller_result_context_command_pending: false,
            controller_coordination_command_pending: false,
            controller_coordination_result_context_command_pending: false,
            controller_coordination_suggestion_command_pending: false,
            continuation_allowance_drafts: BTreeMap::new(),
            worker_control_command_pending: false,
            conversation_instructions: active_inference_settings.instructions,
            conversation_developer_context: active_inference_settings.developer_context,
            conversation_mcp_enabled: active_inference_settings.mcp_enabled,
            archive_backup_path: String::new(),
            archive_maintenance_status: "archive maintenance idle".to_owned(),
            archive_restore_confirmation_pending: false,
            selected_remote_catalog_id: None,
            remote_conversation_total: None,
            history_discovery_started: false,
            history_list_pending: false,
            history_bridge_proven: false,
            pending_history_fetch_proof: None,
            journal_path: journal_path.clone(),
            persist_tx: None,
            notice_rx: None,
            worker: None,
            status,
            account_bridge,
            account_bridge_provider,
            account_bridge_status,
            live_mirror_fetch_tx,
            live_mirror_fetch_rx,
            mirror_controller_tx: None,
            mirror_controller_notice_rx: None,
            mirror_controller_worker: None,
            mirror_controller_state: MirrorControllerState::Stopped,
            remote_health: RemoteHealthController::from_events(&events, unix_now_ms())
                .unwrap_or_default(),
            mirror_controller_current_item: None,
            mirror_controller_detail: None,
            remote: siwc_bridge::BridgeRuntime::start(repaint),
            remote_session: siwc_bridge::SessionState::default(),
            remote_models: Vec::new(),
            capability_probe: load_capability_probe_state(&journal_path),
            local_inference_contract: load_local_inference_contract_state(&journal_path),
            pending_web_search_enable: None,
            model_list_pending: false,
            slow_warning_seconds: active_inference_settings.slow_warning_seconds,
            selected_model: active_inference_settings.model,
            remote_status: "starting sign-in runtime…".to_owned(),
            network_health: network_health::NetworkHealth::default(),
            remote_runtime_ready: false,
            remote_runtime_failed: false,
            sign_in_requested: false,
            sign_in_pending: false,
            pending_remote_turn: None,
            active_remote_turn: None,
            agent_mcp_flow: None,
            openviking_checkpoint_status: "Awaiting an eligible completed assistant turn"
                .to_owned(),
            openviking_summary_in_flight: None,
            openviking_summary_last_attempt: None,
            openviking_write_last_attempt: None,
            openviking_summary_blocked: HashSet::new(),
            openviking_retrieval_cache: BTreeMap::new(),
            openviking_retrieval_in_flight: HashSet::new(),
            openviking_retrieval_retry_after: BTreeMap::new(),
            openviking_retrieval_stale: HashSet::new(),
            openviking_query_tracker: openviking_project_cache::QueryRefreshTracker::default(),
            openviking_project_cache: openviking_project_cache::RetrievalCache::default(),
            commit_remote_intents: BTreeMap::new(),
            delivery: delivery_telemetry::Timeline::default(),
        }
    }
}
