//! Legacy Edge process lifecycle implementation (mechanically moved).
use super::*;

impl LaunchedEdge {
    /// Launch Edge with the dedicated profile and localhost-only ephemeral debugging port.
    pub fn launch(run: &mut CaptureRun) -> Result<Self, TransportError> {
        let config = EdgeLaunchConfig::discover()?;
        Self::launch_with_options(
            config,
            run,
            &SystemEdgeSpawner,
            Box::new(LoopbackDevToolsHttp),
            Box::new(LoopbackWebSocketConnector),
            false,
        )
    }

    /// Launch an incognito `about:blank` browser for the read-only transport smoke.
    ///
    /// The harness-owned profile remains the user-data root, while the private window avoids
    /// loading that profile's persisted site session during a diagnostic run.
    pub fn launch_read_only_smoke(run: &mut CaptureRun) -> Result<Self, TransportError> {
        #[cfg(windows)]
        {
            return Self::launch_pipe_profile(run, "about:blank", true);
        }
        #[cfg(not(windows))]
        {
            let config = EdgeLaunchConfig::discover()?;
            Self::launch_with_options(
                config,
                run,
                &SystemEdgeSpawner,
                Box::new(LoopbackDevToolsHttp),
                Box::new(LoopbackWebSocketConnector),
                true,
            )
        }
    }

    /// Launch the persistent dedicated profile at ChatGPT using the Windows pipe transport.
    pub fn launch_profile_init(run: &mut CaptureRun) -> Result<Self, TransportError> {
        #[cfg(windows)]
        {
            Self::launch_pipe_profile(run, crate::init::CHATGPT_START_URL, false)
        }
        #[cfg(not(windows))]
        {
            let _ = run;
            Err(TransportError::Process(
                "profile bootstrap requires the Windows anonymous-pipe Edge transport".to_owned(),
            ))
        }
    }

    #[cfg(windows)]
    pub(super) fn launch_pipe_profile(
        run: &mut CaptureRun,
        start_url: &str,
        incognito: bool,
    ) -> Result<Self, TransportError> {
        use crate::pipe::spawn_edge_with_pipe;

        #[cfg(test)]
        let diagnostics: &dyn EdgeDiagnostics = &TestEdgeDiagnostics;
        #[cfg(not(test))]
        let diagnostics: &dyn EdgeDiagnostics = &SystemEdgeDiagnostics;

        let config = EdgeLaunchConfig::discover()?;
        validate_dedicated_capture_profile(&config.profile, &config.local_app_data)
            .map_err(TransportError::UnsafeProfile)?;
        ensure_profile_directories(&config)?;
        let lock_path = config.capture_root.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone())?;
        let policy = diagnostics.remote_debugging_policy();
        if let Err(error) = run.append_event(
            "devtools_os_diagnostics",
            json!({
                "stage": "policy_preflight",
                "transport_mode": "pipe",
                "port": null,
                "remote_debugging_disabled": policy.is_disabled(),
                "remote_debugging_allowed": {
                    "summary": policy.summary(),
                    "machine": policy.machine,
                    "user": policy.user,
                },
                "listener_snapshot": null,
                "connect_results": [],
            }),
        ) {
            return Err(record_prelaunch_cleanup(
                run,
                lock_path,
                lock,
                TransportError::Journal(error),
            ));
        }
        if policy.is_disabled() {
            return Err(record_prelaunch_cleanup(
                run,
                lock_path,
                lock,
                TransportError::RemoteDebuggingDisabled(format!(
                    "HKLM={}, HKCU={}",
                    policy.machine, policy.user
                )),
            ));
        }

