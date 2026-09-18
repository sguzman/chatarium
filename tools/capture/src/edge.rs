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
    active_port_file: PathBuf,
    transport: DevToolsBrowserTransport,
}

impl LaunchedEdge {
    /// Launch Edge with the dedicated profile and localhost-only ephemeral debugging port.
    pub fn launch(run: &mut CaptureRun) -> Result<Self, TransportError> {
        let config = EdgeLaunchConfig::discover()?;
        Self::launch_with(
            config,
            run,
            &SystemEdgeSpawner,
            Box::new(LoopbackDevToolsHttp),
            Box::new(LoopbackWebSocketConnector),
        )
    }

    fn launch_with(
        config: EdgeLaunchConfig,
        run: &mut CaptureRun,
        spawner: &dyn EdgeProcessSpawner,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
    ) -> Result<Self, TransportError> {
        validate_dedicated_capture_profile(&config.profile, &config.local_app_data)
            .map_err(TransportError::UnsafeProfile)?;
        ensure_profile_directories(&config)?;
        let lock_path = config.capture_root.join(LOCK_FILE);
        let lock = ProfileLock::acquire(lock_path)?;
        let active_port_file = config.profile.join(ACTIVE_PORT_FILE);
        ensure_active_port_absent(&active_port_file)?;

        let arguments = vec![
            format!("--user-data-dir={}", config.profile.display()),
            "--remote-debugging-address=127.0.0.1".to_owned(),
            "--remote-debugging-port=0".to_owned(),
            "--no-first-run".to_owned(),
            "--no-default-browser-check".to_owned(),
            "about:blank".to_owned(),
        ];
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
            active_port_file: active_port_file.clone(),
            transport: DevToolsBrowserTransport::loopback(DevToolsEndpoint::loopback(1)?),
        };

        let startup = (|| {
            let endpoint = wait_for_debugging_endpoint(
                &mut launched.process,
                &active_port_file,
                config.startup_timeout,
            )?;
            launched.transport = DevToolsBrowserTransport::with_clients(endpoint, http, websocket);
            launched.transport.browser_version(run)?;
            Ok::<(), TransportError>(())
        })();
        if let Err(error) = startup {
            let cleanup = launched.shutdown(run);
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(TransportError::Process(format!(
                    "{error}; cleanup also failed: {cleanup_error}"
                ))),
            };
        }
        Ok(launched)
    }

    /// Browser discovery and attachment API.
    pub fn transport(&mut self) -> &mut DevToolsBrowserTransport {
        &mut self.transport
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
        let cleanup_succeeded = process_result.is_ok() && endpoint_file_result.is_ok();
        let cleanup_error = process_result
            .as_ref()
            .err()
            .cloned()
            .or_else(|| endpoint_file_result.as_ref().err().map(ToString::to_string));
        let journal_result = run
            .append_event(
                "browser_shutdown_cleanup",
                json!({
                    "exit_code": process_result.as_ref().ok().copied(),
                    "active_port_file_removed": endpoint_file_result.as_ref().ok().copied().unwrap_or(false),
                    "cleanup_succeeded": cleanup_succeeded,
                    "cleanup_error": cleanup_error,
                }),
            )
            .map_err(TransportError::Journal);
        process_result.map_err(TransportError::Process)?;
        endpoint_file_result?;
        journal_result?;
        Ok(())
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
    timeout: Duration,
) -> Result<DevToolsEndpoint, TransportError> {
    let deadline = Instant::now() + timeout;
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
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_experiment;
    use crate::transport::{DevToolsResource, WebSocketConnection};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    struct FakeChild {
        id: u32,
        exited: bool,
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
            Ok(self.exited.then_some(0))
        }
        fn kill(&mut self) -> Result<(), String> {
            *self.kills.lock().unwrap() += 1;
            self.exited = true;
            Ok(())
        }
        fn wait(&mut self) -> Result<i32, String> {
            *self.waits.lock().unwrap() += 1;
            self.exited = true;
            Ok(0)
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

    #[test]
    fn mocked_edge_launch_discovery_attachment_and_cleanup_are_journaled() {
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

        let mut browser = LaunchedEdge::launch_with(
            config,
            &mut run,
            &spawner,
            Box::new(FakeHttp),
            Box::new(FakeConnector),
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
        assert!(args.contains(&"about:blank".to_owned()));
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
