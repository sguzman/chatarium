use crate::diagnostics;
use eframe::egui;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum BridgeCommand {
    RefreshSession,
    SignIn,
    CancelSignIn,
    ListModels {
        max_attempts: u8,
    },
    StreamResponse {
        request_id: String,
        model: String,
        input: Value,
        instructions: Option<String>,
        request_patch: Value,
    },
    ProbeResponse {
        request_id: String,
        model: String,
        input: Value,
        request_patch: Value,
    },
    CancelResponse {
        target_request_id: String,
    },
    Disconnect,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub status: String,
    pub sharing: bool,
    pub profile_id: Option<String>,
    pub profile_label: Option<String>,
    pub email: Option<String>,
    pub error_message: Option<String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: "disconnected".to_owned(),
            sharing: false,
            profile_id: None,
            profile_label: None,
            email: None,
            error_message: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub slug: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub status: Option<u16>,
    pub param: Option<String>,
    pub upstream_request_id: Option<String>,
    pub response_shape: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeEvent {
    Ready,
    Session(SessionState),
    Models(Vec<Model>),
    Delta {
        request_id: String,
        delta: String,
    },
    TransportProgress {
        request_id: String,
        stage: String,
    },
    AgentActivity {
        request_id: String,
        frame: String,
    },
    ResponseCompleted {
        request_id: String,
        text: String,
    },
    CommandSucceeded {
        request_id: Option<String>,
    },
    Failed {
        request_id: Option<String>,
        error: BridgeError,
    },
    RuntimeUnavailable(String),
}

pub struct BridgeRuntime {
    command_tx: Option<Sender<BridgeCommand>>,
    event_rx: Receiver<BridgeEvent>,
    worker: Option<JoinHandle<()>>,
}

impl BridgeRuntime {
    #[must_use]
    pub fn start(repaint: &egui::Context) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let repaint = repaint.clone();
        let worker = thread::Builder::new()
            .name("chatarium-siwc-bridge".to_owned())
            .spawn(move || bridge_worker(command_rx, event_tx, repaint))
            .ok();

        Self {
            command_tx: worker.as_ref().map(|_| command_tx),
            event_rx,
            worker,
        }
    }

    pub fn send(&self, command: BridgeCommand) -> Result<(), String> {
        self.command_tx
            .as_ref()
            .ok_or_else(|| "Sign in with ChatGPT bridge worker is unavailable".to_owned())?
            .send(command)
            .map_err(|error| format!("Sign in with ChatGPT bridge stopped: {error}"))
    }

    pub fn drain(&self) -> Vec<BridgeEvent> {
        self.event_rx.try_iter().collect()
    }
}

