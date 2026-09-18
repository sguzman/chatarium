//! Automated read-only smoke orchestration for the concrete Edge/CDP transport.

use crate::edge::{EdgeCleanupStatus, LaunchedEdge};
use crate::run::{CaptureRun, CaptureRunState};
use crate::transport::{BrowserTransport, PageSession, TransportError};
use serde_json::{Value, json};
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DIAGNOSTIC_ID: &str = "smoke-edge";
const DIAGNOSTIC_METHOD: &str = "Page.getFrameTree";
const DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(10);

/// Successful read-only Edge/CDP smoke result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeSuccess {
    /// Browser version returned by the local DevTools endpoint.
    pub edge_version: String,
    /// CDP protocol version returned by the local DevTools endpoint.
    pub cdp_protocol_version: String,
    /// Root frame URL returned by `Page.getFrameTree`.
    pub frame_url: String,
    /// Durable private diagnostic run path.
    pub run_path: PathBuf,
}

/// Failed smoke result with primary and cleanup failures kept separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeFailure {
    /// Diagnostic failure that prevented the smoke from passing, if any.
    pub primary_failure: Option<String>,
    /// Page-session, process, lock, or endpoint cleanup failure, if any.
    pub cleanup_failure: Option<String>,
    /// Whether session and browser cleanup were positively verified.
    pub cleanup_verified: bool,
    /// Failure to durably append/finalize a smoke journal event, if any.
    pub journal_failure: Option<String>,
    /// Compact Windows DevTools listener/policy evidence when available.
    pub diagnostic_summary: Option<String>,
    /// Run path, or `None` if a run directory could not be created.
    pub run_path: Option<PathBuf>,
    /// Diagnostic base path requested for this invocation.
    pub diagnostic_base: PathBuf,
}

impl fmt::Display for SmokeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "FAIL")?;
        if let Some(primary) = &self.primary_failure {
            writeln!(formatter, "primary: {primary}")?;
        }
        match (&self.cleanup_failure, self.cleanup_verified) {
            (Some(failure), _) => writeln!(formatter, "cleanup: failed ({failure})")?,
            (None, true) if self.run_path.is_none() => {
                writeln!(formatter, "cleanup: not needed (Edge was not launched)")?
            }
            (None, true) => writeln!(formatter, "cleanup: passed")?,
            (None, false) => writeln!(formatter, "cleanup: not verified")?,
        }
        if let Some(journal) = &self.journal_failure {
            writeln!(formatter, "journal: failed ({journal})")?;
        }
        if let Some(summary) = &self.diagnostic_summary {
            writeln!(formatter, "{summary}")?;
        }
        if let Some(path) = &self.run_path {
            writeln!(formatter, "run: {}", path.display())
        } else {
            writeln!(
                formatter,
                "run: not created (requested base: {})",
                self.diagnostic_base.display()
            )
        }
    }
}

/// Mockable launched-browser surface used by smoke orchestration.
pub trait SmokeBrowser: Send {
    /// Browser discovery/target transport for the harness-owned process.
    fn transport(&mut self) -> &mut dyn BrowserTransport;
    /// Shut down the owned browser and journal cleanup.
    fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError>;
    /// Independently inspect process, lock, and ephemeral-port cleanup state.
    fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError>;
}

/// Mockable Edge launcher for deterministic smoke-orchestration tests.
pub trait SmokeLauncher: Send + Sync {
    /// Launch the harness-owned browser and its localhost DevTools transport.
    fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn SmokeBrowser>, TransportError>;
}

/// Production launcher backed by the existing managed Edge process implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSmokeLauncher;

struct LaunchedSmokeBrowser(LaunchedEdge);

impl SmokeBrowser for LaunchedSmokeBrowser {
    fn transport(&mut self) -> &mut dyn BrowserTransport {
        self.0.transport()
    }

    fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        self.0.shutdown(run)
    }

    fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
        self.0.cleanup_status()
    }
}

impl SmokeLauncher for SystemSmokeLauncher {
    fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn SmokeBrowser>, TransportError> {
        Ok(Box::new(LaunchedSmokeBrowser(
            LaunchedEdge::launch_read_only_smoke(run)?,
        )))
    }
}

