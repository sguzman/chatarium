//! Windows-first harness-owned Edge process lifecycle.

#[path = "edge/diagnostics.rs"]
mod diagnostics;
mod job_membership;
#[path = "edge/process.rs"]
mod process;
#[path = "edge/readiness.rs"]
mod readiness;
pub(super) use diagnostics::*;
pub(super) use process::*;
pub(super) use readiness::*;
#[path = "edge/launched.rs"]
mod launched;
#[path = "edge/plain.rs"]
mod plain;
#[cfg(test)]
#[path = "edge/tests.rs"]
mod tests;
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
