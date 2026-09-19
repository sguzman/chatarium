//! Windows-first harness-owned Edge process lifecycle.

mod job_membership;
#[cfg(windows)]
#[path = "edge/lock_recovery.rs"]
mod lock_recovery;
#[cfg(windows)]
#[path = "edge/windows_plain.rs"]
mod windows_plain;

#[cfg(test)]
use crate::diagnostics::PolicyState;
#[cfg(not(test))]
use crate::diagnostics::SystemEdgeDiagnostics;
use crate::diagnostics::{EdgeDiagnostics, ListenerRecord, OwnerRelation, RemoteDebuggingPolicy};
use crate::run::CaptureRun;
use crate::transport::{
    BrowserTransport, BrowserVersion, DevToolsBrowserTransport, DevToolsEndpoint, DevToolsHttp,
    DevToolsPort, LoopbackAddressFamily, LoopbackDevToolsHttp, LoopbackWebSocketConnector,
    PageSession, TargetInfo, TransportError, WebSocketConnector,
};
use crate::{find_edge_executable, validate_dedicated_capture_profile};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const DEVTOOLS_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
const DEVTOOLS_RETRY_DELAY: Duration = Duration::from_millis(50);
const INIT_GRACEFUL_CLOSE_TIMEOUT: Duration = Duration::from_secs(4);
const PROCESS_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const ACTIVE_PORT_FILE: &str = "DevToolsActivePort";
const LOCK_FILE: &str = "edge-profile.harness.lock";

#[derive(Debug, Clone)]
struct SnapshotEvidence {
    listeners: Vec<ListenerRecord>,
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ConnectionEvidence {
    family: &'static str,
    attempts: u32,
    last_result: Option<String>,
    succeeded: bool,
}

#[derive(Debug, Clone)]
struct StartupEvidence {
    port: u16,
    launched_pid: u32,
    policy: RemoteDebuggingPolicy,
    initial: SnapshotEvidence,
    final_snapshot: Option<SnapshotEvidence>,
    connections: [ConnectionEvidence; 2],
}

impl StartupEvidence {
    fn connection_mut(&mut self, family: LoopbackAddressFamily) -> &mut ConnectionEvidence {
        match family {
            LoopbackAddressFamily::Ipv4 => &mut self.connections[0],
            LoopbackAddressFamily::Ipv6 => &mut self.connections[1],
        }
    }

    fn snapshot_value(
        &self,
        snapshot: &SnapshotEvidence,
        diagnostics: &dyn EdgeDiagnostics,
    ) -> serde_json::Value {
        let listeners = snapshot
            .listeners
            .iter()
            .map(|listener| {
                let relation = listener.owning_pid.map_or(OwnerRelation::Unknown, |owner| {
                    diagnostics.owner_relation(owner, self.launched_pid)
                });
                json!({
                    "local_address": listener.local_address.to_string(),
                    "family": if listener.local_address.is_ipv4() { "ipv4" } else { "ipv6" },
                    "local_port": listener.local_port,
                    "state": listener.state,
                    "owning_pid": listener.owning_pid,
                    "owner_relation": relation,
                    "same_as_launched_pid": listener.owning_pid == Some(self.launched_pid),
                })
            })
            .collect::<Vec<_>>();
        json!({ "listeners": listeners, "inspection_error": snapshot.error })
    }

    fn journal(
        &self,
        run: &mut CaptureRun,
        stage: &str,
        diagnostics: &dyn EdgeDiagnostics,
        writer: &dyn DiagnosticEventWriter,
    ) -> Result<(), String> {
        let snapshot = match stage {
            "initial" => Some(&self.initial),
            "final" => self.final_snapshot.as_ref(),
            _ => None,
        };
        let listeners = snapshot.map(|snapshot| self.snapshot_value(snapshot, diagnostics));
        writer.append(
            run,
            "devtools_os_diagnostics",
            json!({
                "stage": stage,
                "port": self.port,
                "launched_edge_pid": self.launched_pid,
                "remote_debugging_allowed": {
                    "summary": self.policy.summary(),
                    "machine": self.policy.machine,
                    "user": self.policy.user,
                },
                "listener_snapshot": listeners,
                "connect_results": self.connections,
            }),
        )
    }
}

trait DiagnosticEventWriter: Send + Sync {
    fn append(
        &self,
        run: &mut CaptureRun,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), String>;
}

struct DurableDiagnosticEventWriter;

impl DiagnosticEventWriter for DurableDiagnosticEventWriter {
    fn append(
        &self,
        run: &mut CaptureRun,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        run.append_event(kind, payload).map(|_| ())
    }
}

/// Child-process operations isolated for deterministic process-lifecycle testing.
pub trait ManagedEdgeChild: Send {
    /// Process ID assigned by the operating system.
    fn id(&self) -> u32;
    /// Return the process exit code if it has already exited.
    fn try_wait(&mut self) -> Result<Option<i32>, String>;
    /// Terminate the owned Edge process.
    fn kill(&mut self) -> Result<(), String>;
    /// Wait until the owned Edge process exits.
    fn wait(&mut self) -> Result<i32, String>;
    /// Return the current process IDs positively retained by the plain-auth ownership boundary.
    fn owned_process_ids(&self) -> Result<Vec<u32>, String> {
        Ok(vec![self.id()])
    }
    /// Confirm that a PID currently resolves to a process inside the retained ownership boundary.
    fn owns_live_process_id(&self, pid: u32) -> Result<bool, String> {
        Ok(self.owned_process_ids()?.contains(&pid))
    }
    /// Whether any process in the plain-auth ownership boundary remains alive.
    fn owned_processes_alive(&self) -> Result<bool, String> {
        Ok(true)
    }
    /// Terminate only processes positively held by the plain-auth ownership boundary.
    fn terminate_owned_processes(&mut self) -> Result<(), String> {
        self.kill()
    }
}

/// Edge process creation boundary, injectable in tests.
pub trait EdgeProcessSpawner: Send + Sync {
    /// Spawn the requested executable with the supplied harness-owned arguments.
    fn spawn(
        &self,
        executable: &Path,
        arguments: &[String],
    ) -> Result<Box<dyn ManagedEdgeChild>, String>;
}

struct SystemEdgeSpawner;

#[cfg(test)]
struct TestEdgeDiagnostics;

#[cfg(test)]
impl EdgeDiagnostics for TestEdgeDiagnostics {
    fn listeners(&self, _port: u16) -> Result<Vec<ListenerRecord>, String> {
        Ok(Vec::new())
    }

    fn remote_debugging_policy(&self) -> RemoteDebuggingPolicy {
        RemoteDebuggingPolicy {
            machine: PolicyState::NotConfigured,
            user: PolicyState::NotConfigured,
        }
    }

    fn owner_relation(&self, _owner_pid: u32, _launched_pid: u32) -> OwnerRelation {
        OwnerRelation::Unknown
    }
}

impl EdgeProcessSpawner for SystemEdgeSpawner {
    fn spawn(
        &self,
        executable: &Path,
        arguments: &[String],
    ) -> Result<Box<dyn ManagedEdgeChild>, String> {
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().map_err(|error| error.to_string())?;
        Ok(Box::new(SystemEdgeChild(child)))
    }
}

struct SystemEdgeChild(Child);

impl ManagedEdgeChild for SystemEdgeChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        self.0
            .try_wait()
            .map(|status| status.map(exit_code))
            .map_err(|error| error.to_string())
    }

    fn kill(&mut self) -> Result<(), String> {
        terminate_edge_process_tree(&mut self.0)
    }

    fn wait(&mut self) -> Result<i32, String> {
        self.0
            .wait()
            .map(exit_code)
            .map_err(|error| error.to_string())
    }
}

#[cfg(windows)]
fn terminate_edge_process_tree(child: &mut Child) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
        "SystemRoot is unavailable; cannot terminate the owned Edge process tree".to_owned()
    })?;
    let taskkill = PathBuf::from(system_root)
        .join("System32")
        .join("taskkill.exe");
    let pid = child.id().to_string();
    let status = Command::new(taskkill)
        .args(["/PID", &pid, "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|error| format!("run taskkill for owned Edge PID {pid}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "taskkill could not terminate owned Edge process tree rooted at PID {pid} (exit {})",
            status.code().unwrap_or(-1)
        ))
    }
}

#[cfg(not(windows))]
fn terminate_edge_process_tree(child: &mut Child) -> Result<(), String> {
    child.kill().map_err(|error| error.to_string())
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Concrete Edge launch configuration derived from standard Windows locations.
#[derive(Debug, Clone)]
pub struct EdgeLaunchConfig {
    local_app_data: PathBuf,
    executable: PathBuf,
    profile: PathBuf,
    capture_root: PathBuf,
    startup_timeout: Duration,
}

impl EdgeLaunchConfig {
    /// Discover Edge and derive the only allowed Chatarium-owned profile.
    pub fn discover() -> Result<Self, TransportError> {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| TransportError::Process("LOCALAPPDATA is unavailable".to_owned()))?;
        let local_app_data = fs::canonicalize(&local_app_data).map_err(|error| {
            TransportError::UnsafeProfile(format!("resolve LOCALAPPDATA: {error}"))
        })?;
        let capture_root = local_app_data.join("Chatarium").join("capture-browser");
        let profile = capture_root.join("edge-profile");
        validate_dedicated_capture_profile(&profile, &local_app_data)
            .map_err(TransportError::UnsafeProfile)?;
        let executable = find_edge_executable().ok_or_else(|| {
            TransportError::Process(
                "Microsoft Edge was not found in standard install locations".to_owned(),
            )
        })?;
        Ok(Self {
            local_app_data,
            executable,
            profile,
            capture_root,
            startup_timeout: STARTUP_TIMEOUT,
        })
    }

    /// Candidate Edge executable used by this launch.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Dedicated profile passed to Edge.
    #[must_use]
    pub fn profile(&self) -> &Path {
        &self.profile
    }
}