impl Drop for BridgeRuntime {
    fn drop(&mut self) {
        if let Some(sender) = self.command_tx.take() {
            let _ = sender.send(BridgeCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn bridge_worker(
    commands: Receiver<BridgeCommand>,
    events: Sender<BridgeEvent>,
    repaint: egui::Context,
) {
    let node_version = ProcessCommand::new("node").arg("--version").output();
    let node_version = match node_version {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        Ok(output) => {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "Node.js could not report its version (exit status {})",
                    output.status
                )),
            );
            return;
        }
        Err(error) => {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "Node.js 22 or newer is required for Sign in with ChatGPT: {error}"
                )),
            );
            return;
        }
    };
    let node_major = node_version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse::<u64>().ok());
    if !node_major.is_some_and(|major| major >= 22) {
        send_event(
            &events,
            &repaint,
            BridgeEvent::RuntimeUnavailable(format!(
                "Node.js 22 or newer is required for Sign in with ChatGPT; found {node_version}"
            )),
        );
        return;
    }

    let bootstrap = bootstrap_script_path();
    let prepared = ProcessCommand::new("node")
        .arg(&bootstrap)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match prepared {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let detail = if detail.is_empty() {
                format!("bootstrap exited with {}", output.status)
            } else {
                detail
            };
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "could not prepare the pinned Sign in with ChatGPT runtime: {detail}"
                )),
            );
            return;
        }
        Err(error) => {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "could not run Node.js for the Sign in with ChatGPT runtime at {}: {error}",
                    bootstrap.display()
                )),
            );
            return;
        }
    }

    let script = bridge_script_path();
    let child = ProcessCommand::new("node")
        .arg(&script)
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_ORG_ID")
        .env_remove("OPENAI_PROJECT_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "could not start Node.js Sign in with ChatGPT bridge at {}: {error}",
                    script.display()
                )),
            );
            return;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        send_event(
            &events,
            &repaint,
            BridgeEvent::RuntimeUnavailable(
                "Sign in with ChatGPT bridge did not expose stdout".to_owned(),
            ),
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let Some(mut stdin) = child.stdin.take() else {
        send_event(
            &events,
            &repaint,
            BridgeEvent::RuntimeUnavailable(
                "Sign in with ChatGPT bridge did not expose stdin".to_owned(),
            ),
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    };

    let reader_events = events.clone();
    let reader_repaint = repaint.clone();
    let reader = thread::Builder::new()
        .name("chatarium-siwc-reader".to_owned())
        .spawn(move || {
            forward_bridge_output(BufReader::new(stdout), &reader_events, &reader_repaint);
        });

    // A dead sidecar must not retain a writable-looking command channel.
    while let Some(command) = recv_command_while_sidecar_alive(&commands, &mut child) {
        if matches!(command, BridgeCommand::Shutdown) {
            break;
        }

        let value = command_json(command);
        if writeln!(stdin, "{value}").is_err() || stdin.flush().is_err() {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(
                    "Sign in with ChatGPT bridge input closed".to_owned(),
                ),
            );
            break;
        }
    }

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    if let Ok(reader) = reader {
        let _ = reader.join();
    }
}

// The reader independently reports EOF or the specific fatal error.
// Always surface EOF to the desktop so an OAuth flow or active response cannot
// remain indefinitely in the apparent "connecting"/"streaming" state.
// Check child liveness before consuming a queued command and after each
// bounded idle wait. Otherwise a crashed sidecar leaves an idle writer alive
// indefinitely, and later commands are accepted despite having no recipient.
fn recv_command_while_sidecar_alive(
    commands: &Receiver<BridgeCommand>,
    child: &mut Child,
) -> Option<BridgeCommand> {
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {}
        }
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => return Some(command),
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn forward_bridge_output(
    reader: impl BufRead,
    events: &Sender<BridgeEvent>,
    repaint: &egui::Context,
) {
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if let Some(event) = parse_bridge_event(&line) {
                    let terminal = matches!(event, BridgeEvent::RuntimeUnavailable(_));
                    send_event(events, repaint, event);
                    // Fatal DevKit errors are more actionable than a generic
                    // EOF notice. Emit exactly one terminal failure.
                    if terminal {
                        return;
                    }
                } else if let Some(notice) = read_retry_diagnostic(&line) {
                    // The desktop does not yet have a durable read-retry audit
                    // event. Preserve a safe local operational diagnostic rather
                    // than silently dropping the bridge's retry notice.
                    diagnostics::info("siwc-retry", notice);
                } else if let Some(notice) = agent_activity_diagnostic(&line) {
                    // Read-only diagnostic until the durable native activity
                    // projection is ready; never claim this is journaled.
                    diagnostics::debug("siwc-agent", notice);
                }
            }
            Err(error) => {
                send_event(
                    events,
                    repaint,
                    BridgeEvent::RuntimeUnavailable(format!(
                        "Sign in with ChatGPT bridge output failed: {error}"
                    )),
                );
                return;
            }
        }
    }
    send_event(
        events,
        repaint,
        BridgeEvent::RuntimeUnavailable(
            "Sign in with ChatGPT bridge exited unexpectedly; the pending sign-in or response has not been confirmed.".to_owned(),
        ),
    );
}

