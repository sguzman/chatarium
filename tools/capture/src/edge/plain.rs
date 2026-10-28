//! Legacy Edge process lifecycle implementation (mechanically moved).
use super::*;

impl LaunchedPlainEdge {
    /// Launch ordinary Edge with only the persistent user-data directory and requested URL.
    pub fn launch_profile_auth(run: &mut CaptureRun) -> Result<Self, TransportError> {
        let config = EdgeLaunchConfig::discover()?;
        validate_dedicated_capture_profile(&config.profile, &config.local_app_data)
            .map_err(TransportError::UnsafeProfile)?;
        ensure_profile_directories(&config)?;
        let lock_path = config.capture_root.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone())?;
        let arguments = plain_profile_arguments(&config.profile, crate::init::CHATGPT_START_URL);
        #[cfg(windows)]
        let spawned = windows_plain::spawn(&config.executable, &arguments);
        #[cfg(not(windows))]
        let spawned = SystemEdgeSpawner.spawn(&config.executable, &arguments);
        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                return Err(record_prelaunch_cleanup(
                    run,
                    lock_path,
                    lock,
                    TransportError::Process(format!("launch plain Edge: {error}")),
                ));
            }
        };
        let pid = child.id();
        let mut process = EdgeProcess::new(child, lock);
        // Plain-auth cleanup is gated by observing the visible window close; never kill it from
        // Drop while ownership or operator completion is uncertain.
        process.preserve_on_drop();
        let mut launched = Self {
            process,
            lock_file: lock_path,
            observer: Box::<SystemPlainAuthWindowObserver>::default(),
            startup_timeout: config.startup_timeout,
            process_exit_grace: PLAIN_PROCESS_EXIT_GRACE,
            window_close_observed: false,
        };
        if let Err(error) = run.append_event(
            "plain_auth_edge_process_started",
            json!({
                "pid": pid,
                "transport_mode": "none",
                "remote_debugging": false,
                "profile": crate::init::PROFILE_IDENTITY,
                "requested_start_url": crate::init::CHATGPT_START_URL,
            }),
        ) {
            launched.preserve_active_process();
            return Err(TransportError::DiagnosticJournalFailure {
                primary_failure: Some("plain Edge started but its durable launch event failed; browser and profile lock were retained safely".to_owned()),
                journal_failure: error,
            });
        }
        Ok(launched)
    }

    #[cfg(test)]
    pub(super) fn with_observer(
        process: EdgeProcess,
        lock_file: PathBuf,
        observer: Box<dyn PlainAuthWindowObserver>,
        startup_timeout: Duration,
        process_exit_grace: Duration,
    ) -> Self {
        let mut process = process;
        process.preserve_on_drop();
        Self {
            process,
            lock_file,
            observer,
            startup_timeout,
            process_exit_grace,
            window_close_observed: false,
        }
    }

    /// Wait for a visible owned window and then for its disappearance as operator completion.
    pub fn wait_for_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        let started = run.append_event(
            "auth_phase_started",
            json!({"auth_phase_transport":"plain_browser", "remote_debugging":false}),
        );
        if let Err(error) = started {
            return Err(TransportError::Journal(error));
        }
        let deadline = Instant::now() + self.startup_timeout;
        let mut window_seen = false;
        let mut close_observed_at = None;
        loop {
            if let Some(exit_code) = self
                .process
                .try_wait_preserving_lock()
                .map_err(|error| TransportError::Process(format!("inspect plain Edge: {error}")))?
            {
                if !window_seen {
                    let tree_alive = self
                        .observer
                        .owned_process_tree_alive(
                            self.process.child_ref().map_err(TransportError::Process)?,
                        )
                        .map_err(|error| {
                            TransportError::Process(format!(
                                "verify plain Edge descendants after exit: {error}"
                            ))
                        })?;
                    if tree_alive {
                        self.process.preserve_on_drop();
                    } else {
                        self.process.release_lock_after_exit().map_err(|error| {
                            TransportError::Process(format!(
                                "release profile lock after plain Edge exit: {error}"
                            ))
                        })?;
                    }
                    let lock_absent = path_is_absent(&self.lock_file).unwrap_or(false);
                    let cleanup_event = run.append_event("plain_auth_browser_cleanup", json!({
                        "exit_code":exit_code, "process_exited":true, "owned_descendants_remain":tree_alive, "harness_lock_absent":lock_absent, "cleanup_succeeded":!tree_alive && lock_absent, "cleanup_error":if tree_alive { Some("owned Edge descendants remain; profile lock retained") } else { None }
                    }));
                    let primary = if tree_alive {
                        TransportError::Process(format!(
                            "plain Edge exited with code {exit_code} before a window appeared; owned descendants remain"
                        ))
                    } else {
                        TransportError::Process(format!(
                            "plain Edge exited with code {exit_code} before an authentication window appeared"
                        ))
                    };
                    return match cleanup_event {
                        Ok(_) => Err(primary),
                        Err(journal_failure) => Err(TransportError::DiagnosticJournalFailure {
                            primary_failure: Some(primary.to_string()),
                            journal_failure,
                        }),
                    };
                }
                if close_observed_at.is_none() {
                    close_observed_at = Some(Instant::now());
                }
            }

            let visible = self
                .observer
                .has_visible_owned_window(
                    self.process.child_ref().map_err(TransportError::Process)?,
                )
                .map_err(|error| {
                    TransportError::Process(format!("observe plain Edge window: {error}"))
                })?;
            if visible {
                window_seen = true;
                close_observed_at = None;
            } else if window_seen && close_observed_at.is_none() {
                close_observed_at = Some(Instant::now());
            }

            if let Some(closed_at) = close_observed_at {
                if Instant::now().duration_since(closed_at) >= INIT_PLAIN_CLOSE_GRACE {
                    self.window_close_observed = true;
                    run.append_event(
                        "auth_phase_completed_by_window_close",
                        json!({"owned_window_seen":true, "close_grace_ms":INIT_PLAIN_CLOSE_GRACE.as_millis()}),
                    ).map_err(TransportError::Journal)?;
                    return Ok(());
                }
            } else if !window_seen && Instant::now() >= deadline {
                return Err(TransportError::Process(
                    "plain Edge did not show an owned authentication window before the startup deadline".to_owned(),
                ));
            }
            thread::sleep(PLAIN_WINDOW_POLL_INTERVAL);
        }
    }

    /// After the visible window closed, wait briefly, then terminate only the owned process tree.
    pub fn shutdown_after_window_close(
        &mut self,
        run: &mut CaptureRun,
    ) -> Result<(), TransportError> {
        if !self.window_close_observed {
            self.process.preserve_on_drop();
            return Err(TransportError::Process(
                "refusing to terminate plain Edge before its owned visible window close is observed"
                    .to_owned(),
            ));
        }
        let mut cleanup_error = None;
        let mut exit_code = None;
        let natural_exit_deadline = Instant::now() + self.process_exit_grace;
        let mut owned_alive_at_deadline = false;
        while cleanup_error.is_none() {
            match self.process.try_wait_preserving_lock() {
                Ok(code) => exit_code = code,
                Err(error) => {
                    cleanup_error = Some(format!("inspect plain Edge exit: {error}"));
                    break;
                }
            }
            let owned_alive = match self
                .process
                .child_ref()
                .and_then(|child| self.observer.owned_process_tree_alive(child))
            {
                Ok(alive) => alive,
                Err(error) => {
                    cleanup_error =
                        Some(format!("verify owned Edge job during exit grace: {error}"));
                    break;
                }
            };
            if !owned_alive && exit_code.is_some() {
                break;
            }
            let remaining = natural_exit_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                owned_alive_at_deadline = owned_alive || exit_code.is_none();
                break;
            }
            thread::sleep(remaining.min(PROCESS_EXIT_POLL_INTERVAL));
        }
        if cleanup_error.is_none() {
            if owned_alive_at_deadline {
                match self.process.terminate_owned_processes() {
                    Ok(code) => exit_code = Some(code),
                    Err(error) => {
                        cleanup_error = Some(format!("terminate owned plain Edge job: {error}"))
                    }
                }
            }
        }
        if exit_code.is_some() && cleanup_error.is_none() {
            let verify_deadline = Instant::now() + PLAIN_JOB_TERMINATION_TIMEOUT;
            loop {
                match self
                    .process
                    .child_ref()
                    .and_then(|child| self.observer.owned_process_tree_alive(child))
                {
                    Ok(false) => {
                        if let Err(error) = self.process.release_lock_after_exit() {
                            cleanup_error = Some(format!("release profile lock: {error}"));
                        }
                        break;
                    }
                    Ok(true) => {
                        let remaining = verify_deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            cleanup_error = Some("harness-owned Edge processes remain after termination; retaining the profile lock".to_owned());
                            self.process.preserve_on_drop();
                            break;
                        }
                        thread::sleep(remaining.min(PROCESS_EXIT_POLL_INTERVAL));
                    }
                    Err(error) => {
                        cleanup_error =
                            Some(format!("verify harness-owned Edge job is empty: {error}"));
                        self.process.preserve_on_drop();
                        break;
                    }
                }
            }
        }
        let lock_absent = path_is_absent(&self.lock_file).unwrap_or(false);
        let process_exited = exit_code.is_some();
        let cleanup_succeeded = process_exited && lock_absent && cleanup_error.is_none();
        if !cleanup_succeeded {
            if cleanup_error.is_none() {
                cleanup_error = Some(format!(
                    "plain Edge cleanup incomplete (process_exited={process_exited}, harness_lock_absent={lock_absent})"
                ));
            }
            self.process.preserve_on_drop();
        }
        let journal = run.append_event(
            "plain_auth_browser_cleanup",
            json!({
                "exit_code":exit_code,
                "process_exited":process_exited,
                "harness_lock_absent":lock_absent,
                "cleanup_succeeded":cleanup_succeeded,
                "cleanup_error":cleanup_error,
            }),
        );
        match (cleanup_error, journal) {
            (Some(primary), Err(journal_failure)) => {
                Err(TransportError::DiagnosticJournalFailure {
                    primary_failure: Some(primary),
                    journal_failure,
                })
            }
            (Some(primary), Ok(_)) => Err(TransportError::Process(primary)),
            (None, Err(journal_failure)) => Err(TransportError::Journal(journal_failure)),
            (None, Ok(_)) => Ok(()),
        }
    }

    /// Preserve an active browser and lock if window ownership could not be established.
    pub fn preserve_active_process(&mut self) {
        self.process.preserve_on_drop();
    }

    /// Whether an owned visible window has been observed closed for the configured grace.
    #[must_use]
    pub const fn window_close_observed(&self) -> bool {
        self.window_close_observed
    }

    /// Verify that both the owned process tree and Chatarium profile lock are released.
    pub fn phase_boundary_released(&self) -> Result<bool, TransportError> {
        let tree_alive = self
            .observer
            .owned_process_tree_alive(self.process.child_ref().map_err(TransportError::Process)?)
            .map_err(|error| {
                TransportError::Process(format!("verify plain Edge process tree: {error}"))
            })?;
        Ok(self.process.exit_code.is_some() && !tree_alive && path_is_absent(&self.lock_file)?)
    }
}