/// Private local directory that contains one durable diagnostic run per invocation.
pub fn diagnostic_run_base() -> Result<PathBuf, String> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "LOCALAPPDATA is unavailable; cannot create the smoke run".to_owned())?;
    let local_app_data = std::fs::canonicalize(&local_app_data)
        .map_err(|error| format!("resolve LOCALAPPDATA for smoke run: {error}"))?;
    Ok(local_app_data
        .join("Chatarium")
        .join("captures")
        .join("diagnostics"))
}

/// Create a durable diagnostic run and execute the smoke through an injected launcher.
pub fn run_smoke(
    diagnostic_base: &Path,
    launcher: &dyn SmokeLauncher,
) -> Result<SmokeSuccess, SmokeFailure> {
    let mut run =
        CaptureRun::create_diagnostic(diagnostic_base, DIAGNOSTIC_ID).map_err(|error| {
            SmokeFailure {
                primary_failure: Some(format!("create diagnostic run: {error}")),
                cleanup_failure: None,
                cleanup_verified: true,
                journal_failure: None,
                diagnostic_summary: None,
                run_path: None,
                diagnostic_base: diagnostic_base.to_path_buf(),
            }
        })?;
    run_smoke_with_run(&mut run, launcher)
}

fn run_smoke_with_run(
    run: &mut CaptureRun,
    launcher: &dyn SmokeLauncher,
) -> Result<SmokeSuccess, SmokeFailure> {
    let run_path = run.paths().root.clone();
    let diagnostic_base = run_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut primary_failure = None;
    let mut cleanup_failure = None;
    let mut cleanup_verified = false;
    let mut journal_failure = None;
    let mut diagnostic_summary = None;
    let mut browser_version = None;
    let mut cdp_protocol_version = None;
    let mut frame_url = None;
    let mut browser: Option<Box<dyn SmokeBrowser>> = None;
    let mut session: Option<Box<dyn PageSession>> = None;
    let mut session_closed = true;

    if let Err(error) = run.start() {
        primary_failure = Some(format!("start diagnostic run: {error}"));
        cleanup_verified = true;
    } else if let Err(error) = run.append_event(
        "smoke_started",
        json!({"diagnostic": DIAGNOSTIC_ID, "start_url": "about:blank"}),
    ) {
        primary_failure = Some(format!("journal smoke start: {error}"));
        journal_failure = Some(error);
        cleanup_verified = true;
    }

    if primary_failure.is_none() {
        match launcher.launch(run) {
            Ok(launched) => browser = Some(launched),
            Err(error) => {
                match error {
                    TransportError::DiagnosticJournalFailure {
                        primary_failure: primary,
                        journal_failure: failure,
                    } => {
                        primary_failure = primary.map(|error| format!("launch Edge: {error}"));
                        journal_failure = Some(failure);
                    }
                    other => primary_failure = Some(format!("launch Edge: {other}")),
                }
                diagnostic_summary = format_windows_diagnostics(run);
                match recorded_launch_cleanup(run) {
                    Some(Ok(())) => cleanup_verified = true,
                    Some(Err(reason)) => {
                        cleanup_failure = Some(format!("startup cleanup: {reason}"));
                    }
                    None => {
                        cleanup_failure =
                            Some("launch failed without a durable cleanup verification".to_owned());
                    }
                }
            }
        }
    }

    if let Some(launched) = browser.as_mut() {
        if primary_failure.is_none() {
            match run_diagnostics(launched.as_mut(), run, &mut session) {
                Ok((version, protocol_version, url)) => {
                    browser_version = Some(version);
                    cdp_protocol_version = Some(protocol_version);
                    frame_url = Some(url);
                }
                Err((stage, error)) => {
                    primary_failure = Some(format!("{stage}: {error}"));
                    if let Err(journal_error) = run.append_event(
                        "smoke_phase_failed",
                        json!({"stage": stage, "error": error}),
                    ) {
                        journal_failure = Some(journal_error);
                    }
                }
            }
        }

        if let Some(mut page_session) = session.take() {
            session_closed = match page_session.close() {
                Ok(()) => {
                    if let Err(error) = run.append_event("smoke_page_session_closed", Value::Null) {
                        journal_failure.get_or_insert(error);
                    }
                    true
                }
                Err(error) => {
                    cleanup_failure = Some(format!("close CDP page session: {error}"));
                    if let Err(journal_error) = run.append_event(
                        "smoke_page_session_close_failed",
                        json!({"error": error.to_string()}),
                    ) {
                        journal_failure.get_or_insert(journal_error);
                    }
                    false
                }
            };
        }

        match launched.shutdown(run) {
            Ok(()) => {}
            Err(TransportError::Journal(error)) => {
                journal_failure.get_or_insert(error);
            }
            Err(TransportError::DiagnosticJournalFailure {
                primary_failure,
                journal_failure: error,
            }) => {
                if let Some(primary) = primary_failure {
                    cleanup_failure.get_or_insert(format!("shut down Edge: {primary}"));
                }
                journal_failure.get_or_insert(error);
            }
            Err(error) => {
                cleanup_failure.get_or_insert_with(|| format!("shut down Edge: {error}"));
            }
        }
        match launched.cleanup_status() {
            Ok(status) if status.is_complete() => {
                cleanup_verified = session_closed;
            }
            Ok(status) => {
                cleanup_failure.get_or_insert_with(|| {
                    format!(
                        "cleanup incomplete (process_exited={}, lock_absent={}, active_port_absent={})",
                        status.process_exited,
                        status.harness_lock_absent,
                        status.active_port_file_absent
                    )
                });
            }
            Err(error) => {
                cleanup_failure.get_or_insert_with(|| format!("verify Edge cleanup: {error}"));
            }
        }
    }

    let passed = primary_failure.is_none()
        && cleanup_failure.is_none()
        && cleanup_verified
        && journal_failure.is_none()
        && browser_version.is_some()
        && cdp_protocol_version.is_some()
        && frame_url.as_deref() == Some("about:blank");
    if primary_failure.is_some() && diagnostic_summary.is_none() {
        diagnostic_summary = format_windows_diagnostics(run);
    }
    if let Err(error) = run.append_event(
        "smoke_finished",
        json!({
            "diagnostic_and_cleanup_passed": passed,
            "terminal_state": if passed { "completed" } else { "failed_before_mutation" },
            "primary_failure": primary_failure,
            "cleanup_failure": cleanup_failure,
            "cleanup_verified": cleanup_verified,
            "journal_failure": journal_failure,
        }),
    ) {
        journal_failure.get_or_insert(error);
    }

    let terminal_state = if passed && journal_failure.is_none() {
        CaptureRunState::Completed
    } else {
        CaptureRunState::FailedBeforeMutation
    };
    if let Err(error) = run.finish(terminal_state) {
        journal_failure.get_or_insert(format!("finish diagnostic run: {error}"));
    }

    if primary_failure.is_none()
        && cleanup_failure.is_none()
        && cleanup_verified
        && journal_failure.is_none()
    {
        return Ok(SmokeSuccess {
            edge_version: browser_version.unwrap_or_default(),
            cdp_protocol_version: cdp_protocol_version.unwrap_or_default(),
            frame_url: frame_url.unwrap_or_default(),
            run_path,
        });
    }

    Err(SmokeFailure {
        primary_failure,
        cleanup_failure,
        cleanup_verified,
        journal_failure,
        diagnostic_summary,
        run_path: Some(run_path),
        diagnostic_base,
    })
}

