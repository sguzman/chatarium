//! Edge capture and cleanup test suite, unchanged.
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
        config.startup_timeout = Duration::from_secs(3);
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
        config.startup_timeout = Duration::from_secs(3);
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
        assert!(elapsed < Duration::from_secs(5), "elapsed: {elapsed:?}");
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
        config.startup_timeout = Duration::from_secs(3);
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
