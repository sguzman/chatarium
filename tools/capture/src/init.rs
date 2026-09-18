//! Operator-confirmed bootstrap of Chatarium's dedicated persistent Edge profile.

use crate::edge::{EdgeCleanupStatus, LaunchedEdge};
use crate::run::{CaptureRun, CaptureRunState};
use crate::transport::{TargetInfo, TransportError};
use serde_json::json;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

/// Exact start URL requested by the profile bootstrap.
pub const CHATGPT_START_URL: &str = "https://chatgpt.com/";
/// Persistent profile identity used for journal and operator output.
pub const PROFILE_IDENTITY: &str = "%LOCALAPPDATA%\\Chatarium\\capture-browser\\edge-profile\\";
/// Single instruction shown before waiting for operator confirmation.
pub const OPERATOR_INSTRUCTION: &str = "Sign in to ChatGPT normally in the Chatarium Edge window. When ChatGPT is ready for use, return here and press Enter.";

const INIT_RUN_ID: &str = "chatgpt-profile-init";

/// Read-only view of the launched browser's page targets and cleanup boundary.
pub trait InitBrowser: Send {
    /// Query page targets over the browser-wide DevTools transport.
    fn page_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError>;
    /// Close the pipe and harness-owned Edge process and journal cleanup.
    fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError>;
    /// Independently inspect process and harness-lock cleanup state.
    fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError>;
}

/// Narrow launch boundary used by the bootstrap coordinator and deterministic tests.
pub trait InitLauncher: Send + Sync {
    /// Launch the dedicated persistent profile at the fixed ChatGPT start URL.
    fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn InitBrowser>, TransportError>;
}

/// Production launcher backed by the existing Windows anonymous-pipe Edge transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemInitLauncher;

struct LaunchedInitBrowser(LaunchedEdge);

impl InitBrowser for LaunchedInitBrowser {
    fn page_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        self.0.transport().refresh_targets(run)
    }
    fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        self.0.shutdown_for_profile_init(run)
    }
    fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
        self.0.cleanup_status()
    }
}

impl InitLauncher for SystemInitLauncher {
    fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn InitBrowser>, TransportError> {
        #[cfg(windows)]
        {
            LaunchedEdge::launch_profile_init(run)
                .map(|browser| Box::new(LaunchedInitBrowser(browser)) as Box<dyn InitBrowser>)
        }
        #[cfg(not(windows))]
        {
            let _ = run;
            Err(TransportError::Process(
                "chatarium-capture init requires the Windows anonymous-pipe Edge transport"
                    .to_owned(),
            ))
        }
    }
}

/// Result of the single terminal confirmation read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorResponse {
    /// The operator pressed Enter.
    Confirmed,
    /// Stdin closed before Enter was received.
    EndOfInput,
}

/// Human confirmation boundary; authentication remains in the normal browser UI.
pub trait InitOperator {
    /// Present the one instruction and read exactly one line from stdin.
    fn confirm(&mut self) -> Result<OperatorResponse, String>;
}

/// Stdin-backed operator confirmation used by the CLI.
#[derive(Debug, Default)]
pub struct StdinInitOperator;

impl InitOperator for StdinInitOperator {
    fn confirm(&mut self) -> Result<OperatorResponse, String> {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        writeln!(output, "{OPERATOR_INSTRUCTION}")
            .and_then(|()| output.flush())
            .map_err(|error| format!("flush operator instruction: {error}"))?;
        let mut line = String::new();
        io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|error| format!("read operator confirmation: {error}"))?;
        Ok(parse_operator_line(&line))
    }
}

fn parse_operator_line(line: &str) -> OperatorResponse {
    if line.ends_with('\n') {
        OperatorResponse::Confirmed
    } else {
        OperatorResponse::EndOfInput
    }
}

/// Successful operator-confirmed profile bootstrap. This does not prove authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSuccess {
    /// Sanitized final ChatGPT page URL.
    pub final_target_url: String,
    /// Durable private bootstrap run directory.
    pub run_path: PathBuf,
}