fn send_event(events: &Sender<BridgeEvent>, repaint: &egui::Context, event: BridgeEvent) {
    if matches!(&event, BridgeEvent::RuntimeUnavailable(_)) {
        // A failed sidecar cannot remain authenticated just because its last
        // subscription snapshot said "connected". This also resets stale
        // model discovery state in the desktop before it handles the failure.
        if events
            .send(BridgeEvent::Session(SessionState::default()))
            .is_err()
        {
            return;
        }
    }
    if events.send(event).is_ok() {
        repaint.request_repaint();
    }
}

fn bootstrap_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/siwc-bridge/bootstrap.mjs")
}

fn bridge_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/siwc-bridge/bridge.mjs")
}

fn command_json(command: BridgeCommand) -> Value {
    match command {
        BridgeCommand::RefreshSession => json!({
            "type": "session",
            "request_id": "session"
        }),
        BridgeCommand::SignIn => json!({
            "type": "sign_in",
            "request_id": "sign-in"
        }),
        BridgeCommand::CancelSignIn => json!({
            "type": "cancel_sign_in",
            "request_id": "cancel-sign-in"
        }),
        BridgeCommand::ListModels { max_attempts } => json!({
            "type": "models",
            "request_id": "models",
            "read_retry_policy": { "maxAttempts": max_attempts }
        }),
        BridgeCommand::StreamResponse {
            request_id,
            model,
            input,
            instructions,
            request_patch,
        } => json!({
            "type": "stream_response",
            "request_id": request_id,
            "model": model,
            "input": input,
            "instructions": instructions,
            "request_patch": request_patch
        }),
        BridgeCommand::ProbeResponse {
            request_id,
            model,
            input,
            request_patch,
        } => json!({
            "type": "probe_response",
            "request_id": request_id,
            "model": model,
            "input": input,
            "request_patch": request_patch
        }),
        BridgeCommand::CancelResponse { target_request_id } => json!({
            "type": "cancel_response",
            "request_id": format!("cancel:{target_request_id}"),
            "target_request_id": target_request_id
        }),
        BridgeCommand::Disconnect => json!({
            "type": "disconnect",
            "request_id": "disconnect"
        }),
        BridgeCommand::Shutdown => unreachable!("shutdown is handled before encoding"),
    }
}

fn parse_bridge_event(line: &str) -> Option<BridgeEvent> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if contains_credential_field(&value) {
        return Some(BridgeEvent::RuntimeUnavailable(
            "Sign in with ChatGPT bridge output violated the credential boundary".to_owned(),
        ));
    }
    match value.get("type")?.as_str()? {
        "ready" => Some(BridgeEvent::Ready),
        "session" => parse_session(value.get("session")?).map(BridgeEvent::Session),
        "agent_activity" => {
            // The sidecar frame is bounded and validated before forwarding.
            // The durable worker separately parses and strips unknown fields.
            let request_id = value.get("request_id")?.as_str()?.to_owned();
            agent_activity_diagnostic(line)?;
            Some(BridgeEvent::AgentActivity {
                request_id,
                frame: line.to_owned(),
            })
        }
        "transport_progress" => {
            let request_id = value.get("request_id")?.as_str()?.to_owned();
            let stage = value.get("stage")?.as_str()?;
            if !matches!(stage, "wire_attempted" | "acknowledged") {
                return None;
            }
            Some(BridgeEvent::TransportProgress {
                request_id,
                stage: stage.to_owned(),
            })
        }
        "delta" => Some(BridgeEvent::Delta {
            request_id: value.get("request_id")?.as_str()?.to_owned(),
            delta: value.get("delta")?.as_str()?.to_owned(),
        }),
        "result" => {
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let result = value.get("result")?;
            if let Some(session) = result.get("session").and_then(parse_session) {
                return Some(BridgeEvent::Session(session));
            }
            if let Some(models) = result.get("models").and_then(Value::as_array) {
                let models = models.iter().filter_map(parse_model).collect::<Vec<_>>();
                return Some(BridgeEvent::Models(models));
            }
            if let Some(text) = result.get("text").and_then(Value::as_str) {
                return Some(BridgeEvent::ResponseCompleted {
                    request_id: request_id.unwrap_or_default(),
                    text: text.to_owned(),
                });
            }
            Some(BridgeEvent::CommandSucceeded { request_id })
        }
        "error" | "fatal" => {
            let error = parse_error(value.get("error")?)?;
            if value.get("type").and_then(Value::as_str) == Some("fatal") {
                Some(BridgeEvent::RuntimeUnavailable(error.message))
            } else {
                Some(BridgeEvent::Failed {
                    request_id: value
                        .get("request_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    error,
                })
            }
        }
        _ => None,
    }
}