        let arguments = pipe_profile_arguments(&config.profile, start_url, incognito);
        let startup_deadline = Instant::now() + config.startup_timeout;
        let (child, transport) = match spawn_edge_with_pipe(&config.executable, &arguments, run) {
            Ok(pair) => pair,
            Err(error) => {
                return Err(record_prelaunch_cleanup(run, lock_path, lock, error));
            }
        };
        let pid = child.id();
        let process = EdgeProcess::new(child, lock);
        let mut launched = Self {
            process,
            lock_file: lock_path,
            active_port_file: config.profile.join(ACTIVE_PORT_FILE),
            transport: Some(EdgeBrowserTransport::Pipe(transport)),
            active_port_required: false,
        };
        if let Err(error) = run.append_event(
            "browser_process_started",
            json!({"pid":pid, "transport_mode":"pipe", "debugging_endpoint":"anonymous_pipes"}),
        ) {
            return Err(fail_pipe_startup(
                &mut launched,
                run,
                TransportError::DiagnosticJournalFailure {
                    primary_failure: None,
                    journal_failure: error,
                },
            ));
        }
        let startup_result = (|| {
            run.append_event(
                "devtools_pipe_readiness_started",
                json!({"transport_mode":"pipe"}),
            )
            .map_err(TransportError::Journal)?;
            let mut attempts = 0_u32;
            let mut last_error = None;
            let version = loop {
                let remaining = startup_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let error = TransportError::ReadinessTimeout {
                        attempts,
                        last_error: last_error.unwrap_or_else(|| {
                            "pipe setup exceeded browser startup deadline".to_owned()
                        }),
                    };
                    return Err(journal_pipe_readiness_failure(run, attempts, &error));
                }
                attempts = attempts.saturating_add(1);
                let result = {
                    let transport = match launched.transport.as_mut().expect("pipe transport set") {
                        EdgeBrowserTransport::Pipe(transport) => transport,
                        EdgeBrowserTransport::Tcp(_) => unreachable!(),
                    };
                    transport.set_command_timeout(remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT));
                    transport.browser_version(run)
                };
                match result {
                    Ok(version) => break version,
                    Err(error) if pipe_readiness_retryable(&error) => {
                        if let Some(code) = launched
                            .process
                            .try_wait()
                            .map_err(TransportError::Process)?
                        {
                            return Err(TransportError::Process(format!(
                                "Edge exited during pipe readiness with status {code}; last CDP error: {error}"
                            )));
                        }
                        last_error = Some(error.to_string());
                        thread::sleep(
                            DEVTOOLS_RETRY_DELAY
                                .min(startup_deadline.saturating_duration_since(Instant::now())),
                        );
                    }
                    Err(error) => return Err(error),
                }
            };
            run.append_event(
                "devtools_pipe_first_cdp_response",
                json!({"method":"Browser.getVersion", "transport_mode":"pipe"}),
            )
            .map_err(TransportError::Journal)?;
            if let Some(code) = launched
                .process
                .try_wait()
                .map_err(TransportError::Process)?
            {
                return Err(TransportError::Process(format!(
                    "Edge exited during pipe readiness with status {code}"
                )));
            }
            let remaining = startup_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::ReadinessTimeout {
                    attempts: 1,
                    last_error: "browser startup deadline expired before Target.getTargets"
                        .to_owned(),
                });
            }
            let transport = match launched.transport.as_mut().expect("pipe transport set") {
                EdgeBrowserTransport::Pipe(transport) => transport,
                EdgeBrowserTransport::Tcp(_) => unreachable!(),
            };
            transport.set_command_timeout(remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT));
            let mut target_attempts = 0_u32;
            loop {
                let remaining = startup_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let error = TransportError::ReadinessTimeout {
                        attempts: attempts.saturating_add(target_attempts),
                        last_error: last_error.clone().unwrap_or_else(|| {
                            "browser startup deadline expired before Target.getTargets completed"
                                .to_owned()
                        }),
                    };
                    return Err(journal_pipe_readiness_failure(
                        run,
                        attempts.saturating_add(target_attempts),
                        &error,
                    ));
                }
                target_attempts = target_attempts.saturating_add(1);
                let result = {
                    let transport = match launched.transport.as_mut().expect("pipe transport set") {
                        EdgeBrowserTransport::Pipe(transport) => transport,
                        EdgeBrowserTransport::Tcp(_) => unreachable!(),
                    };
                    transport.set_command_timeout(remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT));
                    transport.list_targets(run)
                };
                match result {
                    Ok(_) => break,
                    Err(error) if pipe_readiness_retryable(&error) => {
                        if let Some(code) = launched
                            .process
                            .try_wait()
                            .map_err(TransportError::Process)?
                        {
                            return Err(TransportError::Process(format!(
                                "Edge exited during pipe readiness with status {code}; last CDP error: {error}"
                            )));
                        }
                        last_error = Some(error.to_string());
                        thread::sleep(
                            DEVTOOLS_RETRY_DELAY
                                .min(startup_deadline.saturating_duration_since(Instant::now())),
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(code) = launched
                .process
                .try_wait()
                .map_err(TransportError::Process)?
            {
                return Err(TransportError::Process(format!(
                    "Edge exited during pipe readiness with status {code}"
                )));
            }
            run.append_event("devtools_pipe_readiness_succeeded", json!({"transport_mode":"pipe", "browser":version.browser, "protocol_version":version.protocol_version})).map_err(TransportError::Journal)?;
            Ok(())
        })();
        if let Err(error) = startup_result {
            return Err(fail_pipe_startup(&mut launched, run, error));
        }
        Ok(launched)
    }

    #[cfg(test)]
    pub(super) fn launch_with(
        config: EdgeLaunchConfig,
        run: &mut CaptureRun,
        spawner: &dyn EdgeProcessSpawner,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
    ) -> Result<Self, TransportError> {
        Self::launch_with_options(config, run, spawner, http, websocket, false)
    }

    pub(super) fn launch_with_options(
        config: EdgeLaunchConfig,
        run: &mut CaptureRun,
        spawner: &dyn EdgeProcessSpawner,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
        incognito: bool,
    ) -> Result<Self, TransportError> {
        #[cfg(test)]
        let diagnostics: &dyn EdgeDiagnostics = &TestEdgeDiagnostics;
        #[cfg(not(test))]
        let diagnostics: &dyn EdgeDiagnostics = &SystemEdgeDiagnostics;
        Self::launch_with_diagnostics(
            config,
            run,
            spawner,
            http,
            websocket,
            incognito,
            diagnostics,
            &DurableDiagnosticEventWriter,
        )
    }

    pub(super) fn launch_with_diagnostics(
        config: EdgeLaunchConfig,
        run: &mut CaptureRun,
        spawner: &dyn EdgeProcessSpawner,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
        incognito: bool,
        diagnostics: &dyn EdgeDiagnostics,
        diagnostic_writer: &dyn DiagnosticEventWriter,
    ) -> Result<Self, TransportError> {
        validate_dedicated_capture_profile(&config.profile, &config.local_app_data)
            .map_err(TransportError::UnsafeProfile)?;
        ensure_profile_directories(&config)?;
        let lock_path = config.capture_root.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone())?;
        let active_port_file = config.profile.join(ACTIVE_PORT_FILE);
        ensure_active_port_absent(&active_port_file)?;

        let mut arguments = vec![
            format!("--user-data-dir={}", config.profile.display()),
            "--remote-debugging-address=127.0.0.1".to_owned(),
            "--remote-debugging-port=0".to_owned(),
            "--no-first-run".to_owned(),
            "--no-default-browser-check".to_owned(),
        ];
        if incognito {
            arguments.push("--incognito".to_owned());
        }
        arguments.push("about:blank".to_owned());
        let startup_deadline = Instant::now() + config.startup_timeout;
        let child = spawner
            .spawn(&config.executable, &arguments)
            .map_err(TransportError::Process)?;
        let pid = child.id();
        if let Err(error) = run.append_event(
            "browser_process_started",
            json!({
                "pid": pid,
                "requested_debugging_address": "127.0.0.1",
                "debugging_port": "ephemeral",
            }),
        ) {
            let mut process = EdgeProcess::new(child, lock);
            let _ = process.shutdown();
            return Err(TransportError::Journal(error));
        }
        let process = EdgeProcess::new(child, lock);
        let mut launched = Self {
            process,
            lock_file: lock_path,
            active_port_file: active_port_file.clone(),
            transport: None,
            active_port_required: true,
        };

        let mut startup_evidence = None;
        let startup = (|| {
            let policy = diagnostics.remote_debugging_policy();
            let policy_preflight = diagnostic_writer.append(
                run,
                "devtools_os_diagnostics",
                json!({
                    "stage": "policy_preflight",
                    "port": null,
                    "launched_edge_pid": pid,
                    "remote_debugging_disabled": policy.is_disabled(),
                    "remote_debugging_allowed": {
                        "summary": policy.summary(),
                        "machine": policy.machine,
                        "user": policy.user,
                    },
                    "listener_snapshot": null,
                    "connect_results": [],
                }),
            );
            if let Err(journal_failure) = policy_preflight {
                return Err(TransportError::DiagnosticJournalFailure {
                    primary_failure: None,
                    journal_failure,
                });
            }
            if policy.is_disabled() {
                return Err(TransportError::RemoteDebuggingDisabled(format!(
                    "HKLM={}, HKCU={}",
                    policy.machine, policy.user
                )));
            }
            let port = wait_for_debugging_endpoint(
                &mut launched.process,
                &active_port_file,
                startup_deadline,
            )?;
            let initial = match diagnostics.listeners(port.port()) {
                Ok(listeners) => SnapshotEvidence {
                    listeners,
                    error: None,
                },
                Err(error) => SnapshotEvidence {
                    listeners: Vec::new(),
                    error: Some(error),
                },
            };
            let mut evidence = StartupEvidence {
                port: port.port(),
                launched_pid: pid,
                policy,
                initial,
                final_snapshot: None,
                connections: [
                    ConnectionEvidence {
                        family: "ipv4",
                        attempts: 0,
                        last_result: None,
                        succeeded: false,
                    },
                    ConnectionEvidence {
                        family: "ipv6",
                        attempts: 0,
                        last_result: None,
                        succeeded: false,
                    },
                ],
            };
            if let Err(journal_failure) =
                evidence.journal(run, "initial", diagnostics, diagnostic_writer)
            {
                startup_evidence = Some(evidence);
                return Err(TransportError::DiagnosticJournalFailure {
                    primary_failure: None,
                    journal_failure,
                });
            }
            let mut transport = DevToolsBrowserTransport::for_readiness(port, http, websocket);
            let result = wait_for_devtools_readiness(
                &mut launched.process,
                &mut transport,
                port,
                run,
                startup_deadline,
                &mut evidence,
            );
            if result.is_ok() {
                launched.transport = Some(EdgeBrowserTransport::Tcp(transport));
            }
            startup_evidence = Some(evidence);
            result
        })();
        if let Err(error) = startup {
            let (primary_failure, mut journal_failures) = match &error {
                TransportError::DiagnosticJournalFailure {
                    primary_failure,
                    journal_failure,
                } => (primary_failure.clone(), vec![journal_failure.clone()]),
                other => (Some(other.to_string()), Vec::new()),
            };
            if let Some(evidence) = &mut startup_evidence {
                evidence.final_snapshot = Some(match diagnostics.listeners(evidence.port) {
                    Ok(listeners) => SnapshotEvidence {
                        listeners,
                        error: None,
                    },
                    Err(error) => SnapshotEvidence {
                        listeners: Vec::new(),
                        error: Some(error),
                    },
                });
                if let Err(journal_failure) =
                    evidence.journal(run, "final", diagnostics, diagnostic_writer)
                {
                    journal_failures.push(journal_failure);
                }
            }
            let _ = launched.shutdown(run);
            if !journal_failures.is_empty() {
                return Err(TransportError::DiagnosticJournalFailure {
                    primary_failure,
                    journal_failure: journal_failures.join("; "),
                });
            }
            return Err(error);
        }
        Ok(launched)
    }

    /// Browser discovery and attachment API.
    pub fn transport(&mut self) -> &mut dyn BrowserTransport {
        self.transport
            .as_mut()
            .map(|transport| transport as &mut dyn BrowserTransport)
            .expect("launched Edge transport passed readiness before being returned")
    }

    /// Verify process, harness-lock, and ephemeral-port cleanup state.
    pub fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
        Ok(EdgeCleanupStatus {
            process_exited: self.process.exit_code.is_some(),
            harness_lock_absent: path_is_absent(&self.lock_file)?,
            active_port_file_absent: !self.active_port_required
                || path_is_absent(&self.active_port_file)?,
        })
    }

    /// Explicitly terminate Edge, remove the harness lock, and durably journal cleanup.
    pub fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        #[cfg(windows)]
        let pipe_close =
            if let Some(EdgeBrowserTransport::Pipe(transport)) = self.transport.as_mut() {
                transport.close()
            } else {
                Ok(())
            };
        #[cfg(not(windows))]
        let pipe_close: Result<(), TransportError> = Ok(());
        self.transport.take();
        // Process termination must still happen when canceling a reader reports an error.
        let process_result = self.process.shutdown();
        let endpoint_file_result = if !self.active_port_required {
            Ok(false)
        } else if process_result.is_ok() {
            match fs::remove_file(&self.active_port_file) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(TransportError::Process(format!(
                    "remove stale DevToolsActivePort: {error}"
                ))),
            }
        } else {
            Ok(false)
        };
        let cleanup_status = self.cleanup_status();
        let cleanup_status_value = cleanup_status.as_ref().ok();
        let cleanup_succeeded = pipe_close.is_ok()
            && process_result.is_ok()
            && endpoint_file_result.is_ok()
            && cleanup_status_value.is_some_and(EdgeCleanupStatus::is_complete);
        let mut cleanup_errors = Vec::new();
        if let Err(error) = &pipe_close {
            cleanup_errors.push(format!("close pipe transport: {error}"));
        }
        if let Err(error) = &process_result {
            cleanup_errors.push(format!("terminate Edge process: {error}"));
        }
        if let Err(error) = &endpoint_file_result {
            cleanup_errors.push(error.to_string());
        }
        if let Err(error) = &cleanup_status {
            cleanup_errors.push(format!("verify cleanup: {error}"));
        } else if cleanup_status_value.is_some_and(|status| !status.is_complete()) {
            cleanup_errors
                .push("process, harness lock, or active-port cleanup was not verified".to_owned());
        }
        let cleanup_error = (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; "));
        let journal_result = run
            .append_event(
                "browser_shutdown_cleanup",
                json!({
                    "exit_code": process_result.as_ref().ok().copied(),
                    "active_port_file_removed": endpoint_file_result.as_ref().ok().copied().unwrap_or(false),
                    "process_exited": cleanup_status_value.map(|status| status.process_exited),
                    "harness_lock_absent": cleanup_status_value.map(|status| status.harness_lock_absent),
                    "active_port_file_absent": cleanup_status_value.map(|status| status.active_port_file_absent),
                    "cleanup_succeeded": cleanup_succeeded,
                    "cleanup_error": cleanup_error,
                }),
            )
            .map_err(TransportError::Journal);
        let journal_failure = journal_result.err().map(|error| error.to_string());
        match (cleanup_error, journal_failure) {
            (Some(cleanup_failure), Some(journal_failure)) => {
                return Err(TransportError::DiagnosticJournalFailure {
                    primary_failure: Some(cleanup_failure),
                    journal_failure,
                });
            }
            (Some(cleanup_failure), None) => {
                return Err(TransportError::Process(cleanup_failure));
            }
            (None, Some(journal_failure)) => {
                return Err(TransportError::Journal(journal_failure));
            }
            (None, None) => {}
        }
        Ok(())
    }

    /// Gracefully close the persistent init browser before using the owned-tree fallback.
    ///
    /// The Browser.close response wait is bounded to one second and natural process exit is
    /// observed for at most four seconds before the existing exact-owned-tree fallback runs.
    pub fn shutdown_for_profile_init(
        &mut self,
        run: &mut CaptureRun,
    ) -> Result<(), TransportError> {
        self.shutdown_for_profile_init_with_writer_and_timeout(
            run,
            INIT_GRACEFUL_CLOSE_TIMEOUT,
            &DurableDiagnosticEventWriter,
        )
    }

    #[cfg(test)]
    pub(super) fn shutdown_for_profile_init_with_timeout(
        &mut self,
        run: &mut CaptureRun,
        grace: Duration,
    ) -> Result<(), TransportError> {
        self.shutdown_for_profile_init_with_writer_and_timeout(
            run,
            grace,
            &DurableDiagnosticEventWriter,
        )
    }

    pub(super) fn shutdown_for_profile_init_with_writer_and_timeout(
        &mut self,
        run: &mut CaptureRun,
        grace: Duration,
        writer: &dyn DiagnosticEventWriter,
    ) -> Result<(), TransportError> {
        let mut journal_failures = Vec::new();
        let mut cleanup_errors = Vec::new();
        append_init_shutdown_event(
            writer,
            run,
            "init_browser_graceful_close_started",
            json!({"grace_timeout_ms": grace.as_millis()}),
            &mut journal_failures,
        );

        let already_exited = match self.process.try_wait_preserving_lock() {
            Ok(status) => status,
            Err(error) => {
                cleanup_errors.push(format!("inspect owned Edge process before close: {error}"));
                None
            }
        };
        let mut close_request_attempted = false;
        let close_result = if already_exited.is_some() {
            append_init_shutdown_event(
                writer,
                run,
                "init_browser_close_dispatch_result",
                json!({"command":"Browser.close", "result":"not_attempted_process_exited"}),
                &mut journal_failures,
            );
            None
        } else {
            match self.transport.as_mut() {
                #[cfg(windows)]
                Some(EdgeBrowserTransport::Pipe(transport)) => {
                    let result = transport.request_browser_close();
                    close_request_attempted = !matches!(&result, Err(TransportError::Disconnected));
                    let (result_name, error) = match &result {
                        Ok(()) => ("response_received", None),
                        Err(TransportError::Disconnected) => (
                            "could_not_attempt_pipe_already_closed",
                            Some(result_error(&result)),
                        ),
                        Err(TransportError::CommandOutcomeUnknown { .. })
                        | Err(TransportError::Eof { .. }) => (
                            "response_missing_outcome_unknown",
                            Some(result_error(&result)),
                        ),
                        Err(_) => ("command_failed", Some(result_error(&result))),
                    };
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_close_dispatch_result",
                        json!({"command":"Browser.close", "result":result_name, "error":error}),
                        &mut journal_failures,
                    );
                    Some(result)
                }
                Some(EdgeBrowserTransport::Tcp(_)) | None => {
                    let error = "browser-wide pipe transport unavailable";
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_close_dispatch_result",
                        json!({"command":"Browser.close", "result":"could_not_attempt", "error":error}),
                        &mut journal_failures,
                    );
                    cleanup_errors.push(error.to_owned());
                    None
                }
            }
        };

        let natural_exit = if let Some(code) = already_exited {
            Some(code)
        } else {
            match self.process.wait_for_exit_preserving_lock(grace) {
                Ok(Some(code)) => Some(code),
                Ok(None) => {
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_graceful_close_timed_out",
                        json!({"grace_timeout_ms": grace.as_millis()}),
                        &mut journal_failures,
                    );
                    None
                }
                Err(error) => {
                    cleanup_errors.push(format!("wait for natural Edge exit: {error}"));
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_graceful_close_wait_failed",
                        json!({"error":error}),
                        &mut journal_failures,
                    );
                    None
                }
            }
        };

        let mut forced_fallback_attempted = false;
        let mut forced_kill_used = false;
        let exit_code = if let Some(code) = natural_exit {
            append_init_shutdown_event(
                writer,
                run,
                "init_browser_natural_exit_observed",
                json!({
                    "exit_code": code,
                    "browser_close_response": close_result.as_ref().map(|result| match result {
                        Ok(()) => "received",
                        Err(TransportError::CommandOutcomeUnknown { .. } | TransportError::Eof { .. } | TransportError::Disconnected) => "missing_or_disconnect",
                        Err(_) => "failed",
                    }).unwrap_or("not_attempted"),
                }),
                &mut journal_failures,
            );
            Some(code)
        } else {
            forced_fallback_attempted = true;
            append_init_shutdown_event(
                writer,
                run,
                "init_browser_force_kill_fallback_started",
                json!({"owned_pid":self.process.id()}),
                &mut journal_failures,
            );
            match self.process.shutdown_process_only() {
                Ok((code, kill_attempted, kill_succeeded)) => {
                    forced_fallback_attempted = kill_attempted;
                    forced_kill_used = kill_succeeded;
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_force_kill_fallback_result",
                        json!({
                            "result": if kill_succeeded { "succeeded" } else if kill_attempted { "failed_but_process_exited" } else { "process_exited_before_kill" },
                            "kill_attempted": kill_attempted,
                            "forced_kill_used": kill_succeeded,
                            "exit_code":code,
                        }),
                        &mut journal_failures,
                    );
                    Some(code)
                }
                Err(error) => {
                    cleanup_errors
                        .push(format!("forced owned Edge process-tree shutdown: {error}"));
                    append_init_shutdown_event(
                        writer,
                        run,
                        "init_browser_force_kill_fallback_result",
                        json!({"result":"failed", "kill_attempted":true, "forced_kill_used":false, "error":error}),
                        &mut journal_failures,
                    );
                    None
                }
            }
        };

        #[cfg(windows)]
        if let Some(EdgeBrowserTransport::Pipe(transport)) = self.transport.as_mut() {
            if let Err(error) = transport.close() {
                cleanup_errors.push(format!("close DevTools pipe resources: {error}"));
            }
        }
        self.transport.take();
        if let Err(error) = self.process.release_lock_after_exit() {
            cleanup_errors.push(format!("release harness profile lock: {error}"));
        }

        let cleanup_status = self.cleanup_status();
        let status = cleanup_status.as_ref().ok();
        match &cleanup_status {
            Ok(status) if status.is_complete() => {}
            Ok(status) => cleanup_errors.push(format!(
                "cleanup incomplete (process_exited={}, harness_lock_absent={}, active_port_absent={})",
                status.process_exited, status.harness_lock_absent, status.active_port_file_absent
            )),
            Err(error) => cleanup_errors.push(format!("verify cleanup: {error}")),
        }
        let cleanup_succeeded =
            cleanup_errors.is_empty() && status.is_some_and(EdgeCleanupStatus::is_complete);
        append_init_shutdown_event(
            writer,
            run,
            "browser_shutdown_cleanup",
            json!({
                "exit_code": exit_code,
                "process_exited": status.map(|status| status.process_exited),
                "harness_lock_absent": status.map(|status| status.harness_lock_absent),
                "active_port_file_absent": status.map(|status| status.active_port_file_absent),
                "graceful_close_requested": close_request_attempted,
                "natural_exit": natural_exit.is_some(),
                "forced_fallback_attempted": forced_fallback_attempted,
                "forced_kill_used": forced_kill_used,
                "cleanup_succeeded": cleanup_succeeded,
                "cleanup_error": (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; ")),
            }),
            &mut journal_failures,
        );

        let cleanup_failure = (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; "));
        match (
            cleanup_failure,
            (!journal_failures.is_empty()).then(|| journal_failures.join("; ")),
        ) {
            (Some(cleanup), Some(journal)) => Err(TransportError::DiagnosticJournalFailure {
                primary_failure: Some(cleanup),
                journal_failure: journal,
            }),
            (Some(cleanup), None) => Err(TransportError::Process(cleanup)),
            (None, Some(journal)) => Err(TransportError::DiagnosticJournalFailure {
                primary_failure: None,
                journal_failure: journal,
            }),
            (None, None) => Ok(()),
        }
    }
}