/// Separate primary, journal, and cleanup failures from one bootstrap attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitFailure {
    /// Main bootstrap failure, if any.
    pub primary_failure: Option<String>,
    /// Durable journal failure, if any.
    pub journal_failure: Option<String>,
    /// Process, pipe, or harness-lock cleanup failure, if any.
    pub cleanup_failure: Option<String>,
    /// Durable private bootstrap run, when it was created.
    pub run_path: Option<PathBuf>,
}

impl fmt::Display for InitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FAIL")?;
        if let Some(primary) = &self.primary_failure {
            write!(formatter, "\nprimary: {primary}")?;
        }
        if let Some(journal) = &self.journal_failure {
            write!(formatter, "\njournal failure: {journal}")?;
        }
        if let Some(cleanup) = &self.cleanup_failure {
            write!(formatter, "\ncleanup failure: {cleanup}")?;
        }
        if let Some(path) = &self.run_path {
            write!(formatter, "\nrun: {}", path.display())?;
        } else {
            formatter.write_str("\nrun: not created")?;
        }
        Ok(())
    }
}

/// Create a durable bootstrap run, launch Edge, wait for one confirmation, and validate targets.
pub fn run_init(
    diagnostic_base: &Path,
    launcher: &dyn InitLauncher,
    operator: &mut dyn InitOperator,
) -> Result<InitSuccess, InitFailure> {
    let mut run = CaptureRun::create_diagnostic(diagnostic_base, INIT_RUN_ID).map_err(|error| {
        InitFailure {
            primary_failure: Some(format!("create bootstrap run: {error}")),
            journal_failure: None,
            cleanup_failure: None,
            run_path: None,
        }
    })?;
    let run_path = run.paths().root.clone();
    let mut failure = InitFailure {
        primary_failure: None,
        journal_failure: None,
        cleanup_failure: None,
        run_path: Some(run_path.clone()),
    };
    let mut terminal_state = CaptureRunState::FailedBeforeMutation;
    let mut final_target_url = None;
    let mut browser: Option<Box<dyn InitBrowser>> = None;

    if let Err(error) = run.start() {
        failure.journal_failure = Some(format!("start bootstrap run: {error}"));
    } else if let Err(error) = run.append_event(
        "init_started",
        json!({
            "operation_type": "profile_bootstrap",
            "transport_mode": "pipe",
            "profile_identity": PROFILE_IDENTITY,
            "requested_start_url": CHATGPT_START_URL,
        }),
    ) {
        failure.journal_failure = Some(error);
    }

    if failure.journal_failure.is_none() {
        match launcher.launch(&mut run) {
            Ok(launched) => browser = Some(launched),
            Err(error) => {
                let context = launch_failure_context(&run);
                record_transport_failure(&mut failure, context, error);
                record_failed_launch_cleanup(&mut failure, &run);
            }
        }
    }

    if let Some(launched) = browser.as_mut() {
        if let Err(error) = run.append_event(
            "init_operator_instruction_presented",
            json!({"instruction": OPERATOR_INSTRUCTION}),
        ) {
            failure.journal_failure = Some(error);
        } else {
            match operator.confirm() {
                Ok(OperatorResponse::Confirmed) => {
                    if let Err(error) = run.append_event(
                        "init_operator_confirmation_received",
                        json!({"confirmation": "enter"}),
                    ) {
                        failure.journal_failure = Some(error);
                    } else {
                        match launched.page_targets(&mut run) {
                            Ok(targets) => {
                                let observed_urls = targets
                                    .iter()
                                    .map(|target| safe_target_url_for_journal(&target.url))
                                    .collect::<Vec<_>>();
                                if let Err(error) = run.append_event(
                                    "init_final_page_targets_observed",
                                    json!({"page_target_urls": observed_urls}),
                                ) {
                                    failure.journal_failure = Some(error);
                                } else if let Some(target) = targets
                                    .iter()
                                    .find(|target| is_chatgpt_https_url(&target.url))
                                {
                                    let url = safe_target_url_for_journal(&target.url);
                                    if let Err(error) = run.append_event(
                                        "init_chatgpt_target_found",
                                        json!({
                                            "target_url": url,
                                            "operator_confirmed": true,
                                            "authentication_verified": false,
                                        }),
                                    ) {
                                        failure.journal_failure = Some(error);
                                    } else {
                                        final_target_url = Some(url);
                                        terminal_state = CaptureRunState::Completed;
                                    }
                                } else {
                                    failure.primary_failure = Some(format!(
                                        "operator confirmed, but no HTTPS page target on the exact chatgpt.com host was present; observed targets: {}",
                                        observed_urls.join(", ")
                                    ));
                                    if let Err(error) = run.append_event(
                                        "init_chatgpt_target_absent",
                                        json!({
                                            "page_target_urls": observed_urls,
                                            "operator_confirmed": true,
                                        }),
                                    ) {
                                        failure.journal_failure = Some(error);
                                    }
                                }
                            }
                            Err(error) => record_transport_failure(
                                &mut failure,
                                "discover final page targets",
                                error,
                            ),
                        }
                    }
                }
                Ok(OperatorResponse::EndOfInput) => {
                    failure.primary_failure = Some(
                        "operator aborted bootstrap because stdin closed before Enter confirmation"
                            .to_owned(),
                    );
                    terminal_state = CaptureRunState::AbortedByOperator;
                    if let Err(error) = run.append_event(
                        "init_operator_aborted",
                        json!({"reason": "stdin_eof_before_confirmation"}),
                    ) {
                        failure.journal_failure = Some(error);
                    }
                }
                Err(error) => {
                    failure.primary_failure = Some(format!("operator input failed: {error}"));
                    if let Err(journal_error) =
                        run.append_event("init_operator_input_failed", json!({"error": error}))
                    {
                        failure.journal_failure = Some(journal_error);
                    }
                }
            }
        }

        if let Err(error) = launched.shutdown(&mut run) {
            record_shutdown_failure(&mut failure, error);
        }
        match launched.cleanup_status() {
            Ok(status) if status.is_complete() => {}
            Ok(status) => {
                failure.cleanup_failure.get_or_insert_with(|| {
                    format!(
                        "cleanup incomplete (process_exited={}, harness_lock_absent={}, active_port_absent={})",
                        status.process_exited, status.harness_lock_absent, status.active_port_file_absent
                    )
                });
            }
            Err(error) => {
                failure
                    .cleanup_failure
                    .get_or_insert_with(|| error.to_string());
            }
        }
    }

    if failure.primary_failure.is_some() || failure.journal_failure.is_some() {
        if let Some(cleanup_failure) = recorded_launch_cleanup_failure(&run) {
            failure.cleanup_failure.get_or_insert(cleanup_failure);
        }
    }
    if failure.primary_failure.is_none()
        && failure.journal_failure.is_none()
        && failure.cleanup_failure.is_none()
        && final_target_url.is_some()
    {
        terminal_state = CaptureRunState::Completed;
    } else if terminal_state != CaptureRunState::AbortedByOperator {
        terminal_state = CaptureRunState::FailedBeforeMutation;
    }

    let terminal_event = run.append_event(
        "init_finished",
        json!({
            "terminal_state": terminal_state,
            "primary_failure": failure.primary_failure.as_deref(),
            "journal_failure": failure.journal_failure.as_deref(),
            "cleanup_failure": failure.cleanup_failure.as_deref(),
        }),
    );
    if let Err(error) = terminal_event {
        failure.journal_failure.get_or_insert(error);
        terminal_state = CaptureRunState::FailedBeforeMutation;
    }
    if let Err(error) = run.finish(terminal_state) {
        failure
            .journal_failure
            .get_or_insert_with(|| format!("finish bootstrap run: {error}"));
    }

    if failure.primary_failure.is_none()
        && failure.journal_failure.is_none()
        && failure.cleanup_failure.is_none()
    {
        if let Some(final_target_url) = final_target_url {
            return Ok(InitSuccess {
                final_target_url,
                run_path,
            });
        }
    }
    Err(failure)
}