fn agent_activity_diagnostic(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if contains_credential_field(&value)
        || value.get("type")?.as_str()? != "agent_activity"
        || value.get("source")?.as_str()? != "siwc_bridge_observation"
    {
        return None;
    }
    let stage = value.get("stage")?.as_str()?;
    if !matches!(
        stage,
        "started"
            | "wire_attempted"
            | "acknowledged"
            | "http_rejected"
            | "first_text_delta"
            | "text_delta"
            | "completed"
            | "failed"
    ) {
        return None;
    }
    let request_id = value.get("request_id")?.as_str()?;
    if request_id.is_empty()
        || request_id.len() > 256
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return None;
    }
    let sequence = value.get("sequence")?.as_u64()?;
    let elapsed_ms = value.get("elapsed_ms")?.as_f64()?;
    if sequence == 0
        || sequence > 100_000
        || !elapsed_ms.is_finite()
        || !(0.0..=86_400_000.0).contains(&elapsed_ms)
    {
        return None;
    }
    let observed_at = value.get("observed_at_utc")?.as_str()?;
    // The source timestamp is retained on the wire; log only validated
    // metadata and never echo arbitrary upstream strings or delta content.
    if observed_at.len() != 24
        || !observed_at.ends_with('Z')
        || !observed_at.is_ascii()
        || observed_at.bytes().any(|byte| byte.is_ascii_control())
    {
        return None;
    }
    let detail = if matches!(stage, "first_text_delta" | "text_delta") {
        let chars = value.get("delta_chars")?.as_u64()?;
        if chars > 1_000_000 {
            return None;
        }
        format!(" chars={chars}")
    } else if stage == "http_rejected" {
        let status = value.get("http_status")?.as_u64()?;
        if !(400..=599).contains(&status) {
            return None;
        }
        format!(" HTTP {status}")
    } else if matches!(stage, "completed" | "failed") {
        let evidence = value.get("delivery_evidence")?.as_str()?;
        if !matches!(
            evidence,
            "not_dispatched"
                | "wire_unconfirmed"
                | "http_acknowledged"
                | "http_rejected"
                | "assistant_delta_seen"
        ) {
            return None;
        }
        if evidence == "http_rejected" {
            let status = value.get("http_status")?.as_u64()?;
            if !(400..=599).contains(&status) {
                return None;
            }
            format!(" evidence={evidence} HTTP {status}")
        } else {
            if value.get("http_status").is_some() {
                return None;
            }
            format!(" evidence={evidence}")
        }
    } else {
        String::new()
    };
    Some(format!(
        "response={} seq={sequence} stage={stage} elapsed={elapsed_ms:.3}ms{detail}",
        diagnostics::short_id(request_id)
    ))
}

fn read_retry_diagnostic(line: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    if contains_credential_field(&value)
        || value.get("type")?.as_str()? != "read_retry_scheduled"
        || value.get("operation")?.as_str()? != "models"
        || value.get("stage")?.as_str()? != "model_discovery"
    {
        return None;
    }
    let category = value.get("category")?.as_str()?;
    if !matches!(
        category,
        "rate_limit" | "http_transient" | "network_transient"
    ) {
        return None;
    }
    let attempt = value.get("attempt")?.as_u64()?;
    let next_attempt = value.get("next_attempt")?.as_u64()?;
    let delay_ms = value.get("delay_ms")?.as_u64()?;
    if attempt == 0 || attempt >= 5 || next_attempt != attempt + 1 || delay_ms > 30_000 {
        return None;
    }
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .filter(|status| (100..=599).contains(status))
        .map(|status| format!(" HTTP {status}"))
        .unwrap_or_default();
    let raw_id = value.get("request_id")?.as_str()?;
    if raw_id.is_empty()
        || raw_id.len() > 256
        || !raw_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        // Nothing untrusted may inject newlines or terminal escapes into logs.
        return None;
    }
    let request_id = diagnostics::short_id(raw_id);
    Some(format!(
        "read-only models request={request_id} {category}{status} attempt={attempt}->{next_attempt} delay={delay_ms}ms"
    ))
}