/// Running harness-owned browser plus its local DevTools transport.
pub struct LaunchedEdge {
    process: EdgeProcess,
    lock_file: PathBuf,
    active_port_file: PathBuf,
    transport: Option<EdgeBrowserTransport>,
    active_port_required: bool,
}

enum EdgeBrowserTransport {
    Tcp(DevToolsBrowserTransport),
    #[cfg(windows)]
    Pipe(crate::pipe::PipeCdpBrowserTransport),
}

impl BrowserTransport for EdgeBrowserTransport {
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError> {
        match self {
            Self::Tcp(transport) => transport.browser_version(run),
            #[cfg(windows)]
            Self::Pipe(transport) => transport.browser_version(run),
        }
    }
    fn browser_version_with_timeout(
        &mut self,
        run: &mut CaptureRun,
        timeout: Duration,
    ) -> Result<BrowserVersion, TransportError> {
        match self {
            Self::Tcp(transport) => transport.browser_version_with_timeout(run, timeout),
            #[cfg(windows)]
            Self::Pipe(transport) => transport.browser_version(run),
        }
    }
    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        match self {
            Self::Tcp(transport) => transport.list_targets(run),
            #[cfg(windows)]
            Self::Pipe(transport) => transport.list_targets(run),
        }
    }
    fn refresh_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        match self {
            Self::Tcp(transport) => transport.list_targets(run),
            #[cfg(windows)]
            Self::Pipe(transport) => transport.refresh_targets(run),
        }
    }
    fn attach(
        &mut self,
        target_id: &str,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PageSession>, TransportError> {
        match self {
            Self::Tcp(transport) => transport.attach(target_id, run),
            #[cfg(windows)]
            Self::Pipe(transport) => transport.attach(target_id, run),
        }
    }
}

/// Independently verified state after closing a harness-owned Edge process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeCleanupStatus {
    /// The child process was positively observed to exit and be waited.
    pub process_exited: bool,
    /// The harness profile lock path is absent.
    pub harness_lock_absent: bool,
    /// Chromium's `DevToolsActivePort` path is absent.
    pub active_port_file_absent: bool,
}

/// Plain-browser authentication window observation, limited to the owned Edge process tree.
pub trait PlainAuthWindowObserver: Send + Sync {
    /// Return whether any visible top-level window belongs to the owned Edge job.
    fn has_visible_owned_window(&self, child: &dyn ManagedEdgeChild) -> Result<bool, String>;
    /// Return whether any process remains in the retained ownership boundary.
    fn owned_process_tree_alive(&self, child: &dyn ManagedEdgeChild) -> Result<bool, String>;
}

/// System observer used only while the plain, non-CDP authentication browser is open.
#[derive(Debug, Default)]
pub struct SystemPlainAuthWindowObserver;