fn record_transport_failure(failure: &mut InitFailure, context: &str, error: TransportError) {
    match error {
        TransportError::Journal(journal) => {
            failure.journal_failure.get_or_insert(journal);
        }
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure,
        } => {
            if let Some(primary) = primary_failure {
                failure
                    .primary_failure
                    .get_or_insert_with(|| format!("{context}: {primary}"));
            }
            failure.journal_failure.get_or_insert(journal_failure);
        }
        other => {
            failure
                .primary_failure
                .get_or_insert_with(|| format!("{context}: {other}"));
        }
    }
}

fn record_shutdown_failure(failure: &mut InitFailure, error: TransportError) {
    match error {
        TransportError::Journal(journal) => {
            failure.journal_failure.get_or_insert(journal);
        }
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure,
        } => {
            if let Some(primary) = primary_failure {
                failure
                    .cleanup_failure
                    .get_or_insert_with(|| format!("shutdown: {primary}"));
            }
            failure.journal_failure.get_or_insert(journal_failure);
        }
        other => {
            failure
                .cleanup_failure
                .get_or_insert_with(|| format!("shutdown: {other}"));
        }
    }
}

fn record_failed_launch_cleanup(failure: &mut InitFailure, run: &CaptureRun) {
    if let Some(cleanup_failure) = recorded_launch_cleanup_failure(run) {
        failure.cleanup_failure = Some(cleanup_failure);
    }
}

