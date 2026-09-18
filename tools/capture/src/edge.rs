//! Windows-first harness-owned Edge process lifecycle.

use crate::run::CaptureRun;
use crate::transport::{
    BrowserTransport, DevToolsBrowserTransport, DevToolsEndpoint, DevToolsHttp,
    LoopbackDevToolsHttp, LoopbackWebSocketConnector, TransportError, WebSocketConnector,
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
const ACTIVE_PORT_FILE: &str = "DevToolsActivePort";
const LOCK_FILE: &str = "edge-profile.harness.lock";

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
    transport: DevToolsBrowserTransport,
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
                "debugging_address": "127.0.0.1",
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
            transport: DevToolsBrowserTransport::loopback(DevToolsEndpoint::loopback(1)?),
        };

        let startup = (|| {
            let endpoint = wait_for_debugging_endpoint(
                &mut launched.process,
                &active_port_file,
                startup_deadline,
            )?;
            launched.transport = DevToolsBrowserTransport::with_clients(endpoint, http, websocket);
            wait_for_devtools_readiness(
                &mut launched.process,
                &mut launched.transport,
                run,
                startup_deadline,
            )
        })();
        if let Err(error) = startup {
            let _ = launched.shutdown(run);
            return Err(error);
        }
        Ok(launched)
    }

    /// Browser discovery and attachment API.
    pub fn transport(&mut self) -> &mut DevToolsBrowserTransport {
        &mut self.transport
    }

    /// Verify process, harness-lock, and ephemeral-port cleanup state.
    pub fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
        Ok(EdgeCleanupStatus {
            process_exited: self.process.exit_code.is_some(),
            harness_lock_absent: path_is_absent(&self.lock_file)?,
            active_port_file_absent: path_is_absent(&self.active_port_file)?,
        })
    }

    /// Explicitly terminate Edge, remove the harness lock, and durably journal cleanup.
    pub fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        let process_result = self.process.shutdown();
        let endpoint_file_result = if process_result.is_ok() {
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
        let cleanup_succeeded = process_result.is_ok()
            && endpoint_file_result.is_ok()
            && cleanup_status_value.is_some_and(EdgeCleanupStatus::is_complete);
        let cleanup_error = process_result
            .as_ref()
            .err()
            .cloned()
            .or_else(|| endpoint_file_result.as_ref().err().map(ToString::to_string))
            .or_else(|| cleanup_status.as_ref().err().map(ToString::to_string))
            .or_else(|| {
                cleanup_status_value
                    .filter(|status| !status.is_complete())
                    .map(|_| {
                        "process, harness lock, or active-port cleanup was not verified".to_owned()
                    })
            });
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
        process_result.map_err(TransportError::Process)?;
        endpoint_file_result?;
        let cleanup_status = cleanup_status?;
        if !cleanup_status.is_complete() {
            return Err(TransportError::Process(
                "Edge cleanup did not satisfy the process/lock/active-port invariant".to_owned(),
            ));
        }
        journal_result?;
        Ok(())
    }
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

struct EdgeProcess {
    child: Option<Box<dyn ManagedEdgeChild>>,
    lock: Option<ProfileLock>,
    exit_code: Option<i32>,
}

impl EdgeProcess {
    fn new(child: Box<dyn ManagedEdgeChild>, mut lock: ProfileLock) -> Self {
        lock.retained_on_drop = true;
        Self {
            child: Some(child),
            lock: Some(lock),
            exit_code: None,
        }
    }

    fn shutdown(&mut self) -> Result<i32, String> {
        if let Some(code) = self.exit_code {
            return Ok(code);
        }
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "owned Edge child is missing".to_owned())?;
        let code = if let Some(code) = child.try_wait()? {
            code
        } else {
            if let Err(kill_error) = child.kill() {
                if let Some(code) = child.try_wait()? {
                    code
                } else {
                    return Err(format!("terminate Edge process: {kill_error}"));
                }
            } else {
                child.wait()?
            }
        };
        self.exit_code = Some(code);
        if let Some(lock) = self.lock.take() {
            lock.release()?;
        }
        Ok(code)
    }
}

impl Drop for EdgeProcess {
    fn drop(&mut self) {
        let _ = self.shutdown();
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
) -> Result<DevToolsEndpoint, TransportError> {
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
            Ok(contents) => return DevToolsEndpoint::from_active_port(&contents),
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
    run: &mut CaptureRun,
    deadline: Instant,
) -> Result<(), TransportError> {
    run.append_event(
        "devtools_readiness_started",
        json!({"port": transport.endpoint().port()}),
    )
    .map_err(TransportError::Journal)?;

    let mut attempts = 0u32;
    let mut last_error: Option<String> = None;
    loop {
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

        attempts = attempts.saturating_add(1);
        match transport.browser_version_with_timeout(run, remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT)) {
            Ok(_) => {
                run.append_event(
                    "devtools_readiness_succeeded",
                    json!({
                        "attempts": attempts,
                        "last_transient_error": last_error,
                    }),
                )
                .map_err(TransportError::Journal)?;
                return Ok(());
            }
            Err(error @ TransportError::ReadinessTransient(_)) => {
                last_error = Some(error.to_string());
            }
            Err(error) => {
                journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
                return Err(error);
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let error = TransportError::ReadinessTimeout {
                attempts,
                last_error: last_error.clone().unwrap_or_default(),
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
    use crate::transport::{DevToolsResource, WebSocketConnection};
    use serde_json::Value;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct FakeChild {
        id: u32,
        exited: bool,
        exit_code: i32,
        exit_signal: Option<Arc<std::sync::atomic::AtomicBool>>,
        kills: Arc<Mutex<usize>>,
        waits: Arc<Mutex<usize>>,
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
        endpoints: Arc<Mutex<Vec<u16>>>,
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
            self.endpoints.lock().unwrap().push(endpoint.port());
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
            self.exited = true;
            Ok(())
        }
        fn wait(&mut self) -> Result<i32, String> {
            *self.waits.lock().unwrap() += 1;
            self.exited = true;
            Ok(self.exit_code)
        }
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
    fn fake_process_can_emit_active_port_but_no_live_browser_is_required() {
        let base = temp_dir();
        fs::create_dir_all(&base).unwrap();
        let profile = base.join("profile");
        fs::create_dir_all(&profile).unwrap();
        let active_port = profile.join(ACTIVE_PORT_FILE);
        fs::write(&active_port, "not-a-port\n/devtools/browser/stale").unwrap();
        let error = DevToolsEndpoint::from_active_port(&fs::read_to_string(&active_port).unwrap())
            .unwrap_err();
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
        assert_eq!(*endpoints.lock().unwrap(), vec![9444, 9444]);
        assert!(
            run.events()
                .iter()
                .any(|event| { event.kind == "devtools_readiness_started" })
        );
        assert!(run.events().iter().any(|event| {
            event.kind == "devtools_readiness_succeeded"
                && event.payload["attempts"] == 2
                && event.payload["last_transient_error"]
                    .as_str()
                    .is_some_and(|value| value.contains("connection timed out"))
        }));
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
        config.startup_timeout = Duration::from_millis(180);
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
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
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

        let mut browser = LaunchedEdge::launch_with_options(
            config,
            &mut run,
            &spawner,
            Box::new(FakeHttp),
            Box::new(FakeConnector),
            true,
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