impl PlainAuthWindowObserver for SystemPlainAuthWindowObserver {
    fn has_visible_owned_window(&self, child: &dyn ManagedEdgeChild) -> Result<bool, String> {
        #[cfg(windows)]
        {
            let pids = child.owned_process_ids()?;
            for pid in windows_plain::visible_window_owners(&pids)? {
                if child.owns_live_process_id(pid)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        #[cfg(not(windows))]
        {
            let _ = child;
            Err("plain Edge window observation requires Windows".to_owned())
        }
    }

    fn owned_process_tree_alive(&self, child: &dyn ManagedEdgeChild) -> Result<bool, String> {
        child.owned_processes_alive()
    }
}

/// Harness-owned plain Edge process used for the human authentication phase.
pub struct LaunchedPlainEdge {
    process: EdgeProcess,
    lock_file: PathBuf,
    observer: Box<dyn PlainAuthWindowObserver>,
    startup_timeout: Duration,
    process_exit_grace: Duration,
    window_close_observed: bool,
}

fn plain_profile_arguments(profile: &Path, start_url: &str) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", profile.display()),
        start_url.to_owned(),
    ]
}

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
    fn with_observer(
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

const PLAIN_WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(750);
const INIT_PLAIN_CLOSE_GRACE: Duration = Duration::from_millis(500);
const PLAIN_PROCESS_EXIT_GRACE: Duration = Duration::from_secs(4);
const PLAIN_JOB_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

impl EdgeCleanupStatus {
    /// Whether every required cleanup condition was positively established.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.process_exited && self.harness_lock_absent && self.active_port_file_absent
    }
}

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
    fn launch_pipe_profile(
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
    fn launch_with(
        config: EdgeLaunchConfig,
        run: &mut CaptureRun,
        spawner: &dyn EdgeProcessSpawner,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
    ) -> Result<Self, TransportError> {
        Self::launch_with_options(config, run, spawner, http, websocket, false)
    }

    fn launch_with_options(
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

    fn launch_with_diagnostics(
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
        let pipe_close =
            if let Some(EdgeBrowserTransport::Pipe(transport)) = self.transport.as_mut() {
                transport.close()
            } else {
                Ok(())
            };
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
    fn shutdown_for_profile_init_with_timeout(
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

    fn shutdown_for_profile_init_with_writer_and_timeout(
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

fn append_init_shutdown_event(
    writer: &dyn DiagnosticEventWriter,
    run: &mut CaptureRun,
    kind: &str,
    payload: serde_json::Value,
    failures: &mut Vec<String>,
) {
    if let Err(error) = writer.append(run, kind, payload) {
        failures.push(format!("{kind}: {error}"));
    }
}

fn result_error(result: &Result<(), TransportError>) -> String {
    result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn path_is_absent(path: &Path) -> Result<bool, TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(TransportError::Process(format!(
            "verify cleanup path {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(all(windows, test))]
fn pipe_smoke_arguments(profile: &Path) -> Vec<String> {
    pipe_profile_arguments(profile, "about:blank", true)
}

#[cfg(windows)]
fn pipe_profile_arguments(profile: &Path, start_url: &str, incognito: bool) -> Vec<String> {
    let mut arguments = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
    ];
    if incognito {
        arguments.push("--incognito".to_owned());
    }
    arguments.push(start_url.to_owned());
    arguments
}

#[cfg(windows)]
fn record_prelaunch_cleanup(
    run: &mut CaptureRun,
    lock_path: PathBuf,
    lock: ProfileLock,
    primary: TransportError,
) -> TransportError {
    drop(lock);
    let (lock_absent, cleanup_error) = match path_is_absent(&lock_path) {
        Ok(absent) => (absent, None),
        Err(error) => (false, Some(error.to_string())),
    };
    let cleanup_succeeded = lock_absent && cleanup_error.is_none();
    let cleanup_journal = run.append_event(
        "browser_shutdown_cleanup",
        json!({
            "exit_code": null,
            "process_exited": true,
            "harness_lock_absent": lock_absent,
            "active_port_file_absent": true,
            "active_port_file_required": false,
            "cleanup_succeeded": cleanup_succeeded,
            "cleanup_error": cleanup_error,
        }),
    );
    let original = primary.clone();
    let (primary_failure, mut journal_failures) = match primary {
        TransportError::Journal(error) => (None, vec![error]),
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure,
        } => (primary_failure, vec![journal_failure]),
        other => (Some(other.to_string()), Vec::new()),
    };
    if let Err(error) = cleanup_journal {
        journal_failures.push(error);
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

struct EdgeProcess {
    child: Option<Box<dyn ManagedEdgeChild>>,
    lock: Option<ProfileLock>,
    exit_code: Option<i32>,
    shutdown_on_drop: bool,
}

impl EdgeProcess {
    fn new(child: Box<dyn ManagedEdgeChild>, mut lock: ProfileLock) -> Self {
        lock.retained_on_drop = true;
        Self {
            child: Some(child),
            lock: Some(lock),
            exit_code: None,
            shutdown_on_drop: true,
        }
    }

    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        let status = self.try_wait_preserving_lock()?;
        if status.is_some() {
            self.release_lock_after_exit()?;
        }
        Ok(status)
    }

    fn try_wait_preserving_lock(&mut self) -> Result<Option<i32>, String> {
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

    fn id(&self) -> u32 {
        self.child.as_ref().map_or(0, |child| child.id())
    }

    fn child_ref(&self) -> Result<&dyn ManagedEdgeChild, String> {
        self.child
            .as_deref()
            .ok_or_else(|| "owned Edge child is missing".to_owned())
    }

    fn wait_for_exit_preserving_lock(&mut self, timeout: Duration) -> Result<Option<i32>, String> {
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

    fn shutdown(&mut self) -> Result<i32, String> {
        let (code, _, _) = self.shutdown_process_only()?;
        self.release_lock_after_exit()?;
        Ok(code)
    }

    fn preserve_on_drop(&mut self) {
        self.shutdown_on_drop = false;
        if let Some(lock) = &mut self.lock {
            lock.retained_on_drop = true;
        }
    }

    fn shutdown_process_only(&mut self) -> Result<(i32, bool, bool), String> {
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

    fn terminate_owned_processes(&mut self) -> Result<i32, String> {
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

    fn release_lock_after_exit(&mut self) -> Result<(), String> {
        if self.exit_code.is_some() {
            if let Some(lock) = self.lock.take() {
                lock.release()?;
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn pipe_readiness_retryable(error: &TransportError) -> bool {
    match error {
        TransportError::CommandOutcomeUnknown { reason, .. } => reason.contains("timed out"),
        TransportError::Timeout { .. } => true,
        _ => false,
    }
}

#[cfg(windows)]
fn journal_pipe_readiness_failure(
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
fn fail_pipe_startup(
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

struct ProfileLock {
    path: PathBuf,
    token: String,
    released: bool,
    retained_on_drop: bool,
}

impl ProfileLock {
    fn acquire(path: PathBuf) -> Result<Self, TransportError> {
        let profile = path
            .parent()
            .map(|parent| parent.join("edge-profile"))
            .ok_or_else(|| {
                TransportError::StaleState("harness lock has no profile root".to_owned())
            })?;
        let token = lock_contents(&path)?;
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return reclaim_or_reject_lock(&path, &profile, &token);
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

    fn release(mut self) -> Result<(), String> {
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

fn lock_contents(_path: &Path) -> Result<String, TransportError> {
    #[cfg(windows)]
    {
        let identity = lock_recovery::current_process_identity()?;
        return Ok(format!(
            "version=2\nharness_pid={}\nharness_creation_filetime={}\nlock_created_unix_ms={}\ntoken={}\n",
            std::process::id(),
            identity,
            unix_ms(),
            unix_ms()
        ));
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "pid={} created_unix_ms={}\n",
            std::process::id(),
            unix_ms()
        ))
    }
}

fn reclaim_or_reject_lock(
    path: &Path,
    profile: &Path,
    replacement: &str,
) -> Result<ProfileLock, TransportError> {
    #[cfg(windows)]
    {
        let _mutex = lock_recovery::RecoveryMutex::acquire(profile)?;
        let existing = fs::read_to_string(path).map_err(|error| {
            TransportError::StaleState(format!("read existing harness lock: {error}"))
        })?;
        let record = lock_recovery::parse_lock(&existing)?;
        match lock_recovery::owner_state(&record)? {
            lock_recovery::OwnerState::LiveSame | lock_recovery::OwnerState::LiveUnknown => {
                return Err(TransportError::StaleState(format!(
                    "existing harness lock belongs to live Chatarium process {}",
                    record.pid()
                )));
            }
            lock_recovery::OwnerState::Dead | lock_recovery::OwnerState::Reused => {}
        }
        if lock_recovery::profile_singleton_present(profile)? {
            return Err(TransportError::StaleState(
                "existing harness lock is orphaned but the dedicated Edge profile is still active"
                    .to_owned(),
            ));
        }
        if fs::read_to_string(path).ok().as_deref() != Some(existing.as_str()) {
            return Err(TransportError::StaleState(
                "harness lock changed during stale-lock recovery".to_owned(),
            ));
        }
        fs::remove_file(path).map_err(|error| {
            TransportError::StaleState(format!("reclaim stale harness lock: {error}"))
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                TransportError::StaleState(format!("install recovered harness lock: {error}"))
            })?;
        let write_result = file
            .write_all(replacement.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(error) = write_result {
            drop(file);
            if fs::read_to_string(path).ok().as_deref() == Some(replacement) {
                let _ = fs::remove_file(path);
            }
            return Err(TransportError::Process(format!(
                "write recovered harness lock: {error}"
            )));
        }
        return Ok(ProfileLock {
            path: path.to_owned(),
            token: replacement.to_owned(),
            released: false,
            retained_on_drop: false,
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (path, profile, replacement);
        Err(TransportError::StaleState(
            "existing harness lock ownership could not be established safely".to_owned(),
        ))
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

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn ensure_profile_directories(config: &EdgeLaunchConfig) -> Result<(), TransportError> {
    let local = fs::canonicalize(&config.local_app_data)
        .map_err(|error| TransportError::UnsafeProfile(format!("resolve LOCALAPPDATA: {error}")))?;
    let expected = local
        .join("Chatarium")
        .join("capture-browser")
        .join("edge-profile");
    if expected != config.profile {
        return Err(TransportError::UnsafeProfile(
            "derived profile path changed after validation".to_owned(),
        ));
    }
    let paths = [
        local.join("Chatarium"),
        local.join("Chatarium").join("capture-browser"),
        expected.clone(),
    ];
    for path in paths {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let actual = fs::canonicalize(&path).map_err(|error| {
                    TransportError::UnsafeProfile(format!(
                        "resolve profile component {}: {error}",
                        path.display()
                    ))
                })?;
                if crate::normalized_windows_path(&actual).map_err(TransportError::UnsafeProfile)?
                    != crate::normalized_windows_path(&path)
                        .map_err(TransportError::UnsafeProfile)?
                {
                    return Err(TransportError::UnsafeProfile(format!(
                        "profile component {} resolves outside its dedicated path",
                        path.display()
                    )));
                }
                if !actual.is_dir() {
                    return Err(TransportError::UnsafeProfile(format!(
                        "profile component {} is not a directory",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|error| {
                    TransportError::Process(format!(
                        "create dedicated profile directory {}: {error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(TransportError::UnsafeProfile(format!(
                    "inspect profile component {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn ensure_active_port_absent(path: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(TransportError::StaleState(format!(
            "{} already exists; refusing to trust a stale debugging endpoint",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TransportError::Process(format!(
            "inspect DevToolsActivePort {}: {error}",
            path.display()
        ))),
    }
}

fn wait_for_debugging_endpoint(
    process: &mut EdgeProcess,
    active_port_file: &Path,
    deadline: Instant,
) -> Result<DevToolsPort, TransportError> {
    loop {
        if let Some(exit_code) = process
            .child
            .as_mut()
            .expect("launched process")
            .try_wait()
            .map_err(TransportError::Process)?
        {
            return Err(TransportError::Process(format!(
                "Edge exited with code {exit_code} before debugging endpoint discovery"
            )));
        }
        match fs::read_to_string(active_port_file) {
            Ok(contents) => return DevToolsPort::from_active_port(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TransportError::Process(format!(
                    "read DevToolsActivePort: {error}"
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(TransportError::Timeout { command_id: None });
        }
        thread::sleep(DEVTOOLS_RETRY_DELAY.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn wait_for_devtools_readiness(
    process: &mut EdgeProcess,
    transport: &mut DevToolsBrowserTransport,
    port: DevToolsPort,
    run: &mut CaptureRun,
    deadline: Instant,
    evidence: &mut StartupEvidence,
) -> Result<(), TransportError> {
    run.append_event(
        "devtools_readiness_started",
        json!({
            "port": port.port(),
            "candidate_addresses": ["127.0.0.1", "::1"],
        }),
    )
    .map_err(TransportError::Journal)?;

    let mut attempts = 0u32;
    let mut last_error: Option<String> = None;
    loop {
        for family in [LoopbackAddressFamily::Ipv4, LoopbackAddressFamily::Ipv6] {
            if let Some(exit_code) = process
                .child
                .as_mut()
                .expect("launched process")
                .try_wait()
                .map_err(TransportError::Process)?
            {
                let error = TransportError::Process(format!(
                    "Edge exited with code {exit_code} during DevTools readiness"
                ));
                journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
                return Err(error);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            attempts = attempts.saturating_add(1);
            let connection = evidence.connection_mut(family);
            connection.attempts = connection.attempts.saturating_add(1);
            let endpoint = DevToolsEndpoint::loopback(port, family);
            match transport.probe_browser_version_at(
                endpoint,
                run,
                remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT),
            ) {
                Ok(_) => {
                    evidence.connection_mut(family).succeeded = true;
                    evidence.connection_mut(family).last_result = Some("connected".to_owned());
                    run.append_event(
                        "devtools_readiness_succeeded",
                        json!({
                            "attempts": attempts,
                            "address": endpoint.address().to_string(),
                            "address_family": endpoint.family().as_str(),
                            "last_transient_error": last_error,
                        }),
                    )
                    .map_err(TransportError::Journal)?;
                    return Ok(());
                }
                Err(error @ TransportError::ReadinessTransient(_)) => {
                    last_error = Some(error.to_string());
                    evidence.connection_mut(family).last_result = Some(error.to_string());
                }
                Err(error) => {
                    evidence.connection_mut(family).last_result = Some(error.to_string());
                    journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
                    return Err(error);
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let error = TransportError::ReadinessTimeout {
                attempts,
                last_error: last_error.clone().unwrap_or_else(|| {
                    "startup deadline expired before a DevTools request could be attempted"
                        .to_owned()
                }),
            };
            journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
            return Err(error);
        }
        thread::sleep(DEVTOOLS_RETRY_DELAY.min(remaining));
    }
}

fn journal_readiness_failure(
    run: &mut CaptureRun,
    attempts: u32,
    error: &TransportError,
    last_transient_error: Option<&str>,
) -> Result<(), TransportError> {
    run.append_event(
        "devtools_readiness_failed",
        json!({
            "attempts": attempts,
            "error": error.to_string(),
            "last_transient_error": last_transient_error,
        }),
    )
    .map(|_| ())
    .map_err(TransportError::Journal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_experiment;
    use crate::diagnostics::relation_from_parent_map;
    #[cfg(windows)]
    use crate::pipe::PipeCdpBrowserTransport;
    #[cfg(windows)]
    use crate::transport::CdpMessageChannel;
    use crate::transport::{DevToolsResource, WebSocketConnection};
    use serde_json::Value;
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[cfg(windows)]
    #[test]
    fn windows_read_only_smoke_arguments_select_pipe_and_keep_about_blank() {
        let arguments = pipe_smoke_arguments(Path::new("C:\\local\\capture profile"));
        assert!(arguments.iter().any(|arg| arg == "about:blank"));
        assert!(
            arguments
                .iter()
                .any(|arg| arg.starts_with("--user-data-dir="))
        );
        assert!(arguments.iter().any(|arg| arg == "--no-first-run"));
        assert!(
            arguments
                .iter()
                .any(|arg| arg == "--no-default-browser-check")
        );
        assert!(arguments.iter().any(|arg| arg == "--incognito"));
        assert!(
            !arguments
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port"))
        );
        assert!(
            !arguments
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-address"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_profile_init_arguments_use_persistent_profile_and_exact_chatgpt_url() {
        let profile =
            Path::new(r"C:\Users\example\AppData\Local\Chatarium\capture-browser\edge-profile");
        let arguments = pipe_profile_arguments(profile, crate::init::CHATGPT_START_URL, false);
        assert_eq!(
            arguments.last().map(String::as_str),
            Some("https://chatgpt.com/")
        );
        assert!(
            arguments
                .iter()
                .any(|arg| arg == &format!("--user-data-dir={}", profile.display()))
        );
        assert!(!arguments.iter().any(|arg| arg == "--incognito"));
        assert!(
            !arguments
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port"))
        );
        assert!(
            !arguments
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-address"))
        );
    }

    #[test]
    fn plain_auth_arguments_have_only_the_profile_and_exact_start_url() {
        let profile =
            Path::new(r"C:\Users\example\AppData\Local\Chatarium\capture-browser\edge-profile");
        let arguments = plain_profile_arguments(profile, crate::init::CHATGPT_START_URL);
        assert_eq!(
            arguments,
            [
                format!("--user-data-dir={}", profile.display()),
                "https://chatgpt.com/".to_owned(),
            ]
        );
        assert!(
            !arguments
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-"))
        );
        assert!(!arguments.iter().any(|arg| arg == "--incognito"));
    }

    struct ScriptedDiagnostics {
        snapshots: Mutex<VecDeque<Result<Vec<ListenerRecord>, String>>>,
        policy: RemoteDebuggingPolicy,
        parent_map: HashMap<u32, Option<u32>>,
        requested_ports: Mutex<Vec<u16>>,
    }

    struct ScriptedDiagnosticEventWriter {
        failing_stage: &'static str,
    }

    struct FailedInitShutdownJournalWriter;

    impl DiagnosticEventWriter for ScriptedDiagnosticEventWriter {
        fn append(
            &self,
            run: &mut CaptureRun,
            kind: &str,
            payload: serde_json::Value,
        ) -> Result<(), String> {
            if kind == "devtools_os_diagnostics" && payload["stage"] == self.failing_stage {
                return Err(format!(
                    "injected {0} diagnostic append failure",
                    self.failing_stage
                ));
            }
            run.append_event(kind, payload).map(|_| ())
        }
    }

    impl DiagnosticEventWriter for FailedInitShutdownJournalWriter {
        fn append(
            &self,
            _run: &mut CaptureRun,
            kind: &str,
            _payload: serde_json::Value,
        ) -> Result<(), String> {
            Err(format!("injected {kind} append failure"))
        }
    }

    impl ScriptedDiagnostics {
        fn new(
            snapshots: impl IntoIterator<Item = Result<Vec<ListenerRecord>, String>>,
            policy: RemoteDebuggingPolicy,
            parent_map: HashMap<u32, Option<u32>>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into_iter().collect()),
                policy,
                parent_map,
                requested_ports: Mutex::new(Vec::new()),
            }
        }
    }

    impl EdgeDiagnostics for ScriptedDiagnostics {
        fn listeners(&self, port: u16) -> Result<Vec<ListenerRecord>, String> {
            self.requested_ports.lock().unwrap().push(port);
            self.snapshots
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        fn remote_debugging_policy(&self) -> RemoteDebuggingPolicy {
            self.policy.clone()
        }

        fn owner_relation(&self, owner_pid: u32, launched_pid: u32) -> OwnerRelation {
            relation_from_parent_map(owner_pid, launched_pid, &self.parent_map)
        }
    }

    fn no_policy() -> RemoteDebuggingPolicy {
        RemoteDebuggingPolicy {
            machine: PolicyState::NotConfigured,
            user: PolicyState::NotConfigured,
        }
    }

    fn listener(address: &str, port: u16, pid: Option<u32>) -> ListenerRecord {
        ListenerRecord {
            local_address: address.parse().unwrap(),
            local_port: port,
            state: "LISTEN".to_owned(),
            owning_pid: pid,
        }
    }

    struct FakeChild {
        id: u32,
        exited: bool,
        exit_code: i32,
        exit_signal: Option<Arc<std::sync::atomic::AtomicBool>>,
        kills: Arc<Mutex<usize>>,
        waits: Arc<Mutex<usize>>,
        fail_kill: bool,
    }

    struct ScriptedPlainWindowObserver {
        visible: Arc<Mutex<VecDeque<bool>>>,
        tree_alive: bool,
    }

    impl PlainAuthWindowObserver for ScriptedPlainWindowObserver {
        fn has_visible_owned_window(&self, _child: &dyn ManagedEdgeChild) -> Result<bool, String> {
            Ok(self.visible.lock().unwrap().pop_front().unwrap_or(false))
        }
        fn owned_process_tree_alive(&self, _child: &dyn ManagedEdgeChild) -> Result<bool, String> {
            Ok(self.tree_alive)
        }
    }

    #[cfg(windows)]
    #[derive(Clone, Copy)]
    enum BrowserCloseScript {
        ResponseAndExit,
        LostResponseAndExit,
        ResponseWithoutExit,
        BrokenBeforeDispatch,
    }

    #[cfg(windows)]
    struct BrowserCloseChannel {
        script: BrowserCloseScript,
        signal: Arc<std::sync::atomic::AtomicBool>,
        outgoing: Arc<Mutex<Vec<String>>>,
        operations: Arc<Mutex<Vec<&'static str>>>,
    }

    #[cfg(windows)]
    impl CdpMessageChannel for BrowserCloseChannel {
        fn send_message(&mut self, text: &str) -> Result<(), TransportError> {
            let command: Value = serde_json::from_str(text).unwrap();
            assert_eq!(command["method"], "Browser.close");
            self.operations.lock().unwrap().push("dispatch");
            self.outgoing.lock().unwrap().push(text.to_owned());
            if matches!(
                self.script,
                BrowserCloseScript::ResponseAndExit | BrowserCloseScript::LostResponseAndExit
            ) {
                self.signal.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }

        fn receive_message(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<String>, TransportError> {
            self.operations.lock().unwrap().push("receive");
            match self.script {
                BrowserCloseScript::ResponseAndExit | BrowserCloseScript::ResponseWithoutExit => {
                    let command: Value =
                        serde_json::from_str(self.outgoing.lock().unwrap().last().unwrap())
                            .unwrap();
                    Ok(Some(json!({"id": command["id"], "result": {}}).to_string()))
                }
                BrowserCloseScript::LostResponseAndExit => Err(TransportError::Eof {
                    unterminated_message: false,
                }),
                BrowserCloseScript::BrokenBeforeDispatch => unreachable!(),
            }
        }

        fn close(&mut self) -> Result<(), TransportError> {
            self.operations.lock().unwrap().push("transport_close");
            Ok(())
        }

        fn is_closed(&self) -> bool {
            matches!(self.script, BrowserCloseScript::BrokenBeforeDispatch)
        }
    }

    struct FakeSpawner {
        active_port_contents: String,
        arguments: Arc<Mutex<Vec<String>>>,
        kills: Arc<Mutex<usize>>,
        waits: Arc<Mutex<usize>>,
    }

    impl EdgeProcessSpawner for FakeSpawner {
        fn spawn(
            &self,
            _executable: &Path,
            arguments: &[String],
        ) -> Result<Box<dyn ManagedEdgeChild>, String> {
            *self.arguments.lock().unwrap() = arguments.to_vec();
            let profile = arguments
                .iter()
                .find_map(|argument| argument.strip_prefix("--user-data-dir="))
                .ok_or_else(|| "missing user data directory".to_owned())?;
            fs::write(
                Path::new(profile).join(ACTIVE_PORT_FILE),
                &self.active_port_contents,
            )
            .map_err(|error| error.to_string())?;
            Ok(Box::new(FakeChild {
                id: 4321,
                exited: false,
                exit_code: 0,
                exit_signal: None,
                kills: self.kills.clone(),
                waits: self.waits.clone(),
                fail_kill: false,
            }))
        }
    }

    struct FakeHttp;

    impl DevToolsHttp for FakeHttp {
        fn get_json(
            &self,
            _endpoint: DevToolsEndpoint,
            resource: DevToolsResource,
        ) -> Result<Value, TransportError> {
            Ok(match resource {
                DevToolsResource::Version => serde_json::json!({
                    "Browser":"Microsoft Edge/test",
                    "Protocol-Version":"1.3",
                    "webSocketDebuggerUrl":"ws://127.0.0.1:9444/devtools/browser/test"
                }),
                DevToolsResource::Targets => serde_json::json!([{
                    "id":"diagnostic-page",
                    "type":"page",
                    "title":"",
                    "url":"about:blank",
                    "webSocketDebuggerUrl":"ws://127.0.0.1:9444/devtools/page/diagnostic-page"
                }]),
            })
        }
    }

    struct SequenceHttp {
        version_results: Mutex<VecDeque<Result<Value, TransportError>>>,
        attempts: Arc<Mutex<usize>>,
        exit_signal: Option<Arc<std::sync::atomic::AtomicBool>>,
        endpoints: Arc<Mutex<Vec<DevToolsEndpoint>>>,
    }

    impl SequenceHttp {
        fn new(version_results: impl IntoIterator<Item = Result<Value, TransportError>>) -> Self {
            Self {
                version_results: Mutex::new(version_results.into_iter().collect()),
                attempts: Arc::new(Mutex::new(0)),
                exit_signal: None,
                endpoints: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn always_transient() -> Self {
            Self::new(std::iter::empty::<Result<Value, TransportError>>())
        }
    }

    impl DevToolsHttp for SequenceHttp {
        fn get_json(
            &self,
            endpoint: DevToolsEndpoint,
            resource: DevToolsResource,
        ) -> Result<Value, TransportError> {
            self.endpoints.lock().unwrap().push(endpoint);
            if resource == DevToolsResource::Targets {
                return Ok(serde_json::json!([]));
            }
            let attempt = {
                let mut attempts = self.attempts.lock().unwrap();
                *attempts += 1;
                *attempts
            };
            let result = self
                .version_results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Err(TransportError::ReadinessTransient(
                        "connection refused (os error 10061)".to_owned(),
                    ))
                });
            if attempt == 1 {
                if let Some(signal) = &self.exit_signal {
                    signal.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            result
        }
    }

    struct ExitingSpawner {
        signal: Arc<std::sync::atomic::AtomicBool>,
        kills: Arc<Mutex<usize>>,
        waits: Arc<Mutex<usize>>,
    }

    impl EdgeProcessSpawner for ExitingSpawner {
        fn spawn(
            &self,
            _executable: &Path,
            arguments: &[String],
        ) -> Result<Box<dyn ManagedEdgeChild>, String> {
            let profile = arguments
                .iter()
                .find_map(|argument| argument.strip_prefix("--user-data-dir="))
                .ok_or_else(|| "missing user data directory".to_owned())?;
            fs::write(
                Path::new(profile).join(ACTIVE_PORT_FILE),
                "9444\n/devtools/browser/test",
            )
            .map_err(|error| error.to_string())?;
            Ok(Box::new(FakeChild {
                id: 4322,
                exited: false,
                exit_code: 23,
                exit_signal: Some(self.signal.clone()),
                kills: self.kills.clone(),
                waits: self.waits.clone(),
                fail_kill: false,
            }))
        }
    }

    fn valid_version() -> Value {
        serde_json::json!({
            "Browser":"Microsoft Edge/test",
            "Protocol-Version":"1.3",
            "webSocketDebuggerUrl":"ws://127.0.0.1:9444/devtools/browser/test"
        })
    }

    struct FakeSocket;

    impl WebSocketConnection for FakeSocket {
        fn send_text(&mut self, _text: &str) -> Result<(), TransportError> {
            Ok(())
        }
        fn receive_text(&mut self, _timeout: Duration) -> Result<Option<String>, TransportError> {
            Ok(None)
        }
        fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    struct FakeConnector;

    impl WebSocketConnector for FakeConnector {
        fn connect(
            &self,
            _endpoint: &url::Url,
            _selected_endpoint: DevToolsEndpoint,
        ) -> Result<Box<dyn WebSocketConnection>, TransportError> {
            Ok(Box::new(FakeSocket))
        }
    }

    impl ManagedEdgeChild for FakeChild {
        fn id(&self) -> u32 {
            self.id
        }
        fn try_wait(&mut self) -> Result<Option<i32>, String> {
            if self
                .exit_signal
                .as_ref()
                .is_some_and(|signal| signal.load(std::sync::atomic::Ordering::SeqCst))
            {
                self.exited = true;
            }
            Ok(self.exited.then_some(self.exit_code))
        }
        fn kill(&mut self) -> Result<(), String> {
            *self.kills.lock().unwrap() += 1;
            if self.fail_kill {
                return Err("injected taskkill failure".to_owned());
            }
            self.exited = true;
            Ok(())
        }
        fn wait(&mut self) -> Result<i32, String> {
            *self.waits.lock().unwrap() += 1;
            self.exited = true;
            Ok(self.exit_code)
        }
        fn owned_processes_alive(&self) -> Result<bool, String> {
            Ok(!self.exited)
        }
    }

    #[test]
    fn plain_auth_waits_for_owned_window_close_before_killing_a_lingering_edge_process() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let child = Box::new(FakeChild {
            id: 4321,
            exited: false,
            exit_code: 0,
            exit_signal: None,
            kills: kills.clone(),
            waits,
            fail_kill: false,
        });
        let process = EdgeProcess::new(child, lock);
        let mut launched = LaunchedPlainEdge::with_observer(
            process,
            lock_path.clone(),
            Box::new(ScriptedPlainWindowObserver {
                visible: Arc::new(Mutex::new(VecDeque::from([true, true, false]))),
                tree_alive: false,
            }),
            Duration::from_millis(100),
            Duration::from_millis(1),
        );
        let mut run = CaptureRun::create_diagnostic(&base.join("runs"), "plain-auth-test").unwrap();
        run.start().unwrap();

        launched.wait_for_window_close(&mut run).unwrap();
        assert_eq!(
            *kills.lock().unwrap(),
            0,
            "visible auth window must not be killed"
        );
        launched.shutdown_after_window_close(&mut run).unwrap();

        assert_eq!(
            *kills.lock().unwrap(),
            1,
            "lingering owned Edge tree is cleaned only after close"
        );
        assert!(!lock_path.exists());
        assert!(launched.phase_boundary_released().unwrap());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn plain_auth_retains_profile_lock_when_root_exits_but_job_members_remain() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let process = EdgeProcess::new(
            Box::new(FakeChild {
                id: 4322,
                exited: true,
                exit_code: 7,
                exit_signal: None,
                kills,
                waits,
                fail_kill: false,
            }),
            lock,
        );
        let mut launched = LaunchedPlainEdge::with_observer(
            process,
            lock_path.clone(),
            Box::new(ScriptedPlainWindowObserver {
                visible: Arc::new(Mutex::new(VecDeque::new())),
                tree_alive: true,
            }),
            Duration::from_millis(100),
            Duration::from_millis(1),
        );
        let mut run = CaptureRun::create_diagnostic(&base.join("runs"), "root-exit-test").unwrap();
        run.start().unwrap();

        let error = launched.wait_for_window_close(&mut run).unwrap_err();
        assert!(error.to_string().contains("owned descendants remain"));
        assert!(
            lock_path.exists(),
            "uncertain job membership must retain the profile lock"
        );
        assert!(!launched.phase_boundary_released().unwrap());
        launched.preserve_active_process();
        drop(launched);
        assert!(lock_path.exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn plain_auth_never_terminates_before_owned_window_close() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let process = EdgeProcess::new(
            Box::new(FakeChild {
                id: 4323,
                exited: false,
                exit_code: 0,
                exit_signal: None,
                kills: kills.clone(),
                waits,
                fail_kill: false,
            }),
            lock,
        );
        let mut launched = LaunchedPlainEdge::with_observer(
            process,
            lock_path.clone(),
            Box::new(ScriptedPlainWindowObserver {
                visible: Arc::new(Mutex::new(VecDeque::from([true]))),
                tree_alive: true,
            }),
            Duration::from_millis(100),
            Duration::from_millis(1),
        );
        let mut run =
            CaptureRun::create_diagnostic(&base.join("runs"), "close-guard-test").unwrap();
        run.start().unwrap();

        assert!(launched.shutdown_after_window_close(&mut run).is_err());
        assert_eq!(*kills.lock().unwrap(), 0);
        assert!(lock_path.exists());
        drop(launched);
        assert_eq!(*kills.lock().unwrap(), 0);
        assert!(lock_path.exists());
        let _ = fs::remove_dir_all(base);
    }

    fn temp_dir() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-edge-process-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn stale_profile_lock_fails_without_overwriting_or_removing_it() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let path = base.join(LOCK_FILE);
        fs::write(&path, "stale lock evidence").unwrap();
        assert!(matches!(
            ProfileLock::acquire(path.clone()),
            Err(TransportError::StaleState(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "stale lock evidence");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn owned_edge_process_kills_waits_and_releases_lock() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let child = FakeChild {
            id: 123,
            exited: false,
            exit_code: 0,
            exit_signal: None,
            kills: kills.clone(),
            waits: waits.clone(),
            fail_kill: false,
        };
        let mut process = EdgeProcess::new(Box::new(child), lock);
        let code = process.shutdown().unwrap();
        assert_eq!(code, 0);
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(!lock_path.exists());
        assert_eq!(process.shutdown().unwrap(), 0);
        drop(process);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn already_exited_edge_process_is_waited_only_once_and_lock_is_released() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let child = FakeChild {
            id: 124,
            exited: true,
            exit_code: 0,
            exit_signal: None,
            kills: kills.clone(),
            waits: waits.clone(),
            fail_kill: false,
        };
        let mut process = EdgeProcess::new(Box::new(child), lock);
        assert_eq!(process.shutdown().unwrap(), 0);
        assert_eq!(*kills.lock().unwrap(), 0);
        assert_eq!(*waits.lock().unwrap(), 0);
        assert!(!lock_path.exists());
        drop(process);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn profile_path_is_derived_from_dedicated_local_app_data() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let profile = local.join("Chatarium/capture-browser/edge-profile");
        let config = EdgeLaunchConfig {
            local_app_data: local.clone(),
            executable: local.join("msedge.exe"),
            profile: profile.clone(),
            capture_root: local.join("Chatarium/capture-browser"),
            startup_timeout: Duration::from_millis(1),
        };
        assert!(
            validate_dedicated_capture_profile(&config.profile, &config.local_app_data).is_ok()
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn existing_dedicated_profile_is_reused_without_rewriting_its_contents() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let config = config_under(&local);
        fs::create_dir_all(&config.profile).unwrap();
        let marker = config.profile.join("existing-site-data.marker");
        fs::write(&marker, b"existing Edge-managed profile state").unwrap();

        ensure_profile_directories(&config).unwrap();

        assert_eq!(
            fs::read(&marker).unwrap(),
            b"existing Edge-managed profile state"
        );
        assert!(config.profile.is_dir());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn fake_process_can_emit_active_port_but_no_live_browser_is_required() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let profile = base.join("profile");
        fs::create_dir_all(&profile).unwrap();
        let active_port = profile.join(ACTIVE_PORT_FILE);
        fs::write(&active_port, "not-a-port\n/devtools/browser/stale").unwrap();
        let error =
            DevToolsPort::from_active_port(&fs::read_to_string(&active_port).unwrap()).unwrap_err();
        assert!(matches!(error, TransportError::InvalidEndpoint(_)));
        let _ = fs::remove_dir_all(base);
    }

    fn config_under(local: &Path) -> EdgeLaunchConfig {
        EdgeLaunchConfig {
            local_app_data: local.to_path_buf(),
            executable: local.join("EdgeStub.exe"),
            profile: local.join("Chatarium/capture-browser/edge-profile"),
            capture_root: local.join("Chatarium/capture-browser"),
            startup_timeout: Duration::from_secs(1),
        }
    }

    fn diagnostic_run_under(base: &Path) -> (CaptureRun, PathBuf) {
        let run_base = base.join("runs");
        let run = CaptureRun::create_diagnostic(&run_base, "smoke-edge").unwrap();
        (run, run_base)
    }

    #[cfg(windows)]
    fn launched_init_for_test(
        base: &Path,
        script: BrowserCloseScript,
        fail_kill: bool,
    ) -> (
        LaunchedEdge,
        CaptureRun,
        PathBuf,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<&'static str>>>,
        Arc<Mutex<usize>>,
    ) {
        fs::create_dir_all(base).unwrap();
        let profile = base.join("persistent-profile");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("profile-marker"), b"preserve session state").unwrap();
        let lock_path = base.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path.clone()).unwrap();
        let signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let child = FakeChild {
            id: 7331,
            exited: false,
            exit_code: 0,
            exit_signal: Some(signal.clone()),
            kills: kills.clone(),
            waits,
            fail_kill,
        };
        let outgoing = Arc::new(Mutex::new(Vec::new()));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let transport = PipeCdpBrowserTransport::new(Box::new(BrowserCloseChannel {
            script,
            signal,
            outgoing: outgoing.clone(),
            operations: operations.clone(),
        }));
        let launched = LaunchedEdge {
            process: EdgeProcess::new(Box::new(child), lock),
            lock_file: lock_path,
            active_port_file: profile.join(ACTIVE_PORT_FILE),
            transport: Some(EdgeBrowserTransport::Pipe(transport)),
            active_port_required: false,
        };
        let (mut run, run_base) = diagnostic_run_under(base);
        run.start().unwrap();
        (launched, run, run_base, outgoing, operations, kills)
    }

    #[cfg(windows)]
    fn event_payload<'a>(run: &'a CaptureRun, kind: &str) -> &'a Value {
        &run.events()
            .iter()
            .find(|event| event.kind == kind)
            .unwrap_or_else(|| panic!("missing event {kind}"))
            .payload
    }

    #[cfg(windows)]
    #[test]
    fn init_browser_close_precedes_pipe_destruction_and_natural_exit_avoids_force_kill() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, outgoing, operations, kills) =
            launched_init_for_test(&base, BrowserCloseScript::ResponseAndExit, false);
        browser
            .shutdown_for_profile_init_with_timeout(&mut run, Duration::from_millis(50))
            .unwrap();

        assert_eq!(outgoing.lock().unwrap().len(), 1);
        let command: Value = serde_json::from_str(&outgoing.lock().unwrap()[0]).unwrap();
        assert_eq!(command["method"], "Browser.close");
        let operations = operations.lock().unwrap().clone();
        assert_eq!(operations, ["dispatch", "receive", "transport_close"]);
        assert_eq!(*kills.lock().unwrap(), 0);
        assert_eq!(
            event_payload(&run, "init_browser_natural_exit_observed")["browser_close_response"],
            "received"
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_kill_used"],
            false
        );
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        assert!(!base.join(LOCK_FILE).exists());
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[cfg(windows)]
    #[test]
    fn browser_close_eof_is_resolved_by_natural_owned_process_exit() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, outgoing, operations, kills) =
            launched_init_for_test(&base, BrowserCloseScript::LostResponseAndExit, false);
        browser
            .shutdown_for_profile_init_with_timeout(&mut run, Duration::from_millis(50))
            .unwrap();

        assert_eq!(outgoing.lock().unwrap().len(), 1);
        assert_eq!(operations.lock().unwrap().last(), Some(&"transport_close"));
        assert_eq!(*kills.lock().unwrap(), 0);
        assert_eq!(
            event_payload(&run, "init_browser_natural_exit_observed")["browser_close_response"],
            "missing_or_disconnect"
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["natural_exit"],
            true
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_kill_used"],
            false
        );
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[cfg(windows)]
    #[test]
    fn graceful_close_timeout_uses_only_the_owned_pid_fallback_and_preserves_profile() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, _, _, kills) =
            launched_init_for_test(&base, BrowserCloseScript::ResponseWithoutExit, false);
        browser
            .shutdown_for_profile_init_with_timeout(&mut run, Duration::from_millis(10))
            .unwrap();

        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(
            event_payload(&run, "init_browser_force_kill_fallback_started")["owned_pid"],
            7331
        );
        assert_eq!(
            event_payload(&run, "init_browser_force_kill_fallback_result")["result"],
            "succeeded"
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_kill_used"],
            true
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_fallback_attempted"],
            true
        );
        assert!(
            run.events()
                .iter()
                .any(|event| event.kind == "init_browser_graceful_close_timed_out")
        );
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[cfg(windows)]
    #[test]
    fn broken_pipe_skips_browser_close_dispatch_and_still_uses_owned_cleanup() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, outgoing, _, kills) =
            launched_init_for_test(&base, BrowserCloseScript::BrokenBeforeDispatch, false);
        browser
            .shutdown_for_profile_init_with_timeout(&mut run, Duration::from_millis(10))
            .unwrap();

        assert!(outgoing.lock().unwrap().is_empty());
        assert_eq!(
            event_payload(&run, "init_browser_close_dispatch_result")["result"],
            "could_not_attempt_pipe_already_closed"
        );
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_kill_used"],
            true
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_fallback_attempted"],
            true
        );
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[cfg(windows)]
    #[test]
    fn failed_force_kill_is_distinct_and_keeps_cleanup_unverified() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, _, _, kills) =
            launched_init_for_test(&base, BrowserCloseScript::ResponseWithoutExit, true);
        let failure = browser
            .shutdown_for_profile_init_with_timeout(&mut run, Duration::from_millis(10))
            .unwrap_err();

        assert_eq!(*kills.lock().unwrap(), 1);
        assert!(failure.to_string().contains("injected taskkill failure"));
        assert_eq!(
            event_payload(&run, "init_browser_force_kill_fallback_result")["result"],
            "failed"
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["cleanup_succeeded"],
            false
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_fallback_attempted"],
            true
        );
        assert_eq!(
            event_payload(&run, "browser_shutdown_cleanup")["forced_kill_used"],
            false
        );
        assert!(base.join(LOCK_FILE).exists());
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[cfg(windows)]
    #[test]
    fn graceful_shutdown_journal_failure_does_not_skip_exit_pipe_or_lock_cleanup() {
        let base = temp_dir();
        let (mut browser, mut run, run_base, outgoing, _, kills) =
            launched_init_for_test(&base, BrowserCloseScript::ResponseAndExit, false);

        let failure = browser
            .shutdown_for_profile_init_with_writer_and_timeout(
                &mut run,
                Duration::from_millis(50),
                &FailedInitShutdownJournalWriter,
            )
            .unwrap_err();

        assert!(matches!(
            failure,
            TransportError::DiagnosticJournalFailure {
                primary_failure: None,
                journal_failure: _
            }
        ));
        assert_eq!(outgoing.lock().unwrap().len(), 1);
        assert_eq!(*kills.lock().unwrap(), 0);
        assert!(!base.join(LOCK_FILE).exists());
        assert_eq!(
            fs::read(base.join("persistent-profile/profile-marker")).unwrap(),
            b"preserve session state"
        );
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn readiness_retries_after_port_file_until_http_version_is_ready() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_secs(1);
        let attempts = Arc::new(Mutex::new(0));
        let mut http = SequenceHttp::new([
            Err(TransportError::ReadinessTransient(
                "connection timed out".to_owned(),
            )),
            Ok(valid_version()),
        ]);
        http.attempts = attempts.clone();
        let endpoints = http.endpoints.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let (mut run, run_base) = diagnostic_run_under(&base);

        let mut browser = LaunchedEdge::launch_with_options(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
        )
        .unwrap();
        assert_eq!(*attempts.lock().unwrap(), 2);
        assert_eq!(
            *endpoints.lock().unwrap(),
            vec![
                DevToolsEndpoint::loopback(
                    DevToolsPort::new(9444).unwrap(),
                    LoopbackAddressFamily::Ipv4,
                ),
                DevToolsEndpoint::loopback(
                    DevToolsPort::new(9444).unwrap(),
                    LoopbackAddressFamily::Ipv6,
                ),
            ]
        );
        assert!(
            run.events()
                .iter()
                .any(|event| { event.kind == "devtools_readiness_started" })
        );
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_readiness_succeeded"
                && event.payload["attempts"] == 2
                && event.payload["address_family"] == "ipv6"
                && event.payload["last_transient_error"]
                    .as_str()
                    .is_some_and(|value| value.contains("connection timed out"))
        }));
        assert_eq!(
            run.events()
                .iter()
                .filter(|event| {
                    event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "initial"
                })
                .count(),
            1
        );
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "policy_preflight"
        }));
        browser.transport().list_targets(&mut run).unwrap();
        assert_eq!(
            endpoints.lock().unwrap().last().unwrap().family(),
            LoopbackAddressFamily::Ipv6
        );
        browser.shutdown(&mut run).unwrap();
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        drop(browser);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn transient_readiness_failures_stop_at_the_overall_startup_deadline_and_cleanup() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_millis(800);
        let http = SequenceHttp::always_transient();
        let attempts = http.attempts.clone();
        let endpoints = http.endpoints.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let (mut run, run_base) = diagnostic_run_under(&base);
        let started = Instant::now();
        let error = LaunchedEdge::launch_with_options(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
        )
        .err()
        .unwrap();
        let elapsed = started.elapsed();

        assert!(matches!(error, TransportError::ReadinessTimeout { .. }));
        assert!(error.to_string().contains("connection refused"));
        assert!(*attempts.lock().unwrap() >= 2);
        assert!(endpoints.lock().unwrap().iter().any(|endpoint| {
            endpoint.family() == LoopbackAddressFamily::Ipv4 && endpoint.port() == 9444
        }));
        assert!(endpoints.lock().unwrap().iter().any(|endpoint| {
            endpoint.family() == LoopbackAddressFamily::Ipv6 && endpoint.port() == 9444
        }));
        assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(
            !local
                .join("Chatarium/capture-browser")
                .join(LOCK_FILE)
                .exists()
        );
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_readiness_failed"
                && event.payload["attempts"] == *attempts.lock().unwrap()
                && event.payload["last_transient_error"]
                    .as_str()
                    .is_some_and(|value| value.contains("connection refused"))
        }));
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn listener_snapshots_report_owner_and_listener_disappearance_after_timeout() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_millis(600);
        let http = SequenceHttp::always_transient();
        let attempts = http.attempts.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let diagnostics = ScriptedDiagnostics::new(
            [
                Ok(vec![listener("127.0.0.1", 9444, Some(4321))]),
                Ok(Vec::new()),
            ],
            no_policy(),
            HashMap::new(),
        );
        let (mut run, run_base) = diagnostic_run_under(&base);

        let error = LaunchedEdge::launch_with_diagnostics(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
            &diagnostics,
            &DurableDiagnosticEventWriter,
        )
        .err()
        .unwrap();

        assert!(matches!(error, TransportError::ReadinessTimeout { .. }));
        assert!(*attempts.lock().unwrap() > 0);
        assert_eq!(
            *diagnostics.requested_ports.lock().unwrap(),
            vec![9444, 9444]
        );
        let events = run.events();
        let initial = events
            .iter()
            .find(|event| {
                event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "initial"
            })
            .unwrap();
        let final_event = events
            .iter()
            .find(|event| {
                event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "final"
            })
            .unwrap();
        assert_eq!(
            initial.payload["listener_snapshot"]["listeners"][0]["family"],
            "ipv4"
        );
        assert_eq!(
            initial.payload["listener_snapshot"]["listeners"][0]["owner_relation"],
            "same"
        );
        assert_eq!(
            final_event.payload["listener_snapshot"]["listeners"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn disabled_remote_debugging_policy_short_circuits_and_still_cleans_up() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let config = config_under(&local);
        let http = SequenceHttp::always_transient();
        let attempts = http.attempts.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let diagnostics = ScriptedDiagnostics::new(
            [],
            RemoteDebuggingPolicy {
                machine: PolicyState::Disabled,
                user: PolicyState::NotConfigured,
            },
            HashMap::new(),
        );
        let (mut run, run_base) = diagnostic_run_under(&base);

        let error = LaunchedEdge::launch_with_diagnostics(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
            &diagnostics,
            &DurableDiagnosticEventWriter,
        )
        .err()
        .unwrap();

        assert!(matches!(error, TransportError::RemoteDebuggingDisabled(_)));
        assert!(error.to_string().contains("disabled by policy"));
        assert_eq!(*attempts.lock().unwrap(), 0);
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(!config.capture_root.join(LOCK_FILE).exists());
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_os_diagnostics"
                && event.payload["stage"] == "policy_preflight"
                && event.payload["remote_debugging_allowed"]["summary"] == "disabled"
        }));
        assert!(diagnostics.requested_ports.lock().unwrap().is_empty());
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn initial_diagnostic_append_failure_stops_readiness_and_still_cleans_up() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let config = config_under(&local);
        let http = SequenceHttp::always_transient();
        let attempts = http.attempts.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let diagnostics = ScriptedDiagnostics::new(
            [Ok(Vec::new()), Ok(Vec::new())],
            no_policy(),
            HashMap::new(),
        );
        let writer = ScriptedDiagnosticEventWriter {
            failing_stage: "initial",
        };
        let (mut run, run_base) = diagnostic_run_under(&base);

        let error = LaunchedEdge::launch_with_diagnostics(
            config,
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
            &diagnostics,
            &writer,
        )
        .err()
        .unwrap();

        match error {
            TransportError::DiagnosticJournalFailure {
                primary_failure,
                journal_failure,
            } => {
                assert!(primary_failure.is_none());
                assert!(journal_failure.contains("initial diagnostic append failure"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(*attempts.lock().unwrap(), 0);
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "final"
        }));
        let cleanup = run
            .events()
            .iter()
            .find(|event| event.kind == "browser_shutdown_cleanup")
            .unwrap();
        assert_eq!(cleanup.payload["cleanup_succeeded"], true);
        assert_eq!(cleanup.payload["process_exited"], true);
        assert_eq!(cleanup.payload["harness_lock_absent"], true);
        assert_eq!(cleanup.payload["active_port_file_absent"], true);
        assert!(diagnostics.requested_ports.lock().unwrap().len() == 2);
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn final_diagnostic_append_failure_keeps_readiness_failure_primary_and_cleans_up() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_millis(80);
        let http = SequenceHttp::always_transient();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let diagnostics = ScriptedDiagnostics::new(
            [Ok(Vec::new()), Ok(Vec::new())],
            no_policy(),
            HashMap::new(),
        );
        let writer = ScriptedDiagnosticEventWriter {
            failing_stage: "final",
        };
        let (mut run, run_base) = diagnostic_run_under(&base);

        let error = LaunchedEdge::launch_with_diagnostics(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
            &diagnostics,
            &writer,
        )
        .err()
        .unwrap();

        match error {
            TransportError::DiagnosticJournalFailure {
                primary_failure: Some(primary),
                journal_failure,
            } => {
                assert!(primary.contains("DevTools readiness deadline expired"));
                assert!(journal_failure.contains("final diagnostic append failure"));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(!config.capture_root.join(LOCK_FILE).exists());
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        let cleanup = run
            .events()
            .iter()
            .find(|event| event.kind == "browser_shutdown_cleanup")
            .unwrap();
        assert_eq!(cleanup.payload["cleanup_succeeded"], true);
        assert_eq!(cleanup.payload["process_exited"], true);
        assert_eq!(cleanup.payload["harness_lock_absent"], true);
        assert_eq!(cleanup.payload["active_port_file_absent"], true);
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "initial"
        }));
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn listener_recording_supports_ipv4_ipv6_and_descendant_ownership() {
        let evidence = StartupEvidence {
            port: 9444,
            launched_pid: 4321,
            policy: no_policy(),
            initial: SnapshotEvidence {
                listeners: vec![
                    listener("127.0.0.1", 9444, Some(4321)),
                    listener("::1", 9444, Some(4321)),
                ],
                error: None,
            },
            final_snapshot: Some(SnapshotEvidence {
                listeners: vec![listener("::1", 9444, Some(7002))],
                error: None,
            }),
            connections: [
                ConnectionEvidence {
                    family: "ipv4",
                    attempts: 1,
                    last_result: Some("timed out".to_owned()),
                    succeeded: false,
                },
                ConnectionEvidence {
                    family: "ipv6",
                    attempts: 1,
                    last_result: Some("timed out".to_owned()),
                    succeeded: false,
                },
            ],
        };
        let diagnostics = ScriptedDiagnostics::new(
            [],
            no_policy(),
            HashMap::from([(7002, Some(7001)), (7001, Some(4321))]),
        );

        let initial = evidence.snapshot_value(&evidence.initial, &diagnostics);
        let final_snapshot =
            evidence.snapshot_value(evidence.final_snapshot.as_ref().unwrap(), &diagnostics);

        assert_eq!(initial["listeners"][0]["family"], "ipv4");
        assert_eq!(initial["listeners"][0]["owner_relation"], "same");
        assert_eq!(initial["listeners"][1]["family"], "ipv6");
        assert_eq!(initial["listeners"][1]["owner_relation"], "same");
        assert_eq!(final_snapshot["listeners"][0]["family"], "ipv6");
        assert_eq!(
            final_snapshot["listeners"][0]["owner_relation"],
            "descendant"
        );
        assert_eq!(
            evidence.connections[0].last_result.as_deref(),
            Some("timed out")
        );
    }

    #[test]
    fn edge_exit_during_readiness_fails_immediately_with_status_and_cleanup() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_secs(2);
        let signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut http = SequenceHttp::always_transient();
        http.exit_signal = Some(signal.clone());
        let attempts = http.attempts.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = ExitingSpawner {
            signal,
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let (mut run, run_base) = diagnostic_run_under(&base);
        let error = LaunchedEdge::launch_with_options(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
        )
        .err()
        .unwrap();

        assert!(matches!(error, TransportError::Process(_)));
        assert!(error.to_string().contains("exited with code 23"));
        assert_eq!(*attempts.lock().unwrap(), 1);
        assert_eq!(*kills.lock().unwrap(), 0);
        assert_eq!(*waits.lock().unwrap(), 0);
        assert!(
            !local
                .join("Chatarium/capture-browser")
                .join(LOCK_FILE)
                .exists()
        );
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_readiness_failed" && event.payload["attempts"] == 1
        }));
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn permanent_protocol_validation_failure_is_not_retried_and_cleanup_runs() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let mut config = config_under(&local);
        config.startup_timeout = Duration::from_secs(1);
        let mut invalid_version = valid_version();
        invalid_version["webSocketDebuggerUrl"] =
            serde_json::json!("ws://example.com/devtools/browser/test");
        let http = SequenceHttp::new([Ok(invalid_version)]);
        let attempts = http.attempts.clone();
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: Arc::new(Mutex::new(Vec::new())),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let (mut run, run_base) = diagnostic_run_under(&base);
        let error = LaunchedEdge::launch_with_options(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(http),
            Box::new(FakeConnector),
            true,
        )
        .err()
        .unwrap();

        assert!(matches!(error, TransportError::InvalidEndpoint(_)));
        assert_eq!(*attempts.lock().unwrap(), 1);
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);
        assert!(
            !local
                .join("Chatarium/capture-browser")
                .join(LOCK_FILE)
                .exists()
        );
        assert!(!config.profile.join(ACTIVE_PORT_FILE).exists());
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_readiness_failed" && event.payload["attempts"] == 1
        }));
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn mocked_read_only_edge_launch_discovery_attachment_and_cleanup_are_journaled() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let config = config_under(&local);
        let arguments = Arc::new(Mutex::new(Vec::new()));
        let kills = Arc::new(Mutex::new(0));
        let waits = Arc::new(Mutex::new(0));
        let spawner = FakeSpawner {
            active_port_contents: "9444\n/devtools/browser/test".to_owned(),
            arguments: arguments.clone(),
            kills: kills.clone(),
            waits: waits.clone(),
        };
        let (mut run, run_base) = {
            let experiment = canonical_experiment("C00-idle-load").unwrap();
            let run_base = base.join("runs");
            (
                CaptureRun::create(&run_base, &experiment).unwrap(),
                run_base,
            )
        };

        let diagnostics = ScriptedDiagnostics::new(
            [Ok(Vec::new())],
            RemoteDebuggingPolicy {
                machine: PolicyState::Enabled,
                user: PolicyState::NotConfigured,
            },
            HashMap::new(),
        );

        let mut browser = LaunchedEdge::launch_with_diagnostics(
            config,
            &mut run,
            &spawner,
            Box::new(FakeHttp),
            Box::new(FakeConnector),
            true,
            &diagnostics,
            &DurableDiagnosticEventWriter,
        )
        .unwrap();
        let discovered = browser.transport().list_targets(&mut run).unwrap();
        assert_eq!(discovered[0].id, "diagnostic-page");
        let mut session = browser
            .transport()
            .attach("diagnostic-page", &mut run)
            .unwrap();
        session.close().unwrap();
        browser.shutdown(&mut run).unwrap();

        let args = arguments.lock().unwrap().clone();
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_owned()));
        assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(args.contains(&"--incognito".to_owned()));
        assert!(args.contains(&"about:blank".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
        assert!(!args.iter().any(|argument| argument.contains("chatgpt.com")));
        assert_eq!(*kills.lock().unwrap(), 1);
        assert_eq!(*waits.lock().unwrap(), 1);

        let event_kinds = run
            .events()
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        for required in [
            "browser_process_started",
            "debugging_endpoint_discovered",
            "cdp_page_targets_discovered",
            "cdp_target_attached",
            "browser_shutdown_cleanup",
        ] {
            assert!(event_kinds.contains(&required), "missing {required}");
        }
        let readiness = run
            .events()
            .iter()
            .find(|event| event.kind == "devtools_readiness_succeeded")
            .unwrap();
        assert_eq!(readiness.payload["address"], "127.0.0.1");
        assert_eq!(readiness.payload["address_family"], "ipv4");
        drop(run);
        let lock_path = local.join("Chatarium/capture-browser").join(LOCK_FILE);
        assert!(!lock_path.exists());
        let active_port_file = local
            .join("Chatarium/capture-browser/edge-profile")
            .join(ACTIVE_PORT_FILE);
        assert!(!active_port_file.exists());
        drop(browser);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }

    #[test]
    fn stale_active_port_is_rejected_before_spawn_and_releases_its_temporary_lock() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let local = fs::canonicalize(&base).unwrap();
        let config = config_under(&local);
        ensure_profile_directories(&config).unwrap();
        let active_port = config.profile.join(ACTIVE_PORT_FILE);
        fs::write(&active_port, "9444\n/devtools/browser/stale").unwrap();
        let args = Arc::new(Mutex::new(Vec::new()));
        let spawner = FakeSpawner {
            active_port_contents: String::new(),
            arguments: args.clone(),
            kills: Arc::new(Mutex::new(0)),
            waits: Arc::new(Mutex::new(0)),
        };
        let (mut run, run_base) = {
            let experiment = canonical_experiment("C00-idle-load").unwrap();
            let run_base = base.join("runs");
            (
                CaptureRun::create(&run_base, &experiment).unwrap(),
                run_base,
            )
        };
        let error = LaunchedEdge::launch_with(
            config.clone(),
            &mut run,
            &spawner,
            Box::new(FakeHttp),
            Box::new(FakeConnector),
        )
        .err()
        .unwrap();
        assert!(matches!(error, TransportError::StaleState(_)));
        assert!(args.lock().unwrap().is_empty());
        assert!(!config.capture_root.join(LOCK_FILE).exists());
        drop(run);
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(run_base);
    }
}