fn contains_credential_field(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_credential_field),
        Value::Object(object) => object.iter().any(|(key, nested)| {
            let normalized = key
                .chars()
                .filter(|character| *character != '_' && *character != '-')
                .flat_map(char::to_lowercase)
                .collect::<String>();
            matches!(
                normalized.as_str(),
                "accesstoken" | "refreshtoken" | "idtoken" | "authorization" | "cookie" | "cookies"
            ) || contains_credential_field(nested)
        }),
        _ => false,
    }
}

fn parse_session(value: &Value) -> Option<SessionState> {
    Some(SessionState {
        status: value.get("status")?.as_str()?.to_owned(),
        sharing: value
            .get("sharing")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        profile_id: value
            .get("profileId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        profile_label: value
            .get("profileLabel")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        email: value
            .pointer("/identity/email")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        error_message: value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn parse_model(value: &Value) -> Option<Model> {
    Some(Model {
        slug: value.get("slug")?.as_str()?.to_owned(),
        display_name: value
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_else(|| value.get("slug").and_then(Value::as_str).unwrap_or("model"))
            .to_owned(),
    })
}

fn parse_error(value: &Value) -> Option<BridgeError> {
    Some(BridgeError {
        code: value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("bridge_error")
            .to_owned(),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("ChatGPT bridge operation failed")
            .to_owned(),
        retryable: value
            .get("retryable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status: value
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok()),
        param: value
            .get("param")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        upstream_request_id: value
            .get("request_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        response_shape: value
            .get("response_shape")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn only_read_only_model_discovery_receives_retry_override() {
        let read = command_json(BridgeCommand::ListModels { max_attempts: 1 });
        assert_eq!(read["type"], "models");
        assert_eq!(read["read_retry_policy"]["maxAttempts"], 1);
        let inference = command_json(BridgeCommand::StreamResponse {
            request_id: "turn".to_owned(),
            model: "test-model".to_owned(),
            input: json!("hello"),
            instructions: None,
            request_patch: json!({}),
        });
        assert!(inference.get("read_retry_policy").is_none());
    }

    #[test]
    fn exited_sidecar_drops_even_queued_commands_while_sender_is_open() {
        let (sender, receiver) = mpsc::channel::<BridgeCommand>();
        let mut child = ProcessCommand::new("node")
            .args(["-e", "process.exit(0)"])
            .spawn()
            .expect("Node.js 22+ is available in CI");
        child.wait().expect("reap synthetic sidecar");
        sender.send(BridgeCommand::RefreshSession).unwrap();
        assert!(recv_command_while_sidecar_alive(&receiver, &mut child).is_none());
    }

    #[test]
    fn live_sidecar_accepts_shutdown_even_while_idle() {
        let (sender, receiver) = mpsc::channel::<BridgeCommand>();
        let mut child = ProcessCommand::new("node")
            .args(["-e", "setTimeout(() => process.exit(0), 30000)"])
            .spawn()
            .expect("Node.js 22+ is available in CI");
        sender.send(BridgeCommand::Shutdown).unwrap();
        assert!(matches!(
            recv_command_while_sidecar_alive(&receiver, &mut child),
            Some(BridgeCommand::Shutdown)
        ));
        child.kill().expect("stop synthetic sidecar");
        child.wait().expect("reap synthetic sidecar");
    }

    #[test]
    fn sidecar_stdout_eof_does_not_leave_runtime_marked_ready() {
        let (sender, receiver) = mpsc::channel();
        let repaint = egui::Context::default();
        forward_bridge_output(
            Cursor::new(
                br#"{"type":"ready","protocol":1}
"#,
            ),
            &sender,
            &repaint,
        );
        assert!(matches!(receiver.try_recv(), Ok(BridgeEvent::Ready)));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::Session(session)) if session == SessionState::default()
        ));
        let event = receiver
            .try_recv()
            .expect("EOF must become a runtime failure");
        assert!(matches!(
            event,
            BridgeEvent::RuntimeUnavailable(message)
                if message.contains("exited unexpectedly") &&
                   message.contains("pending sign-in or response")
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn sidecar_stdout_empty_exits_with_runtime_failure() {
        let (sender, receiver) = mpsc::channel();
        forward_bridge_output(
            Cursor::new(Vec::<u8>::new()),
            &sender,
            &egui::Context::default(),
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::Session(session)) if session == SessionState::default()
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::RuntimeUnavailable(_))
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn sidecar_fatal_keeps_specific_error_without_duplicate_eof() {
        let (sender, receiver) = mpsc::channel();
        forward_bridge_output(
            Cursor::new(
                br#"{"type":"fatal","error":{"code":"bootstrap_failed","message":"pin mismatch"}}"#,
            ),
            &sender,
            &egui::Context::default(),
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::Session(session)) if session == SessionState::default()
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::RuntimeUnavailable(message)) if message == "pin mismatch"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn fatal_after_connected_session_revokes_stale_connection_before_error() {
        let (sender, receiver) = mpsc::channel();
        forward_bridge_output(
            Cursor::new(concat!(
                r#"{"type":"session","session":{"status":"connected","sharing":true,"profileId":"offline"}}"#,
                "\n",
                r#"{"type":"fatal","error":{"code":"fatal","message":"sidecar stopped"}}"#,
                "\n"
            )),
            &sender,
            &egui::Context::default(),
        );

        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::Session(session))
                if session.status == "connected" && session.sharing
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::Session(session)) if session == SessionState::default()
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(BridgeEvent::RuntimeUnavailable(message)) if message == "sidecar stopped"
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn stream_response_command_preserves_instructions() {
        assert_eq!(
            command_json(BridgeCommand::StreamResponse {
                request_id: "turn-1".to_owned(),
                model: "gpt-example".to_owned(),
                input: serde_json::json!([{"role":"user","content":"hi"}]),
                instructions: Some("Be concise.".to_owned()),
                request_patch: serde_json::json!({
                    "reasoning": {"effort": "low"},
                    "text": {"verbosity": "low"}
                }),
            }),
            serde_json::json!({
                "type": "stream_response",
                "request_id": "turn-1",
                "model": "gpt-example",
                "input": [{"role":"user","content":"hi"}],
                "instructions": "Be concise.",
                "request_patch": {
                    "reasoning": {"effort": "low"},
                    "text": {"verbosity": "low"}
                }
            })
        );
    }

    #[test]
    fn probe_response_command_keeps_patch_inside_bridge_protocol() {
        assert_eq!(
            command_json(BridgeCommand::ProbeResponse {
                request_id: "probe:reasoning".to_owned(),
                model: "gpt-example".to_owned(),
                input: serde_json::json!("Reply with exactly REASONING_OK."),
                request_patch: serde_json::json!({"reasoning":{"effort":"low"}}),
            }),
            serde_json::json!({
                "type": "probe_response",
                "request_id": "probe:reasoning",
                "model": "gpt-example",
                "input": "Reply with exactly REASONING_OK.",
                "request_patch": {"reasoning":{"effort":"low"}}
            })
        );
    }

    #[test]
    fn cancel_response_command_targets_active_request() {
        assert_eq!(
            command_json(BridgeCommand::CancelResponse {
                target_request_id: "turn-1".to_owned(),
            }),
            serde_json::json!({
                "type": "cancel_response",
                "request_id": "cancel:turn-1",
                "target_request_id": "turn-1"
            })
        );
    }

    #[test]
    fn parses_safe_session_without_credentials() {
        let event = parse_bridge_event(
            r#"{"type":"session","session":{"status":"connected","sharing":true,"profileId":"profile-1","profileLabel":"Connection 1","identity":{"email":"user@example.com"}}}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            BridgeEvent::Session(SessionState {
                status: "connected".to_owned(),
                sharing: true,
                profile_id: Some("profile-1".to_owned()),
                profile_label: Some("Connection 1".to_owned()),
                email: Some("user@example.com".to_owned()),
                error_message: None,
            })
        );
    }

    #[test]
    fn parses_models_and_stream_deltas() {
        let models = parse_bridge_event(
            r#"{"type":"result","request_id":"models","result":{"models":[{"slug":"gpt-example","displayName":"Example"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            models,
            BridgeEvent::Models(vec![Model {
                slug: "gpt-example".to_owned(),
                display_name: "Example".to_owned(),
            }])
        );

        let delta = parse_bridge_event(r#"{"type":"delta","request_id":"turn-1","delta":"hello"}"#)
            .unwrap();
        assert_eq!(
            delta,
            BridgeEvent::Delta {
                request_id: "turn-1".to_owned(),
                delta: "hello".to_owned(),
            }
        );
    }

    #[test]
    fn parses_safe_error_parameter_diagnostics() {
        let event = parse_bridge_event(
            r#"{"type":"error","request_id":"probe:reasoning","error":{"code":"invalid_request_error","message":"rejected","retryable":false,"status":400,"param":"reasoning","request_id":"req_probe_123","response_shape":"{error:{code:string,param:string}}"}}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            BridgeEvent::Failed {
                request_id: Some("probe:reasoning".to_owned()),
                error: BridgeError {
                    code: "invalid_request_error".to_owned(),
                    message: "rejected".to_owned(),
                    retryable: false,
                    status: Some(400),
                    param: Some("reasoning".to_owned()),
                    upstream_request_id: Some("req_probe_123".to_owned()),
                    response_shape: Some("{error:{code:string,param:string}}".to_owned()),
                },
            }
        );
    }

    #[test]
    fn rejects_credential_bearing_output_even_if_sidecar_regresses() {
        let event = parse_bridge_event(
            r#"{"type":"session","session":{"status":"connected","sharing":true,"access_token":"secret"}}"#,
        )
        .unwrap();
        assert_eq!(
            event,
            BridgeEvent::RuntimeUnavailable(
                "Sign in with ChatGPT bridge output violated the credential boundary".to_owned()
            )
        );
    }

    #[test]
    fn explicit_http_rejection_is_not_ambiguous_delivery() {
        let rejection = r#"{"type":"agent_activity","request_id":"test-request","source":"siwc_bridge_observation","sequence":3,"stage":"http_rejected","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":123.0,"http_status":429}"#;
        assert!(
            agent_activity_diagnostic(rejection)
                .unwrap()
                .contains("HTTP 429")
        );
        let terminal = r#"{"type":"agent_activity","request_id":"test-request","source":"siwc_bridge_observation","sequence":4,"stage":"failed","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":130.0,"delivery_evidence":"http_rejected","http_status":429}"#;
        assert!(
            agent_activity_diagnostic(terminal)
                .unwrap()
                .contains("evidence=http_rejected HTTP 429")
        );
        let forged = r#"{"type":"agent_activity","request_id":"test-request","source":"siwc_bridge_observation","sequence":4,"stage":"failed","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":130.0,"delivery_evidence":"wire_unconfirmed","http_status":429}"#;
        assert!(agent_activity_diagnostic(forged).is_none());
    }

    #[test]
    fn passes_validated_agent_activity_to_native_journal_sink() {
        let raw = r#"{"type":"agent_activity","request_id":"turn-1","source":"siwc_bridge_observation","sequence":1,"stage":"started","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0.0}"#;
        let event = parse_bridge_event(raw).unwrap();
        assert!(matches!(
            event,
            BridgeEvent::AgentActivity { request_id, frame }
                if request_id == "turn-1" && frame == raw
        ));
        assert!(parse_bridge_event(r#"{"type":"agent_activity","request_id":"turn-1","source":"siwc_bridge_observation","sequence":1,"stage":"started","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0.0,"access_token":"forbidden"}"#).is_some_and(|event| matches!(event, BridgeEvent::RuntimeUnavailable(_))));
    }

    #[test]
    fn source_observed_activity_is_sanitized_before_terminal_logging() {
        let valid = r#"{"type":"agent_activity","request_id":"long-request-activity-id","source":"siwc_bridge_observation","sequence":3,"stage":"first_text_delta","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":123.456,"delta_chars":12}"#;
        let notice = agent_activity_diagnostic(valid).unwrap();
        assert!(notice.contains("stage=first_text_delta"));
        assert!(notice.contains("seq=3"));
        assert!(notice.contains("elapsed=123.456ms"));
        assert!(!notice.contains("long-request-activity-id"));
        for invalid in [
            r#"{"type":"agent_activity","request_id":"bad\\nline","source":"siwc_bridge_observation","sequence":1,"stage":"started","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0}"#,
            r#"{"type":"agent_activity","request_id":"a","source":"fake_source","sequence":1,"stage":"started","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0}"#,
            r#"{"type":"agent_activity","request_id":"a","source":"siwc_bridge_observation","sequence":1,"stage":"reasoning_summary","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0}"#,
            r#"{"type":"agent_activity","request_id":"a","source":"siwc_bridge_observation","sequence":1,"stage":"started","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0,"access_token":"secret"}"#,
            r#"{"type":"agent_activity","request_id":"a","source":"siwc_bridge_observation","sequence":1,"stage":"completed","observed_at_utc":"2026-10-13T18:30:00.000Z","elapsed_ms":0,"delivery_evidence":"magical_success"}"#,
        ] {
            assert!(agent_activity_diagnostic(invalid).is_none());
        }
    }

    #[test]
    fn only_sanitized_model_read_retries_are_diagnostic() {
        let message = r#"{"type":"read_retry_scheduled","request_id":"an-account-local-request-id","operation":"models","stage":"model_discovery","category":"rate_limit","status":429,"attempt":1,"next_attempt":2,"delay_ms":350}"#;
        let notice = read_retry_diagnostic(message).unwrap();
        assert!(notice.contains("read-only models"));
        assert!(notice.contains("HTTP 429"));
        assert!(notice.contains("delay=350ms"));
        assert!(!notice.contains("an-account-local-request-id"));
        let configured = r#"{"type":"read_retry_scheduled","request_id":"request-4","operation":"models","stage":"model_discovery","category":"http_transient","status":503,"attempt":4,"next_attempt":5,"delay_ms":20000}"#;
        assert!(
            read_retry_diagnostic(configured)
                .unwrap()
                .contains("delay=20000ms")
        );
        for invalid in [
            r#"{"type":"read_retry_scheduled","request_id":"a","operation":"models","stage":"model_discovery","category":"rate_limit","status":429,"attempt":1,"next_attempt":2,"delay_ms":1,"access_token":"secret"}"#,
            r#"{"type":"read_retry_scheduled","request_id":"a","operation":"stream_response","category":"rate_limit","status":429,"attempt":1,"next_attempt":2,"delay_ms":1}"#,
            r#"{"type":"read_retry_scheduled","request_id":"a","operation":"models","stage":"model_discovery","category":"arbitrary","attempt":1,"next_attempt":2,"delay_ms":1}"#,
            r#"{"type":"read_retry_scheduled","request_id":"a","operation":"models","stage":"model_discovery","category":"rate_limit","attempt":1,"next_attempt":2,"delay_ms":100000}"#,
            r#"{"type":"read_retry_scheduled","request_id":"injected\\nline","operation":"models","stage":"model_discovery","category":"rate_limit","attempt":1,"next_attempt":2,"delay_ms":1}"#,
        ] {
            assert!(read_retry_diagnostic(invalid).is_none());
        }
    }

    #[test]
    fn fatal_bridge_error_never_exposes_raw_error_object() {
        let event = parse_bridge_event(
            r#"{"type":"fatal","error":{"code":"siwc_devkit_not_built","message":"bootstrap required","retryable":false}}"#,
        )
        .unwrap();
        assert_eq!(
            event,
            BridgeEvent::RuntimeUnavailable("bootstrap required".to_owned())
        );
    }
}
