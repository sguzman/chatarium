//! Plain-browser authentication and pipe-only verification for the dedicated Edge profile.

use crate::edge::{EdgeCleanupStatus, LaunchedEdge, LaunchedPlainEdge};
use crate::run::{CaptureRun, CaptureRunState};
use crate::transport::{TargetInfo, TransportError};
use serde_json::json;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Exact start URL requested by the profile bootstrap.
pub const CHATGPT_START_URL: &str = "https://chatgpt.com/";
/// Persistent profile identity used for journal and operator output.
pub const PROFILE_IDENTITY: &str = "%LOCALAPPDATA%\\Chatarium\\capture-browser\\edge-profile\\";
/// Single instruction shown before waiting for the owned browser window to close.
pub const OPERATOR_INSTRUCTION: &str = "Sign in to ChatGPT normally in this Chatarium Edge window. When ChatGPT is ready, close this Edge window.";

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

/// Browser lifecycle for the ordinary, non-debuggable human authentication phase.
pub trait PlainAuthBrowser: Send {
    /// Wait for an owned visible browser window, then treat its close as operator completion.
    fn wait_for_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError>;
    /// Whether the owned visible window close was observed, even if journaling that event failed.
    fn window_close_observed(&self) -> bool;
    /// Shut down only after the owned visible window has been closed.
    fn shutdown_after_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError>;
    /// Report whether the owned process has exited and the shared profile lock is gone.
    fn phase_boundary_released(&self) -> Result<bool, TransportError>;
    /// Leave an active browser and its profile lock intact when window ownership is uncertain.
    fn preserve_active_process(&mut self);
}

/// Narrow launch boundary used by the bootstrap coordinator and deterministic tests.
pub trait InitLauncher: Send + Sync {
    /// Launch plain Edge for the human phase without constructing a DevTools transport.
    fn launch_plain(
        &self,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PlainAuthBrowser>, TransportError>;
    /// Reopen the released dedicated profile at the fixed URL over anonymous pipes.
    fn launch(&self, run: &mut CaptureRun) -> Result<Box<dyn InitBrowser>, TransportError>;
}

/// Production launcher backed by the existing Windows anonymous-pipe Edge transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemInitLauncher;

struct LaunchedInitBrowser(LaunchedEdge);
struct LaunchedPlainAuthBrowser(LaunchedPlainEdge);

impl PlainAuthBrowser for LaunchedPlainAuthBrowser {
    fn wait_for_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        self.0.wait_for_window_close(run)
    }
    fn window_close_observed(&self) -> bool {
        self.0.window_close_observed()
    }
    fn shutdown_after_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
        self.0.shutdown_after_window_close(run)
    }
    fn phase_boundary_released(&self) -> Result<bool, TransportError> {
        self.0.phase_boundary_released()
    }
    fn preserve_active_process(&mut self) {
        self.0.preserve_active_process();
    }
}

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
    fn launch_plain(
        &self,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PlainAuthBrowser>, TransportError> {
        #[cfg(windows)]
        {
            LaunchedPlainEdge::launch_profile_auth(run).map(|browser| {
                Box::new(LaunchedPlainAuthBrowser(browser)) as Box<dyn PlainAuthBrowser>
            })
        }
        #[cfg(not(windows))]
        {
            let _ = run;
            Err(TransportError::Process("chatarium-capture init authentication phase requires Windows Edge window observation".to_owned()))
        }
    }

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

/// Human instruction boundary; browser-window close is observed as completion.
pub trait InitOperator {
    /// Present the one instruction without waiting for terminal input.
    fn present_instruction(&mut self) -> Result<(), String>;
}

/// Terminal instruction output used by the CLI; completion comes from the browser window.
#[derive(Debug, Default)]
pub struct StdinInitOperator;

impl InitOperator for StdinInitOperator {
    fn present_instruction(&mut self) -> Result<(), String> {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        writeln!(output, "{OPERATOR_INSTRUCTION}")
            .and_then(|()| output.flush())
            .map_err(|error| format!("flush operator instruction: {error}"))
    }
}

/// Test-only compatibility selector for deterministic operator outcomes.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperatorInstructionOutcome {
    Succeeds,
    Fails,
}

/// Successful window-close bootstrap. This does not prove authentication.
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

