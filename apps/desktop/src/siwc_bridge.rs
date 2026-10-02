use eframe::egui;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

#[derive(Debug)]
pub enum BridgeCommand {
    RefreshSession,
    SignIn,
    CancelSignIn,
    ListModels,
    StreamResponse {
        request_id: String,
        model: String,
        input: Value,
    },
    Disconnect,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub status: String,
    pub sharing: bool,
    pub profile_label: Option<String>,
    pub email: Option<String>,
    pub error_message: Option<String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            status: "disconnected".to_owned(),
            sharing: false,
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
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match prepared {
        Ok(status) if status.success() => {}
        Ok(status) => {
            send_event(
                &events,
                &repaint,
                BridgeEvent::RuntimeUnavailable(format!(
                    "could not prepare the pinned Sign in with ChatGPT runtime (bootstrap exited with {status})"
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
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if let Some(event) = parse_bridge_event(&line) {
                            send_event(&reader_events, &reader_repaint, event);
                        }
                    }
                    Err(error) => {
                        send_event(
                            &reader_events,
                            &reader_repaint,
                            BridgeEvent::RuntimeUnavailable(format!(
                                "Sign in with ChatGPT bridge output failed: {error}"
                            )),
                        );
                        break;
                    }
                }
            }
        });

    while let Ok(command) = commands.recv() {
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

fn send_event(events: &Sender<BridgeEvent>, repaint: &egui::Context, event: BridgeEvent) {
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
        BridgeCommand::ListModels => json!({
            "type": "models",
            "request_id": "models"
        }),
        BridgeCommand::StreamResponse {
            request_id,
            model,
            input,
        } => json!({
            "type": "stream_response",
            "request_id": request_id,
            "model": model,
            "input": input
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
                "accesstoken"
                    | "refreshtoken"
                    | "idtoken"
                    | "authorization"
                    | "cookie"
                    | "cookies"
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_session_without_credentials() {
        let event = parse_bridge_event(
            r#"{"type":"session","session":{"status":"connected","sharing":true,"profileLabel":"Connection 1","identity":{"email":"user@example.com"}}}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            BridgeEvent::Session(SessionState {
                status: "connected".to_owned(),
                sharing: true,
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