fn launch_failure_context(run: &CaptureRun) -> &'static str {
    if run
        .events()
        .iter()
        .any(|event| event.kind == "browser_process_started")
    {
        "Edge startup/process failure"
    } else if run.events().iter().any(|event| {
        event.kind == "devtools_pipe_setup_started" || event.kind == "devtools_pipe_setup_failed"
    }) {
        "pipe setup failure"
    } else {
        "failed before Edge launch"
    }
}

fn recorded_launch_cleanup_failure(run: &CaptureRun) -> Option<String> {
    let attempted = run.events().iter().any(|event| {
        event.kind == "devtools_pipe_setup_started" || event.kind == "browser_process_started"
    });
    let cleanup = run
        .events()
        .iter()
        .rev()
        .find(|event| event.kind == "browser_shutdown_cleanup");
    match cleanup {
        Some(event) if event.payload["cleanup_succeeded"] == true => None,
        Some(event) => Some(
            event.payload["cleanup_error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| "launch cleanup was not verified".to_owned()),
        ),
        None if attempted => Some("launch cleanup was not durably verified".to_owned()),
        None => None,
    }
}

/// Restrict final target acceptance to a normal HTTPS URL on the exact ChatGPT host.
#[must_use]
pub fn is_chatgpt_https_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}

/// Remove URL query/fragment data and credentials before target URL provenance is journaled.
#[must_use]
pub(crate) fn safe_target_url_for_journal(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "<invalid-url>".to_owned();
    };
    let is_chatgpt = url.scheme() == "https" && url.host_str() == Some("chatgpt.com");
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    if !is_chatgpt && url.scheme() != "about" {
        url.set_path("/");
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone)]
    struct MockOperator(OperatorResponse);

    impl InitOperator for MockOperator {
        fn confirm(&mut self) -> Result<OperatorResponse, String> {
            Ok(self.0)
        }
    }

    #[derive(Clone)]
    struct MockLauncher {
        targets: Vec<TargetInfo>,
        launch_error: Option<TransportError>,
        launch_process_started_on_error: bool,
        launch_cleanup_failure: bool,
        target_error: Option<TransportError>,
        shutdown_error: Option<TransportError>,
        cleanup: EdgeCleanupStatus,
        calls: Arc<Mutex<Vec<&'static str>>>,
        profile_marker: PathBuf,
    }

    struct MockBrowser {
        targets: Vec<TargetInfo>,
        target_error: Option<TransportError>,
        shutdown_error: Option<TransportError>,
        cleanup: EdgeCleanupStatus,
        calls: Arc<Mutex<Vec<&'static str>>>,
        profile_marker: PathBuf,
    }

    impl InitLauncher for MockLauncher {
        fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn InitBrowser>, TransportError> {
            self.calls.lock().unwrap().push("launch");
            if let Some(error) = &self.launch_error {
                if self.launch_process_started_on_error {
                    run.append_event(
                        "devtools_pipe_setup_started",
                        json!({"transport_mode": "pipe"}),
                    )
                    .unwrap();
                    run.append_event(
                        "browser_process_started",
                        json!({"pid": 77, "transport_mode": "pipe"}),
                    )
                    .unwrap();
                }
                if self.launch_cleanup_failure {
                    run.append_event(
                        "browser_shutdown_cleanup",
                        json!({
                            "cleanup_succeeded": false,
                            "cleanup_error": "simulated launch cleanup failure",
                        }),
                    )
                    .unwrap();
                }
                return Err(error.clone());
            }
            if let Some(parent) = self.profile_marker.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            if !self.profile_marker.exists() {
                std::fs::write(&self.profile_marker, b"new-profile-state").unwrap();
            }
            run.append_event(
                "browser_process_started",
                json!({"pid": 77, "transport_mode": "pipe"}),
            )
            .unwrap();
            Ok(Box::new(MockBrowser {
                targets: self.targets.clone(),
                target_error: self.target_error.clone(),
                shutdown_error: self.shutdown_error.clone(),
                cleanup: self.cleanup.clone(),
                calls: self.calls.clone(),
                profile_marker: self.profile_marker.clone(),
            }))
        }
    }

    impl InitBrowser for MockBrowser {
        fn page_targets(
            &mut self,
            run: &mut CaptureRun,
        ) -> Result<Vec<TargetInfo>, TransportError> {
            self.calls.lock().unwrap().push("targets");
            if let Some(error) = self.target_error.take() {
                return Err(error);
            }
            run.append_event(
                "cdp_page_targets_discovered",
                json!({"transport_mode": "pipe"}),
            )
            .map_err(TransportError::Journal)?;
            Ok(self.targets.clone())
        }

        fn shutdown(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
            self.calls.lock().unwrap().push("shutdown");
            let cleanup_ok = self.cleanup.is_complete();
            run.append_event(
                "browser_shutdown_cleanup",
                json!({
                    "process_exited": self.cleanup.process_exited,
                    "harness_lock_absent": self.cleanup.harness_lock_absent,
                    "active_port_file_absent": self.cleanup.active_port_file_absent,
                    "cleanup_succeeded": cleanup_ok,
                    "cleanup_error": if cleanup_ok { Value::Null } else { json!("simulated cleanup failure") },
                }),
            )
            .map_err(TransportError::Journal)?;
            assert!(
                self.profile_marker.exists(),
                "bootstrap must preserve profile data"
            );
            if let Some(error) = self.shutdown_error.take() {
                Err(error)
            } else {
                Ok(())
            }
        }

        fn cleanup_status(&self) -> Result<EdgeCleanupStatus, TransportError> {
            Ok(self.cleanup.clone())
        }
    }

    fn target(url: &str) -> TargetInfo {
        TargetInfo {
            id: "page-1".to_owned(),
            target_type: "page".to_owned(),
            title: "not journaled".to_owned(),
            url: url.to_owned(),
            websocket_url: None,
        }
    }

    fn fixture(
        label: &str,
        targets: Vec<TargetInfo>,
        response: OperatorResponse,
    ) -> (
        PathBuf,
        MockLauncher,
        MockOperator,
        Arc<Mutex<Vec<&'static str>>>,
    ) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("chatarium-init-{label}-{stamp}"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let launcher = MockLauncher {
            targets,
            launch_error: None,
            launch_process_started_on_error: false,
            launch_cleanup_failure: false,
            target_error: None,
            shutdown_error: None,
            cleanup: EdgeCleanupStatus {
                process_exited: true,
                harness_lock_absent: true,
                active_port_file_absent: true,
            },
            calls: calls.clone(),
            profile_marker: base.join("profile").join("existing.marker"),
        };
        (base, launcher, MockOperator(response), calls)
    }

    #[test]
    fn chatgpt_target_validation_accepts_normal_https_paths_and_rejects_other_hosts_or_schemes() {
        for raw in [
            "https://chatgpt.com/",
            "https://chatgpt.com/c/abc123",
            "https://chatgpt.com/settings",
        ] {
            assert!(is_chatgpt_https_url(raw), "rejected {raw}");
        }
        for raw in [
            "http://chatgpt.com/",
            "https://chatgpt.com.evil.example/",
            "https://evilchatgpt.com/",
            "https://sub.chatgpt.com/",
            "https://user@chatgpt.com/",
            "https://chatgpt.com:8443/",
            "not a URL",
        ] {
            assert!(!is_chatgpt_https_url(raw), "accepted {raw}");
        }
    }

    #[test]
    fn stdin_confirmation_requires_one_terminated_line_and_treats_eof_as_abort() {
        assert_eq!(parse_operator_line("\n"), OperatorResponse::Confirmed);
        assert_eq!(
            parse_operator_line("confirm\r\n"),
            OperatorResponse::Confirmed
        );
        assert_eq!(parse_operator_line(""), OperatorResponse::EndOfInput);
        assert_eq!(parse_operator_line("confirm"), OperatorResponse::EndOfInput);
    }

    #[test]
    fn target_url_journal_projection_removes_query_fragment_and_external_auth_path() {
        assert_eq!(
            safe_target_url_for_journal("https://chatgpt.com/c/abc?access_token=secret#private"),
            "https://chatgpt.com/c/abc"
        );
        assert_eq!(
            safe_target_url_for_journal("https://login.example/authorize?code=secret"),
            "https://login.example/"
        );
        assert_eq!(
            safe_target_url_for_journal("https://user:pass@chatgpt.com/"),
            "https://chatgpt.com/"
        );
    }

    #[test]
    fn confirmation_success_preserves_existing_profile_and_never_claims_authentication() {
        let (base, launcher, mut operator, calls) = fixture(
            "success",
            vec![target(
                "https://chatgpt.com/c/abc?access_token=never-journal",
            )],
            OperatorResponse::Confirmed,
        );
        std::fs::create_dir_all(base.join("profile")).unwrap();
        std::fs::write(
            base.join("profile/existing.marker"),
            b"preexisting-session-state",
        )
        .unwrap();
        let success = run_init(&base, &launcher, &mut operator).unwrap();
        assert_eq!(success.final_target_url, "https://chatgpt.com/c/abc");
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["launch", "targets", "shutdown"]
        );
        assert_eq!(
            std::fs::read(base.join("profile/existing.marker")).unwrap(),
            b"preexisting-session-state"
        );
        let event_text = std::fs::read_to_string(success.run_path.join("events.jsonl")).unwrap();
        let events = event_text
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .any(|event| event["kind"] == "init_operator_confirmation_received")
        );
        let found = events
            .iter()
            .find(|event| event["kind"] == "init_chatgpt_target_found")
            .unwrap();
        assert_eq!(found["payload"]["authentication_verified"], false);
        assert!(!event_text.contains("never-journal"));
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(success.run_path.join("run.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["state"], "completed");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn auth_provider_redirect_is_allowed_until_final_post_confirmation_check() {
        let (base, launcher, mut operator, _) = fixture(
            "redirect",
            vec![
                target("https://login.example/authorize?code=private"),
                target("https://chatgpt.com/"),
            ],
            OperatorResponse::Confirmed,
        );
        assert!(run_init(&base, &launcher, &mut operator).is_ok());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn final_missing_http_and_lookalike_targets_fail_explicitly_and_preserve_profile() {
        for (label, raw) in [
            ("missing", "https://login.example/authorize?code=private"),
            ("http", "http://chatgpt.com/"),
            ("lookalike", "https://chatgpt.com.example/"),
        ] {
            let (base, launcher, mut operator, calls) =
                fixture(label, vec![target(raw)], OperatorResponse::Confirmed);
            let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
            assert!(
                failure
                    .primary_failure
                    .as_deref()
                    .unwrap()
                    .contains("no HTTPS page target")
            );
            assert!(failure.cleanup_failure.is_none());
            assert!(base.join("profile/existing.marker").exists());
            assert_eq!(calls.lock().unwrap().last(), Some(&"shutdown"));
            let event_text =
                std::fs::read_to_string(failure.run_path.unwrap().join("events.jsonl")).unwrap();
            assert!(event_text.contains("init_chatgpt_target_absent"));
            assert!(!event_text.contains("private"));
            let _ = std::fs::remove_dir_all(base);
        }
    }

    #[test]
    fn stdin_eof_is_an_explicit_abort_and_still_cleans_up_without_deleting_profile() {
        let (base, launcher, mut operator, calls) = fixture(
            "eof",
            vec![target("https://chatgpt.com/")],
            OperatorResponse::EndOfInput,
        );
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("stdin closed")
        );
        assert!(failure.journal_failure.is_none());
        assert!(failure.cleanup_failure.is_none());
        assert!(base.join("profile/existing.marker").exists());
        assert_eq!(calls.lock().unwrap().as_slice(), ["launch", "shutdown"]);
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(failure.run_path.unwrap().join("run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["state"], "aborted_by_operator");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn cleanup_failure_is_separate_from_confirmation_result_and_keeps_profile() {
        let (base, mut launcher, mut operator, calls) = fixture(
            "cleanup-failure",
            vec![target("https://chatgpt.com/")],
            OperatorResponse::Confirmed,
        );
        launcher.cleanup.process_exited = false;
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(failure.primary_failure.is_none());
        assert!(failure.journal_failure.is_none());
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("process_exited=false")
        );
        assert!(base.join("profile/existing.marker").exists());
        assert_eq!(calls.lock().unwrap().last(), Some(&"shutdown"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn target_validation_remains_primary_when_graceful_cleanup_fails() {
        let (base, mut launcher, mut operator, calls) = fixture(
            "target-and-cleanup-failure",
            vec![target("https://login.example/authorize?code=private")],
            OperatorResponse::Confirmed,
        );
        launcher.shutdown_error = Some(TransportError::Process(
            "graceful close timed out and owned fallback failed".to_owned(),
        ));

        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();

        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("no HTTPS page target")
        );
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("owned fallback failed")
        );
        assert!(failure.journal_failure.is_none());
        assert!(base.join("profile/existing.marker").exists());
        assert_eq!(calls.lock().unwrap().last(), Some(&"shutdown"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn primary_pipe_failure_and_journal_failure_remain_separate() {
        let (base, mut launcher, mut operator, _) =
            fixture("journal-failure", vec![], OperatorResponse::Confirmed);
        launcher.launch_error = Some(TransportError::DiagnosticJournalFailure {
            primary_failure: Some("pipe startup failed".to_owned()),
            journal_failure: "readiness event append failed".to_owned(),
        });
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("pipe startup failed")
        );
        assert_eq!(
            failure.journal_failure.as_deref(),
            Some("readiness event append failed")
        );
        assert!(failure.cleanup_failure.is_none());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn primary_journal_and_cleanup_failures_remain_three_distinct_results() {
        let (base, mut launcher, mut operator, _) =
            fixture("three-failures", vec![], OperatorResponse::Confirmed);
        std::fs::create_dir_all(base.join("profile")).unwrap();
        std::fs::write(base.join("profile/existing.marker"), b"keep").unwrap();
        launcher.launch_error = Some(TransportError::DiagnosticJournalFailure {
            primary_failure: Some("DevTools readiness failed".to_owned()),
            journal_failure: "readiness journal append failed".to_owned(),
        });
        launcher.launch_process_started_on_error = true;
        launcher.launch_cleanup_failure = true;
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert_eq!(
            failure.primary_failure.as_deref(),
            Some("Edge startup/process failure: DevTools readiness failed")
        );
        assert_eq!(
            failure.journal_failure.as_deref(),
            Some("readiness journal append failed")
        );
        assert_eq!(
            failure.cleanup_failure.as_deref(),
            Some("simulated launch cleanup failure")
        );
        assert_eq!(
            std::fs::read(base.join("profile/existing.marker")).unwrap(),
            b"keep"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn final_target_journal_failure_keeps_cleanup_separate_and_visible() {
        let (base, mut launcher, mut operator, calls) = fixture(
            "final-journal-failure",
            vec![target("https://chatgpt.com/")],
            OperatorResponse::Confirmed,
        );
        launcher.target_error = Some(TransportError::DiagnosticJournalFailure {
            primary_failure: None,
            journal_failure: "target journal append failed".to_owned(),
        });
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(failure.primary_failure.is_none());
        assert_eq!(
            failure.journal_failure.as_deref(),
            Some("target journal append failed")
        );
        assert!(failure.cleanup_failure.is_none());
        assert_eq!(calls.lock().unwrap().last(), Some(&"shutdown"));
        assert!(base.join("profile/existing.marker").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn init_browser_boundary_exposes_only_target_discovery_and_shutdown() {
        let (base, launcher, mut operator, calls) = fixture(
            "read-only-boundary",
            vec![target("https://chatgpt.com/")],
            OperatorResponse::Confirmed,
        );
        run_init(&base, &launcher, &mut operator).unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["launch", "targets", "shutdown"]
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