fn format_windows_diagnostics(run: &CaptureRun) -> Option<String> {
    let events = run.events();
    let initial = events.iter().find(|event| {
        event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "initial"
    });
    let Some(initial) = initial else {
        let preflight = events.iter().find(|event| {
            event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "policy_preflight"
        })?;
        let policy = preflight.payload["remote_debugging_allowed"]["summary"]
            .as_str()
            .unwrap_or("unknown");
        let pid = preflight.payload["launched_edge_pid"]
            .as_u64()
            .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
        let diagnosis = if preflight.payload["remote_debugging_disabled"] == true {
            "listener diagnosis: readiness stopped because remote debugging is disabled by policy"
        } else {
            "listener diagnosis: no port was discovered before startup failed; listener state is unknown"
        };
        return Some(format!(
            "DevTools port: not discovered\nRemoteDebuggingAllowed: {policy}\nlaunched Edge pid: {pid}\nlistener at initial probe: not sampled (no port was selected)\nlistener at final probe: not sampled (no port was selected)\n{diagnosis}\nconnect result: not attempted"
        ));
    };
    let final_event = events
        .iter()
        .rev()
        .find(|event| event.kind == "devtools_os_diagnostics" && event.payload["stage"] == "final");
    let port = initial.payload["port"].as_u64()?;
    let policy = initial.payload["remote_debugging_allowed"]["summary"]
        .as_str()
        .unwrap_or("unknown");
    let pid = initial.payload["launched_edge_pid"]
        .as_u64()
        .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
    let render_listeners = |event: &serde_json::Value| {
        let snapshot = &event["listener_snapshot"];
        if let Some(error) = snapshot["inspection_error"].as_str() {
            return format!("unknown ({error})");
        }
        let listeners = snapshot["listeners"].as_array();
        match listeners {
            Some(listeners) if listeners.is_empty() => "none".to_owned(),
            Some(listeners) => listeners
                .iter()
                .map(|listener| {
                    let address = listener["local_address"].as_str().unwrap_or("unknown");
                    let formatted_address = if address.contains(':') {
                        format!("[{address}]")
                    } else {
                        address.to_owned()
                    };
                    format!(
                        "{}:{} state={} pid={} relation={}",
                        formatted_address,
                        listener["local_port"].as_u64().unwrap_or_default(),
                        listener["state"].as_str().unwrap_or("unknown"),
                        listener["owning_pid"]
                            .as_u64()
                            .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
                        listener["owner_relation"].as_str().unwrap_or("unknown"),
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            None => snapshot["inspection_error"].as_str().map_or_else(
                || "unknown".to_owned(),
                |error| format!("unknown ({error})"),
            ),
        }
    };
    let connections = final_event.unwrap_or(initial).payload["connect_results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|connection| {
            format!(
                "{}: {}",
                connection["family"].as_str().unwrap_or("unknown"),
                connection["last_result"]
                    .as_str()
                    .unwrap_or("not attempted"),
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let final_listeners = final_event.map_or_else(
        || "not captured".to_owned(),
        |event| render_listeners(&event.payload),
    );
    let has_listener = |event: &serde_json::Value| {
        event["listener_snapshot"]["listeners"]
            .as_array()
            .is_some_and(|listeners| {
                listeners
                    .iter()
                    .any(|listener| listener["state"] == "LISTEN")
            })
    };
    let snapshot_has_listener = |event: &serde_json::Value| {
        let snapshot = &event["listener_snapshot"];
        if snapshot["inspection_error"].as_str().is_some() {
            None
        } else {
            snapshot["listeners"]
                .as_array()
                .map(|_| has_listener(event))
        }
    };
    let initial_has_listener = snapshot_has_listener(&initial.payload);
    let final_has_listener = final_event.and_then(|event| snapshot_has_listener(&event.payload));
    let diagnosis = if initial_has_listener == Some(false) && final_has_listener == Some(false) {
        "listener diagnosis: Edge advertised a DevTools port but no TCP listener was observed"
    } else if final_has_listener == Some(true) {
        "listener diagnosis: TCP listener observed; connection readiness failed separately"
    } else if initial_has_listener == Some(true) && final_has_listener == Some(false) {
        "listener diagnosis: listener was observed initially and absent at the final probe; it may have disappeared during startup"
    } else {
        "listener diagnosis: unknown from available OS snapshots"
    };
    Some(format!(
        "DevTools port: {port}\nRemoteDebuggingAllowed: {policy}\nlaunched Edge pid: {pid}\nlistener at initial probe: {}\nlistener at final probe: {final_listeners}\n{diagnosis}\nconnect result: {connections}",
        render_listeners(&initial.payload),
    ))
}

fn run_diagnostics(
    browser: &mut dyn SmokeBrowser,
    run: &mut CaptureRun,
    session: &mut Option<Box<dyn PageSession>>,
) -> Result<(String, String, String), (&'static str, String)> {
    let transport = browser.transport();
    let version = transport
        .browser_version(run)
        .map_err(|error| ("read browser/CDP version", error.to_string()))?;
    let targets = transport
        .list_targets(run)
        .map_err(|error| ("list page targets", error.to_string()))?;
    let blank_targets = targets
        .iter()
        .filter(|target| target.target_type == "page" && target.url == "about:blank")
        .collect::<Vec<_>>();
    if blank_targets.len() != 1 {
        return Err((
            "select harness about:blank target",
            format!(
                "expected exactly one about:blank page target; observed {}",
                blank_targets.len()
            ),
        ));
    }
    let target = blank_targets[0];
    if target.id.is_empty() {
        return Err((
            "select harness about:blank target",
            "DevTools returned an empty target ID".to_owned(),
        ));
    }
    run.append_event(
        "smoke_page_target_selected",
        json!({"target_id": target.id, "url": "about:blank"}),
    )
    .map_err(|error| ("journal target selection", error))?;
    *session = Some(
        transport
            .attach(&target.id, run)
            .map_err(|error| ("attach about:blank target", error.to_string()))?,
    );
    run.append_event("smoke_page_session_attached", Value::Null)
        .map_err(|error| ("journal page attachment", error))?;
    run.append_event(
        "smoke_diagnostic_command_started",
        json!({"method": DIAGNOSTIC_METHOD}),
    )
    .map_err(|error| ("journal diagnostic start", error))?;
    let result = session
        .as_mut()
        .expect("session was just attached")
        .command(DIAGNOSTIC_METHOD, json!({}), DIAGNOSTIC_TIMEOUT)
        .map_err(|error| ("execute read-only CDP diagnostic", error.to_string()))?;
    let root_frame = result
        .get("frameTree")
        .and_then(|tree| tree.get("frame"))
        .ok_or_else(|| {
            (
                "decode Page.getFrameTree response",
                "response is missing frameTree.frame".to_owned(),
            )
        })?;
    let frame_url = root_frame
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            (
                "decode Page.getFrameTree response",
                "root frame URL is missing or not a string".to_owned(),
            )
        })?;
    if frame_url != "about:blank" {
        return Err((
            "validate read-only CDP diagnostic",
            format!("root frame URL was {frame_url:?}, expected about:blank"),
        ));
    }
    run.append_event(
        "smoke_diagnostic_command_completed",
        json!({"method": DIAGNOSTIC_METHOD, "frame_url": frame_url, "frame_tree": result}),
    )
    .map_err(|error| ("journal diagnostic result", error))?;
    Ok((
        version.browser,
        version.protocol_version,
        frame_url.to_owned(),
    ))
}

fn recorded_launch_cleanup(run: &CaptureRun) -> Option<Result<(), String>> {
    run.events()
        .iter()
        .rev()
        .find(|event| event.kind == "browser_shutdown_cleanup")
        .and_then(|event| {
            event
                .payload
                .get("cleanup_succeeded")
                .and_then(Value::as_bool)
                .map(|succeeded| {
                    if succeeded {
                        Ok(())
                    } else {
                        Err(event
                            .payload
                            .get("cleanup_error")
                            .and_then(Value::as_str)
                            .unwrap_or("the launch cleanup journal reports incomplete cleanup")
                            .to_owned())
                    }
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{BrowserVersion, CdpEvent, TargetInfo};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use url::Url;

    #[derive(Clone)]
    struct MockConfig {
        targets: Vec<TargetInfo>,
        list_error: Option<TransportError>,
        attach_error: Option<TransportError>,
        command_result: Result<Value, TransportError>,
        close_result: Result<(), TransportError>,
        cleanup: EdgeCleanupStatus,
        shutdown_result: Result<(), TransportError>,
        launch_error: Option<TransportError>,
    }

    impl Default for MockConfig {
        fn default() -> Self {
            Self {
                targets: vec![target("blank", "about:blank")],
                list_error: None,
                attach_error: None,
                command_result: Ok(json!({
                    "frameTree": {"frame": {"url":"about:blank"}}
                })),
                close_result: Ok(()),
                cleanup: EdgeCleanupStatus {
                    process_exited: true,
                    harness_lock_absent: true,
                    active_port_file_absent: true,
                },
                shutdown_result: Ok(()),
                launch_error: None,
            }
        }
    }

    fn target(id: &str, url: &str) -> TargetInfo {
        TargetInfo {
            id: id.to_owned(),
            target_type: "page".to_owned(),
            title: String::new(),
            url: url.to_owned(),
            websocket_url: Some(Url::parse("ws://127.0.0.1:9321/devtools/page/test").unwrap()),
        }
    }

    #[derive(Default)]
    struct MockState {
        attached_target: Option<String>,
        command_method: Option<String>,
        page_closed: bool,
        shutdown_called: bool,
    }

    struct MockLauncher {
        config: MockConfig,
        state: Arc<Mutex<MockState>>,
    }

    struct DiagnosticJournalFailureLauncher;

    impl SmokeLauncher for DiagnosticJournalFailureLauncher {
        fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn SmokeBrowser>, TransportError> {
            run.append_event(
                "browser_shutdown_cleanup",
                json!({"cleanup_succeeded": true}),
            )
            .map_err(TransportError::Journal)?;
            Err(TransportError::DiagnosticJournalFailure {
                primary_failure: Some(
                    "DevTools readiness deadline expired after 12 attempts".to_owned(),
                ),
                journal_failure: "append final listener snapshot: disk full".to_owned(),
            })
        }
    }

    impl MockLauncher {
        fn new(config: MockConfig) -> (Self, Arc<Mutex<MockState>>) {
            let state = Arc::new(Mutex::new(MockState::default()));
            (
                Self {
                    config,
                    state: state.clone(),
                },
                state,
            )
        }
    }

    impl SmokeLauncher for MockLauncher {
        fn launch(&self, _run: &mut CaptureRun) -> Result<Box<dyn SmokeBrowser>, TransportError> {
            if let Some(error) = &self.config.launch_error {
                return Err(error.clone());
            }
            Ok(Box::new(MockBrowser {
                config: self.config.clone(),
                state: self.state.clone(),
            }))
        }
    }

    struct MockBrowser {
        config: MockConfig,
        state: Arc<Mutex<MockState>>,
    }

    impl SmokeBrowser for MockBrowser {
        fn transport(&mut self) -> &mut dyn BrowserTransport {
            self
        }

        fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
            self.state.lock().unwrap().shutdown_called = true;
            run.append_event(
                "browser_shutdown_cleanup",
                json!({"cleanup_succeeded": self.config.cleanup.is_complete()}),
            )
            .map_err(TransportError::Journal)?;
            self.config.shutdown_result.clone()
        }

        fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
            Ok(self.config.cleanup.clone())
        }
    }

    impl BrowserTransport for MockBrowser {
        fn browser_version(
            &mut self,
            _run: &mut CaptureRun,
        ) -> Result<BrowserVersion, TransportError> {
            Ok(BrowserVersion {
                browser: "Microsoft Edge/mock".to_owned(),
                protocol_version: "1.3".to_owned(),
            })
        }

        fn list_targets(
            &mut self,
            _run: &mut CaptureRun,
        ) -> Result<Vec<TargetInfo>, TransportError> {
            if let Some(error) = &self.config.list_error {
                return Err(error.clone());
            }
            Ok(self.config.targets.clone())
        }

        fn attach(
            &mut self,
            target_id: &str,
            _run: &mut CaptureRun,
        ) -> Result<Box<dyn PageSession>, TransportError> {
            if let Some(error) = &self.config.attach_error {
                return Err(error.clone());
            }
            self.state.lock().unwrap().attached_target = Some(target_id.to_owned());
            Ok(Box::new(MockPageSession {
                config: self.config.clone(),
                state: self.state.clone(),
            }))
        }
    }

    struct MockPageSession {
        config: MockConfig,
        state: Arc<Mutex<MockState>>,
    }

    impl PageSession for MockPageSession {
        fn command(
            &mut self,
            method: &str,
            _params: Value,
            _timeout: Duration,
        ) -> Result<Value, TransportError> {
            self.state.lock().unwrap().command_method = Some(method.to_owned());
            self.config.command_result.clone()
        }

        fn next_event(&mut self, _timeout: Duration) -> Result<Option<CdpEvent>, TransportError> {
            Ok(None)
        }

        fn close(&mut self) -> Result<(), TransportError> {
            self.state.lock().unwrap().page_closed = true;
            self.config.close_result.clone()
        }
    }

    fn diagnostic_run() -> (CaptureRun, PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("chatarium-smoke-{stamp}"));
        let run = CaptureRun::create_diagnostic(&base, DIAGNOSTIC_ID).unwrap();
        (run, base)
    }

    #[test]
    fn terminal_failure_summary_renders_listener_policy_pid_and_connection_evidence() {
        let (mut run, base) = diagnostic_run();
        run.append_event(
            "devtools_os_diagnostics",
            json!({
                "stage":"initial",
                "port":12345,
                "launched_edge_pid":1234,
                "remote_debugging_allowed":{"summary":"not configured"},
                "listener_snapshot":{"listeners":[]},
                "connect_results":[
                    {"family":"ipv4","last_result":"timed out"},
                    {"family":"ipv6","last_result":"timed out"}
                ]
            }),
        )
        .unwrap();
        run.append_event(
            "devtools_os_diagnostics",
            json!({
                "stage":"final",
                "port":12345,
                "launched_edge_pid":1234,
                "remote_debugging_allowed":{"summary":"not configured"},
                "listener_snapshot":{"listeners":[{
                    "local_address":"::1",
                    "local_port":12345,
                    "state":"LISTEN",
                    "owning_pid":5678,
                    "owner_relation":"descendant"
                }]},
                "connect_results":[
                    {"family":"ipv4","last_result":"timed out"},
                    {"family":"ipv6","last_result":"timed out"}
                ]
            }),
        )
        .unwrap();

        let summary = format_windows_diagnostics(&run).unwrap();
        assert!(summary.contains("DevTools port: 12345"));
        assert!(summary.contains("RemoteDebuggingAllowed: not configured"));
        assert!(summary.contains("launched Edge pid: 1234"));
        assert!(summary.contains("listener at initial probe: none"));
        assert!(summary.contains("[::1]:12345 state=LISTEN pid=5678 relation=descendant"));
        assert!(summary.contains("connection readiness failed separately"));
        assert!(summary.contains("ipv6: timed out"));
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn terminal_failure_identifies_no_listener_at_either_snapshot() {
        let (mut run, base) = diagnostic_run();
        for stage in ["initial", "final"] {
            run.append_event(
                "devtools_os_diagnostics",
                json!({
                    "stage":stage,
                    "port":12345,
                    "launched_edge_pid":1234,
                    "remote_debugging_allowed":{"summary":"enabled"},
                    "listener_snapshot":{"listeners":[]},
                    "connect_results":[
                        {"family":"ipv4","last_result":"connection refused"},
                        {"family":"ipv6","last_result":"connection refused"}
                    ]
                }),
            )
            .unwrap();
        }

        let summary = format_windows_diagnostics(&run).unwrap();
        assert!(summary.contains("no TCP listener was observed"));
        assert!(summary.contains("RemoteDebuggingAllowed: enabled"));
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn policy_preflight_failure_reports_that_no_port_or_listener_was_sampled() {
        let (mut run, base) = diagnostic_run();
        run.append_event(
            "devtools_os_diagnostics",
            json!({
                "stage":"policy_preflight",
                "port":null,
                "launched_edge_pid":1234,
                "remote_debugging_allowed":{"summary":"disabled"},
                "remote_debugging_disabled":true,
                "listener_snapshot":null,
                "connect_results":[]
            }),
        )
        .unwrap();

        let summary = format_windows_diagnostics(&run).unwrap();
        assert!(summary.contains("RemoteDebuggingAllowed: disabled"));
        assert!(summary.contains("not sampled (no port was selected)"));
        assert!(summary.contains("connect result: not attempted"));
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn diagnostic_journal_failure_is_visible_separately_from_primary_and_cleanup() {
        let (mut run, base) = diagnostic_run();
        let failure = run_smoke_with_run(&mut run, &DiagnosticJournalFailureLauncher).unwrap_err();

        assert!(
            failure
                .primary_failure
                .as_deref()
                .is_some_and(|message| message.contains("DevTools readiness deadline expired"))
        );
        assert!(
            failure
                .journal_failure
                .as_deref()
                .is_some_and(|message| message.contains("final listener snapshot: disk full"))
        );
        assert!(failure.cleanup_verified);
        assert!(failure.cleanup_failure.is_none());
        let output = failure.to_string();
        assert!(output.contains("primary: launch Edge: DevTools readiness deadline expired"));
        assert!(output.contains("journal: failed (append final listener snapshot: disk full)"));
        assert!(output.contains("cleanup: passed"));
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn smoke_success_attaches_only_blank_runs_read_only_command_and_verifies_cleanup() {
        let mut config = MockConfig::default();
        config.targets.push(target("other", "https://example.com/"));
        let (launcher, state) = MockLauncher::new(config);
        let (mut run, base) = diagnostic_run();
        let success = run_smoke_with_run(&mut run, &launcher).unwrap();

        assert_eq!(success.edge_version, "Microsoft Edge/mock");
        assert_eq!(success.cdp_protocol_version, "1.3");
        assert_eq!(success.frame_url, "about:blank");
        assert_eq!(
            state.lock().unwrap().attached_target.as_deref(),
            Some("blank")
        );
        assert_eq!(
            state.lock().unwrap().command_method.as_deref(),
            Some("Page.getFrameTree")
        );
        assert!(state.lock().unwrap().page_closed);
        assert!(state.lock().unwrap().shutdown_called);
        assert_eq!(run.manifest().state, CaptureRunState::Completed);
        assert!(
            run.events()
                .iter()
                .any(|event| event.kind == "smoke_finished")
        );
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn command_failure_is_preserved_and_still_closes_session_and_browser() {
        let mut config = MockConfig::default();
        config.command_result = Err(TransportError::CommandOutcomeUnknown {
            command_id: 1,
            reason: "disconnected after dispatch".to_owned(),
        });
        let (launcher, state) = MockLauncher::new(config);
        let (mut run, base) = diagnostic_run();
        let failure = run_smoke_with_run(&mut run, &launcher).unwrap_err();

        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("outcome is unknown")
        );
        assert!(failure.cleanup_verified);
        assert!(state.lock().unwrap().page_closed);
        assert!(state.lock().unwrap().shutdown_called);
        assert_eq!(run.manifest().state, CaptureRunState::FailedBeforeMutation);
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn cleanup_failure_is_not_reported_as_pass_and_is_durable() {
        let mut config = MockConfig::default();
        config.cleanup.active_port_file_absent = false;
        let (launcher, state) = MockLauncher::new(config);
        let (mut run, base) = diagnostic_run();
        let failure = run_smoke_with_run(&mut run, &launcher).unwrap_err();

        assert!(failure.primary_failure.is_none());
        assert!(!failure.cleanup_verified);
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("active_port_absent=false")
        );
        assert!(state.lock().unwrap().page_closed);
        assert!(state.lock().unwrap().shutdown_called);
        assert_eq!(run.manifest().state, CaptureRunState::FailedBeforeMutation);
        assert_eq!(
            run.events()
                .iter()
                .find(|event| event.kind == "smoke_finished")
                .unwrap()
                .payload["diagnostic_and_cleanup_passed"],
            false
        );
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn target_discovery_failure_still_shuts_down_browser() {
        let mut config = MockConfig::default();
        config.list_error = Some(TransportError::Disconnected);
        let (launcher, state) = MockLauncher::new(config);
        let (mut run, base) = diagnostic_run();
        let failure = run_smoke_with_run(&mut run, &launcher).unwrap_err();

        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("list page targets")
        );
        assert!(failure.cleanup_verified);
        assert!(state.lock().unwrap().shutdown_called);
        assert!(!state.lock().unwrap().page_closed);
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn launch_failure_without_cleanup_evidence_remains_unverified() {
        let mut config = MockConfig::default();
        config.launch_error = Some(TransportError::Process("Edge not found".to_owned()));
        let (launcher, _) = MockLauncher::new(config);
        let (mut run, base) = diagnostic_run();
        let failure = run_smoke_with_run(&mut run, &launcher).unwrap_err();

        assert!(!failure.cleanup_verified);
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("without a durable cleanup verification")
        );
        assert_eq!(run.manifest().state, CaptureRunState::FailedBeforeMutation);
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }
}
