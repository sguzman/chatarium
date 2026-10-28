//! Edge child-process and profile-lock lifecycles, preserving drop cleanup.
use super::*;

pub(super) struct EdgeProcess {
    pub(super) child: Option<Box<dyn ManagedEdgeChild>>,
    pub(super) lock: Option<ProfileLock>,
    pub(super) exit_code: Option<i32>,
    pub(super) shutdown_on_drop: bool,
}

impl EdgeProcess {
    pub(super) fn new(child: Box<dyn ManagedEdgeChild>, mut lock: ProfileLock) -> Self {
        lock.retained_on_drop = true;
        Self {
            child: Some(child),
            lock: Some(lock),
            exit_code: None,
            shutdown_on_drop: true,
        }
    }

    pub(super) fn try_wait(&mut self) -> Result<Option<i32>, String> {
        let status = self.try_wait_preserving_lock()?;
        if status.is_some() {
            self.release_lock_after_exit()?;
        }
        Ok(status)
    }

    pub(super) fn try_wait_preserving_lock(&mut self) -> Result<Option<i32>, String> {
        if self.exit_code.is_some() {
            return Ok(self.exit_code);
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "owned Edge child is missing".to_owned())?;
        let status = child.try_wait()?;
        if let Some(code) = status {
            self.exit_code = Some(code);
        }
        Ok(status)
    }

    pub(super) fn id(&self) -> u32 {
        self.child.as_ref().map_or(0, |child| child.id())
    }

    pub(super) fn child_ref(&self) -> Result<&dyn ManagedEdgeChild, String> {
        self.child
            .as_deref()
            .ok_or_else(|| "owned Edge child is missing".to_owned())
    }

    pub(super) fn wait_for_exit_preserving_lock(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<i32>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(code) = self.try_wait_preserving_lock()? {
                return Ok(Some(code));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            thread::sleep(remaining.min(PROCESS_EXIT_POLL_INTERVAL));
        }
    }

    pub(super) fn shutdown(&mut self) -> Result<i32, String> {
        let (code, _, _) = self.shutdown_process_only()?;
        self.release_lock_after_exit()?;
        Ok(code)
    }

    pub(super) fn preserve_on_drop(&mut self) {
        self.shutdown_on_drop = false;
        if let Some(lock) = &mut self.lock {
            lock.retained_on_drop = true;
        }
    }

    pub(super) fn shutdown_process_only(&mut self) -> Result<(i32, bool, bool), String> {
        if let Some(code) = self.exit_code {
            return Ok((code, false, false));
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "owned Edge child is missing".to_owned())?;
        let (code, kill_attempted, kill_succeeded) = if let Some(code) = child.try_wait()? {
            (code, false, false)
        } else {
            if let Err(kill_error) = child.kill() {
                if let Some(code) = child.try_wait()? {
                    (code, true, false)
                } else {
                    return Err(format!("terminate Edge process: {kill_error}"));
                }
            } else {
                (child.wait()?, true, true)
            }
        };
        self.exit_code = Some(code);
        Ok((code, kill_attempted, kill_succeeded))
    }

    pub(super) fn terminate_owned_processes(&mut self) -> Result<i32, String> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "owned Edge child is missing".to_owned())?;
        child.terminate_owned_processes()?;
        let code = if let Some(code) = self.exit_code {
            code
        } else {
            child.wait()?
        };
        self.exit_code = Some(code);
        Ok(code)
    }

    pub(super) fn release_lock_after_exit(&mut self) -> Result<(), String> {
        if self.exit_code.is_some() {
            if let Some(lock) = self.lock.take() {
                lock.release()?;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(super) fn pipe_readiness_retryable(error: &TransportError) -> bool {
    match error {
        TransportError::CommandOutcomeUnknown { reason, .. } => reason.contains("timed out"),
        TransportError::Timeout { .. } => true,
        _ => false,
    }
}

#[cfg(windows)]
pub(super) fn journal_pipe_readiness_failure(
    run: &mut CaptureRun,
    attempts: u32,
    primary: &TransportError,
) -> TransportError {
    match run.append_event(
        "devtools_pipe_readiness_failed",
        json!({"transport_mode":"pipe", "attempts":attempts, "error":primary.to_string()}),
    ) {
        Ok(_) => primary.clone(),
        Err(journal_failure) => TransportError::DiagnosticJournalFailure {
            primary_failure: Some(primary.to_string()),
            journal_failure,
        },
    }
}

#[cfg(windows)]
pub(super) fn fail_pipe_startup(
    launched: &mut LaunchedEdge,
    run: &mut CaptureRun,
    error: TransportError,
) -> TransportError {
    let original = error.clone();
    let (primary_failure, mut journal_failures) = match &error {
        TransportError::Journal(journal) => (None, vec![journal.clone()]),
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure,
        } => (primary_failure.clone(), vec![journal_failure.clone()]),
        other => (Some(other.to_string()), Vec::new()),
    };
    if let Err(error) = launched.shutdown(run) {
        match error {
            TransportError::Journal(journal)
            | TransportError::DiagnosticJournalFailure {
                journal_failure: journal,
                ..
            } => journal_failures.push(journal),
            _ => {}
        }
    }
    if journal_failures.is_empty() {
        original
    } else {
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure: journal_failures.join("; "),
        }
    }
}

impl Drop for EdgeProcess {
    fn drop(&mut self) {
        if self.shutdown_on_drop {
            let _ = self.shutdown();
        }
    }
}

pub(super) struct ProfileLock {
    pub(super) path: PathBuf,
    pub(super) token: String,
    pub(super) released: bool,
    pub(super) retained_on_drop: bool,
}

impl ProfileLock {
    pub(super) fn acquire(path: PathBuf) -> Result<Self, TransportError> {
        let token = format!("pid={} created_unix_ms={}\n", std::process::id(), unix_ms());
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(TransportError::StaleState(format!(
                    "{} already exists; inspect the harness-owned Edge state and remove the lock only after confirming no capture browser is running",
                    path.display()
                )));
            }
            Err(error) => {
                return Err(TransportError::Process(format!(
                    "create Edge profile lock {}: {error}",
                    path.display()
                )));
            }
        };
        let lock = Self {
            path,
            token,
            released: false,
            retained_on_drop: false,
        };
        if let Err(error) = file
            .write_all(lock.token.as_bytes())
            .and_then(|()| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&lock.path);
            return Err(TransportError::Process(format!(
                "write/sync Edge profile lock: {error}"
            )));
        }
        Ok(lock)
    }

    pub(super) fn release(mut self) -> Result<(), String> {
        let mut actual = String::new();
        OpenOptions::new()
            .read(true)
            .open(&self.path)
            .and_then(|mut file| file.read_to_string(&mut actual))
            .map_err(|error| format!("read harness lock {}: {error}", self.path.display()))?;
        if actual != self.token {
            return Err(format!(
                "harness lock {} changed while Edge was running; leaving it in place",
                self.path.display()
            ));
        }
        fs::remove_file(&self.path)
            .map_err(|error| format!("remove harness lock {}: {error}", self.path.display()))?;
        self.released = true;
        Ok(())
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        if self.released || self.retained_on_drop {
            return;
        }
        let mut actual = String::new();
        let matches = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .and_then(|mut file| file.read_to_string(&mut actual))
            .is_ok()
            && actual == self.token;
        if matches {
            let _ = fs::remove_file(&self.path);
        }
    }
}