/// Run plain-browser authentication first, then pipe-only read-only profile verification.
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
    let mut final_target_url = None;
    let mut plain_browser: Option<Box<dyn PlainAuthBrowser>> = None;
    let mut verification_browser: Option<Box<dyn InitBrowser>> = None;
    let mut phase_boundary_released = false;

    if let Err(error) = run.start() {
        failure.journal_failure = Some(format!("start bootstrap run: {error}"));
    } else if let Err(error) = run.append_event(
        "init_started",
        json!({
            "operation_type":"profile_bootstrap",
            "auth_phase_transport":"plain_browser",
            "verification_phase_transport":"pipe",
            "profile_identity":PROFILE_IDENTITY,
            "requested_start_url":CHATGPT_START_URL,
            "authentication_verified":false,
        }),
    ) {
        failure.journal_failure = Some(error);
    }

    if failure.journal_failure.is_none() {
        match launcher.launch_plain(&mut run) {
            Ok(browser) => plain_browser = Some(browser),
            Err(error) => {
                let context = if run
                    .events()
                    .iter()
                    .any(|event| event.kind == "plain_auth_edge_process_started")
                {
                    "plain Edge startup/process failure"
                } else {
                    "plain authentication launch failure"
                };
                let retained_after_launch_journal_failure = error
                    .to_string()
                    .contains("browser and profile lock were retained safely");
                record_transport_failure(&mut failure, context, error);
                if retained_after_launch_journal_failure {
                    failure.cleanup_failure = Some("plain Edge and profile lock remain in place because launch evidence could not be durably written".to_owned());
                }
                record_failed_launch_cleanup(&mut failure, &run);
            }
        }
    }

    if let Some(browser) = plain_browser.as_mut() {
        if let Err(error) = run.append_event(
            "init_operator_instruction_presentation_started",
            json!({
                "completion_signal":"owned_window_closed"
            }),
        ) {
            failure.journal_failure = Some(error);
            browser.preserve_active_process();
            failure.cleanup_failure = Some("plain Edge left open and profile lock retained because operator-prompt evidence could not be durably recorded".to_owned());
        } else if let Err(error) = operator.present_instruction() {
            failure.primary_failure = Some(format!("present operator instruction: {error}"));
            if let Err(journal_error) = run.append_event(
                "init_operator_instruction_presentation_failed",
                json!({"error":error}),
            ) {
                failure.journal_failure = Some(journal_error);
            }
            browser.preserve_active_process();
            failure.cleanup_failure = Some("plain Edge left open and profile lock retained because its visible window may still be in use".to_owned());
        } else if let Err(error) = run.append_event(
            "init_operator_instruction_presented",
            json!({"instruction":OPERATOR_INSTRUCTION, "completion_signal":"owned_window_closed"}),
        ) {
            failure.journal_failure = Some(error);
            browser.preserve_active_process();
            failure.cleanup_failure = Some("plain Edge left open and profile lock retained because operator-prompt evidence could not be durably recorded".to_owned());
        } else {
            if let Err(error) = browser.wait_for_window_close(&mut run) {
                record_transport_failure(&mut failure, "plain authentication phase", error);
            }
            if browser.window_close_observed() {
                if let Err(error) = browser.shutdown_after_window_close(&mut run) {
                    record_shutdown_failure(&mut failure, error);
                }
                match browser.phase_boundary_released() {
                    Ok(true) => {
                        phase_boundary_released = true;
                        if let Err(error) = run.append_event(
                            "profile_phase_boundary_released",
                            json!({"plain_process_tree_exited":true, "harness_lock_absent":true, "profile_preserved":true}),
                        ) {
                            failure.journal_failure = Some(error);
                        }
                    }
                    Ok(false) => {
                        failure.cleanup_failure.get_or_insert_with(|| "plain Edge process tree or profile lock remains; pipe verification was not started".to_owned());
                    }
                    Err(error) => {
                        failure
                            .cleanup_failure
                            .get_or_insert_with(|| error.to_string());
                    }
                };
            } else {
                match browser.phase_boundary_released() {
                    Ok(true) => {}
                    _ => {
                        browser.preserve_active_process();
                        failure.cleanup_failure.get_or_insert_with(|| "plain Edge retained with its profile lock because browser-window close was not established".to_owned());
                    }
                }
            }
        }
    }

    if failure.primary_failure.is_some() || failure.journal_failure.is_some() {
        if let Some(cleanup_failure) = recorded_plain_launch_cleanup_failure(&run) {
            failure.cleanup_failure.get_or_insert(cleanup_failure);
        }
    }
    if !phase_boundary_released
        && failure.primary_failure.is_none()
        && failure.journal_failure.is_none()
        && failure.cleanup_failure.is_none()
    {
        failure.primary_failure =
            Some("plain authentication phase did not release the dedicated profile".to_owned());
    }

    if phase_boundary_released
        && failure.primary_failure.is_none()
        && failure.journal_failure.is_none()
        && failure.cleanup_failure.is_none()
    {
        if let Err(error) = run.append_event(
            "verification_phase_started",
            json!({"transport":"pipe", "profile_identity":PROFILE_IDENTITY, "requested_start_url":CHATGPT_START_URL}),
        ) {
            failure.journal_failure = Some(error);
        } else {
            match launcher.launch(&mut run) {
                Ok(browser) => verification_browser = Some(browser),
                Err(error) => {
                    let context = launch_failure_context(&run);
                    record_transport_failure(&mut failure, context, error);
                    record_failed_launch_cleanup(&mut failure, &run);
                }
            }
        }
    }

    if let Some(browser) = verification_browser.as_mut() {
        match browser.page_targets(&mut run) {
            Ok(targets) => {
                let observed_urls = targets
                    .iter()
                    .map(|target| safe_target_url_for_journal(&target.url))
                    .collect::<Vec<_>>();
                if let Err(error) = run.append_event(
                    "init_final_page_targets_observed",
                    json!({"page_target_urls":observed_urls, "transport":"pipe"}),
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
                            "target_url":url,
                            "operator_completed_by_window_close":true,
                            "authentication_verified":false,
                        }),
                    ) {
                        failure.journal_failure = Some(error);
                    } else {
                        final_target_url = Some(url);
                    }
                } else {
                    failure.primary_failure = Some(format!(
                        "plain browser was closed, but pipe verification found no HTTPS page target on exact chatgpt.com host; observed targets: {}",
                        observed_urls.join(", ")
                    ));
                    if let Err(error) = run.append_event(
                        "init_chatgpt_target_absent",
                        json!({"page_target_urls":observed_urls, "operator_completed_by_window_close":true}),
                    ) {
                        failure.journal_failure = Some(error);
                    }
                }
            }
            Err(error) => {
                record_transport_failure(&mut failure, "discover final page targets", error)
            }
        }
        if let Err(error) = browser.shutdown(&mut run) {
            record_shutdown_failure(&mut failure, error);
        }
        match browser.cleanup_status() {
            Ok(status) if status.is_complete() => {}
            Ok(status) => {
                failure.cleanup_failure.get_or_insert_with(|| format!(
                    "verification cleanup incomplete (process_exited={}, harness_lock_absent={}, active_port_absent={})",
                    status.process_exited, status.harness_lock_absent, status.active_port_file_absent
                ));
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
    let mut terminal_state = if failure.primary_failure.is_none()
        && failure.journal_failure.is_none()
        && failure.cleanup_failure.is_none()
        && final_target_url.is_some()
    {
        CaptureRunState::Completed
    } else {
        CaptureRunState::FailedBeforeMutation
    };

    if let Err(error) = run.append_event(
        "init_finished",
        json!({
            "terminal_state":terminal_state,
            "primary_failure":failure.primary_failure.as_deref(),
            "journal_failure":failure.journal_failure.as_deref(),
            "cleanup_failure":failure.cleanup_failure.as_deref(),
            "authentication_verified":false,
        }),
    ) {
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

fn recorded_plain_launch_cleanup_failure(run: &CaptureRun) -> Option<String> {
    let cleanup = run.events().iter().rev().find(|event| {
        event.kind == "plain_auth_browser_cleanup" || event.kind == "browser_shutdown_cleanup"
    });
    match cleanup {
        Some(event) if event.payload["cleanup_succeeded"] == true => None,
        Some(event) => Some(
            event.payload["cleanup_error"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| "plain Edge cleanup was not verified".to_owned()),
        ),
        None => None,
    }
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
    struct MockOperator(OperatorInstructionOutcome);

    impl InitOperator for MockOperator {
        fn present_instruction(&mut self) -> Result<(), String> {
            match self.0 {
                OperatorInstructionOutcome::Succeeds => Ok(()),
                OperatorInstructionOutcome::Fails => {
                    Err("simulated terminal output failure".to_owned())
                }
            }
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
        plain_boundary_released: bool,
    }

    struct MockPlainAuthBrowser {
        calls: Arc<Mutex<Vec<&'static str>>>,
        boundary_released: bool,
    }

    impl PlainAuthBrowser for MockPlainAuthBrowser {
        fn wait_for_window_close(&mut self, run: &mut CaptureRun) -> Result<(), TransportError> {
            self.calls.lock().unwrap().push("window_close");
            run.append_event(
                "auth_phase_started",
                json!({"auth_phase_transport":"plain_browser", "remote_debugging":false}),
            )
            .map_err(TransportError::Journal)?;
            run.append_event(
                "auth_phase_completed_by_window_close",
                json!({"owned_window_seen":true}),
            )
            .map_err(TransportError::Journal)?;
            Ok(())
        }
        fn window_close_observed(&self) -> bool {
            true
        }
        fn shutdown_after_window_close(
            &mut self,
            run: &mut CaptureRun,
        ) -> Result<(), TransportError> {
            self.calls.lock().unwrap().push("plain_shutdown");
            run.append_event("plain_auth_browser_cleanup", json!({"process_exited":self.boundary_released, "harness_lock_absent":self.boundary_released, "cleanup_succeeded":self.boundary_released})).map_err(TransportError::Journal)?;
            Ok(())
        }
        fn phase_boundary_released(&self) -> Result<bool, TransportError> {
            Ok(self.boundary_released)
        }
        fn preserve_active_process(&mut self) {
            self.calls.lock().unwrap().push("preserve_active");
        }
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
        fn launch_plain(
            &self,
            _run: &mut CaptureRun,
        ) -> Result<Box<dyn PlainAuthBrowser>, TransportError> {
            self.calls.lock().unwrap().push("plain_launch");
            Ok(Box::new(MockPlainAuthBrowser {
                calls: self.calls.clone(),
                boundary_released: self.plain_boundary_released,
            }))
        }

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
        response: OperatorInstructionOutcome,
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
            plain_boundary_released: true,
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
    fn window_close_success_preserves_existing_profile_and_never_claims_authentication() {
        let (base, launcher, mut operator, calls) = fixture(
            "success",
            vec![target(
                "https://chatgpt.com/c/abc?access_token=never-journal",
            )],
            OperatorInstructionOutcome::Succeeds,
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
            [
                "plain_launch",
                "window_close",
                "plain_shutdown",
                "launch",
                "targets",
                "shutdown"
            ]
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
                .any(|event| event["kind"] == "auth_phase_completed_by_window_close")
        );
        let event_kind = |kind: &str| {
            events
                .iter()
                .position(|event| event["kind"] == kind)
                .unwrap()
        };
        assert!(
            event_kind("auth_phase_completed_by_window_close")
                < event_kind("profile_phase_boundary_released")
        );
        assert!(
            event_kind("profile_phase_boundary_released")
                < event_kind("verification_phase_started")
        );
        let auth_start = events
            .iter()
            .find(|event| event["kind"] == "auth_phase_started")
            .unwrap();
        assert_eq!(
            auth_start["payload"]["auth_phase_transport"],
            "plain_browser"
        );
        assert_eq!(auth_start["payload"]["remote_debugging"], false);
        assert!(!event_text.contains("init_operator_confirmation_received"));
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
            OperatorInstructionOutcome::Succeeds,
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
            let (base, launcher, mut operator, calls) = fixture(
                label,
                vec![target(raw)],
                OperatorInstructionOutcome::Succeeds,
            );
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
    fn operator_instruction_failure_prevents_pipe_verification_and_preserves_profile() {
        let (base, launcher, mut operator, calls) = fixture(
            "eof",
            vec![target("https://chatgpt.com/")],
            OperatorInstructionOutcome::Fails,
        );
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(
            failure
                .primary_failure
                .as_deref()
                .unwrap()
                .contains("terminal output failure")
        );
        assert!(failure.journal_failure.is_none());
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("profile lock retained")
        );
        std::fs::create_dir_all(base.join("profile")).unwrap();
        std::fs::write(base.join("profile/existing.marker"), b"keep").unwrap();
        assert!(base.join("profile/existing.marker").exists());
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["plain_launch", "preserve_active"]
        );
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(failure.run_path.unwrap().join("run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["state"], "failed_before_mutation");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn cleanup_failure_is_separate_from_confirmation_result_and_keeps_profile() {
        let (base, mut launcher, mut operator, calls) = fixture(
            "cleanup-failure",
            vec![target("https://chatgpt.com/")],
            OperatorInstructionOutcome::Succeeds,
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
            OperatorInstructionOutcome::Succeeds,
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
        let (base, mut launcher, mut operator, _) = fixture(
            "journal-failure",
            vec![],
            OperatorInstructionOutcome::Succeeds,
        );
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
        let (base, mut launcher, mut operator, _) = fixture(
            "three-failures",
            vec![],
            OperatorInstructionOutcome::Succeeds,
        );
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
            OperatorInstructionOutcome::Succeeds,
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
            OperatorInstructionOutcome::Succeeds,
        );
        run_init(&base, &launcher, &mut operator).unwrap();
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                "plain_launch",
                "window_close",
                "plain_shutdown",
                "launch",
                "targets",
                "shutdown"
            ]
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn pipe_verification_never_starts_before_plain_profile_release() {
        let (base, mut launcher, mut operator, calls) = fixture(
            "phase-boundary",
            vec![target("https://chatgpt.com/")],
            OperatorInstructionOutcome::Succeeds,
        );
        launcher.plain_boundary_released = false;
        let failure = run_init(&base, &launcher, &mut operator).unwrap_err();
        assert!(failure.primary_failure.is_none());
        assert!(
            failure
                .cleanup_failure
                .as_deref()
                .unwrap()
                .contains("process tree or profile lock remains")
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            ["plain_launch", "window_close", "plain_shutdown"]
        );
        assert!(!calls.lock().unwrap().contains(&"launch"));
        let _ = std::fs::remove_dir_all(base);
    }
}
