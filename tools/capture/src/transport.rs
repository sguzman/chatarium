//! Mockable localhost-only Chrome DevTools Protocol transport.

use crate::run::CaptureRun;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::Message;
use tungstenite::client;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocket;
use url::Url;

const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_MESSAGES: usize = 4096;
const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Transport, endpoint, protocol, and journal errors remain distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// A supplied URL or local debugging endpoint violated the localhost boundary.
    InvalidEndpoint(String),
    /// A local socket or I/O operation failed.
    Io(String),
    /// A DevTools HTTP endpoint returned an invalid response.
    Http(String),
    /// A CDP JSON message was malformed or unsupported.
    MalformedMessage(String),
    /// The WebSocket closed before the requested operation completed.
    Disconnected,
    /// A CDP command was sent or may have been sent, but its response is unknown.
    CommandOutcomeUnknown {
        /// Command ID assigned before attempting to send the command.
        command_id: u64,
        /// Transport/protocol reason the matching response was not obtained.
        reason: String,
    },
    /// The requested operation exceeded its caller-supplied deadline.
    Timeout {
        /// Correlated command ID, or `None` while waiting only for an event.
        command_id: Option<u64>,
    },
    /// CDP returned an error object for a command.
    CommandError {
        /// Raw error code from the CDP response.
        code: i64,
        /// Raw error message from the CDP response.
        message: String,
    },
    /// A response used an ID that has no outstanding command.
    UnexpectedResponseId {
        /// Current command ID, or `None` when no command is outstanding.
        expected: Option<u64>,
        /// Response ID received from CDP.
        received: u64,
    },
    /// A requested target was not among the most recently discovered page targets.
    UnknownTarget(String),
    /// A durable journal append failed.
    Journal(String),
    /// Browser process management failed.
    Process(String),
    /// A requested profile was not the dedicated Chatarium-owned path.
    UnsafeProfile(String),
    /// A stale process lock or debugging-port file was left behind.
    StaleState(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(reason) => {
                write!(formatter, "invalid DevTools endpoint: {reason}")
            }
            Self::Io(reason) => write!(formatter, "transport I/O: {reason}"),
            Self::Http(reason) => write!(formatter, "DevTools HTTP: {reason}"),
            Self::MalformedMessage(reason) => write!(formatter, "malformed CDP message: {reason}"),
            Self::Disconnected => formatter.write_str("CDP WebSocket disconnected"),
            Self::CommandOutcomeUnknown { command_id, reason } => write!(
                formatter,
                "CDP command {command_id} may have executed; outcome is unknown: {reason}"
            ),
            Self::Timeout {
                command_id: Some(id),
            } => write!(formatter, "timed out waiting for CDP command {id}"),
            Self::Timeout { command_id: None } => {
                formatter.write_str("timed out waiting for a CDP event")
            }
            Self::CommandError { code, message } => {
                write!(formatter, "CDP command failed ({code}): {message}")
            }
            Self::UnexpectedResponseId {
                expected: Some(expected),
                received,
            } => write!(
                formatter,
                "unexpected CDP response ID {received} while waiting for {expected}"
            ),
            Self::UnexpectedResponseId {
                expected: None,
                received,
            } => write!(formatter, "unsolicited CDP response ID {received}"),
            Self::UnknownTarget(target) => {
                write!(formatter, "page target '{target}' was not discovered")
            }
            Self::Journal(reason) => write!(formatter, "capture journal: {reason}"),
            Self::Process(reason) => write!(formatter, "Edge process: {reason}"),
            Self::UnsafeProfile(reason) => write!(formatter, "unsafe Edge profile: {reason}"),
            Self::StaleState(reason) => write!(formatter, "stale Edge capture state: {reason}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Ephemeral HTTP endpoint bound to IPv4 loopback only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevToolsEndpoint {
    port: u16,
}

impl DevToolsEndpoint {
    /// Construct a loopback endpoint from a nonzero port.
    pub fn loopback(port: u16) -> Result<Self, TransportError> {
        if port == 0 {
            return Err(TransportError::InvalidEndpoint(
                "port must be nonzero".to_owned(),
            ));
        }
        Ok(Self { port })
    }

    /// Read Chromium's selected ephemeral port from its `DevToolsActivePort` contents.
    pub fn from_active_port(contents: &str) -> Result<Self, TransportError> {
        let port = contents
            .lines()
            .next()
            .ok_or_else(|| {
                TransportError::InvalidEndpoint("DevToolsActivePort is empty".to_owned())
            })?
            .trim()
            .parse::<u16>()
            .map_err(|_| {
                TransportError::InvalidEndpoint("DevToolsActivePort has an invalid port".to_owned())
            })?;
        Self::loopback(port)
    }

    /// Port selected by the harness-owned browser.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }

    fn socket_addr(self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.port)
    }
}

/// Browser identity returned by `/json/version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserVersion {
    /// Browser product/version string as returned by CDP.
    pub browser: String,
    /// CDP protocol version as returned by CDP.
    pub protocol_version: String,
}

/// Page target from `/json/list`; raw target type and title/url observations are preserved.
#[derive(Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// CDP target identifier.
    pub id: String,
    /// Raw CDP target type.
    pub target_type: String,
    /// Raw target title.
    pub title: String,
    /// Raw target URL.
    pub url: String,
    websocket_url: Url,
}

impl fmt::Debug for TargetInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetInfo")
            .field("id", &self.id)
            .field("target_type", &self.target_type)
            .finish_non_exhaustive()
    }
}

/// Raw CDP event, keeping the browser-provided method name unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct CdpEvent {
    /// Raw CDP event method name.
    pub method: String,
    /// Raw CDP event parameters.
    pub params: Value,
}

/// Mockable browser discovery and page-attachment boundary.
pub trait BrowserTransport: Send {
    /// Fetch browser and CDP version metadata and journal endpoint discovery.
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError>;
    /// Discover page targets and journal the raw target identifiers/types.
    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError>;
    /// Attach to a target returned by `list_targets` and journal attachment.
    fn attach(
        &mut self,
        target_id: &str,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PageSession>, TransportError>;
}

/// Mockable CDP page/session boundary.
pub trait PageSession: Send {
    /// Send one raw CDP method and return its matching result.
    ///
    /// A [`TransportError::CommandOutcomeUnknown`] after dispatch does not prove that the
    /// browser did not execute the command. Callers must preserve that uncertainty and must not
    /// automatically retry a mutating command.
    fn command(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError>;
    /// Return the next raw event, or `None` when the requested wait expires.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CdpEvent>, TransportError>;
    /// Close this CDP session. Repeated calls are harmless.
    fn close(&mut self) -> Result<(), TransportError>;
}

/// Narrow HTTP interface used to query only known DevTools JSON resources.
pub trait DevToolsHttp: Send + Sync {
    /// Fetch `/json/version` or `/json/list` from the validated local endpoint.
    fn get_json(
        &self,
        endpoint: DevToolsEndpoint,
        resource: DevToolsResource,
    ) -> Result<Value, TransportError>;
}

/// Supported DevTools HTTP resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevToolsResource {
    /// Browser and protocol version metadata.
    Version,
    /// Browser target listing.
    Targets,
}

impl DevToolsResource {
    fn path(self) -> &'static str {
        match self {
            Self::Version => "/json/version",
            Self::Targets => "/json/list",
        }
    }
}

/// Mockable WebSocket connection factory.
pub trait WebSocketConnector: Send + Sync {
    /// Connect to a previously validated localhost CDP WebSocket URL.
    fn connect(&self, endpoint: &Url) -> Result<Box<dyn WebSocketConnection>, TransportError>;
}

/// Mockable WebSocket message boundary.
pub trait WebSocketConnection: Send {
    /// Send one text frame.
    fn send_text(&mut self, text: &str) -> Result<(), TransportError>;
    /// Receive one text frame; `None` means the read timeout elapsed.
    fn receive_text(&mut self, timeout: Duration) -> Result<Option<String>, TransportError>;
    /// Close the WebSocket.
    fn close(&mut self) -> Result<(), TransportError>;
}

/// Real loopback-only HTTP client for Edge's DevTools JSON endpoints.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackDevToolsHttp;

impl DevToolsHttp for LoopbackDevToolsHttp {
    fn get_json(
        &self,
        endpoint: DevToolsEndpoint,
        resource: DevToolsResource,
    ) -> Result<Value, TransportError> {
        let mut stream = TcpStream::connect_timeout(&endpoint.socket_addr().into(), IO_TIMEOUT)
            .map_err(|error| TransportError::Io(error.to_string()))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        write!(stream, "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n", resource.path(), endpoint.port())
            .map_err(|error| TransportError::Io(error.to_string()))?;

        let mut response = Vec::new();
        stream
            .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut response)
            .map_err(|error| TransportError::Io(error.to_string()))?;
        if response.len() > MAX_HTTP_RESPONSE_BYTES {
            return Err(TransportError::Http(
                "response exceeded 1 MiB limit".to_owned(),
            ));
        }
        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| TransportError::Http("response has no header terminator".to_owned()))?;
        let headers = std::str::from_utf8(&response[..body_start]).map_err(|error| {
            TransportError::Http(format!("response headers are not UTF-8: {error}"))
        })?;
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| TransportError::Http("response status line is malformed".to_owned()))?;
        if status != 200 {
            return Err(TransportError::Http(format!(
                "DevTools returned HTTP {status}"
            )));
        }
        let body = &response[body_start + 4..];
        serde_json::from_slice(body)
            .map_err(|error| TransportError::Http(format!("invalid JSON response: {error}")))
    }
}

/// Real WebSocket connector restricted to IPv4 loopback.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackWebSocketConnector;

impl WebSocketConnector for LoopbackWebSocketConnector {
    fn connect(&self, endpoint: &Url) -> Result<Box<dyn WebSocketConnection>, TransportError> {
        let port = validate_websocket_url(endpoint, None)?;
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let stream = TcpStream::connect_timeout(&address.into(), IO_TIMEOUT)
            .map_err(|error| TransportError::Io(error.to_string()))?;
        stream
            .set_nodelay(true)
            .map_err(|error| TransportError::Io(error.to_string()))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        let request = endpoint
            .as_str()
            .into_client_request()
            .map_err(|error| TransportError::InvalidEndpoint(error.to_string()))?;
        let (socket, response) =
            client(request, stream).map_err(|error| TransportError::Io(error.to_string()))?;
        if response.status().as_u16() != 101 {
            return Err(TransportError::Http(format!(
                "WebSocket upgrade returned HTTP {}",
                response.status()
            )));
        }
        Ok(Box::new(TungsteniteConnection {
            socket,
            closed: false,
        }))
    }
}

struct TungsteniteConnection {
    socket: WebSocket<TcpStream>,
    closed: bool,
}

impl WebSocketConnection for TungsteniteConnection {
    fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        self.socket
            .send(Message::Text(text.to_owned().into()))
            .map_err(map_websocket_error)
    }

    fn receive_text(&mut self, timeout: Duration) -> Result<Option<String>, TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        self.socket
            .get_ref()
            .set_read_timeout(Some(timeout))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => return Ok(Some(text.to_string())),
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                Ok(Message::Close(_)) => {
                    self.closed = true;
                    return Err(TransportError::Disconnected);
                }
                Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => {
                    return Err(TransportError::MalformedMessage(
                        "expected a JSON text frame".to_owned(),
                    ));
                }
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(map_websocket_error(error)),
            }
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.socket.close(None).map_err(map_websocket_error)
    }
}

fn map_websocket_error(error: tungstenite::Error) -> TransportError {
    match error {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            TransportError::Disconnected
        }
        tungstenite::Error::Io(error) => TransportError::Io(error.to_string()),
        other => TransportError::MalformedMessage(other.to_string()),
    }
}

/// Concrete HTTP/WebSocket CDP browser implementation.
pub struct DevToolsBrowserTransport {
    endpoint: DevToolsEndpoint,
    http: Box<dyn DevToolsHttp>,
    websocket: Box<dyn WebSocketConnector>,
    version: Option<BrowserVersion>,
    targets: HashMap<String, TargetInfo>,
}

impl DevToolsBrowserTransport {
    /// Build a transport with concrete localhost clients.
    #[must_use]
    pub fn loopback(endpoint: DevToolsEndpoint) -> Self {
        Self::with_clients(
            endpoint,
            Box::new(LoopbackDevToolsHttp),
            Box::new(LoopbackWebSocketConnector),
        )
    }

    /// Build a transport with injected clients for deterministic tests or alternate hosts.
    #[must_use]
    pub fn with_clients(
        endpoint: DevToolsEndpoint,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
    ) -> Self {
        Self {
            endpoint,
            http,
            websocket,
            version: None,
            targets: HashMap::new(),
        }
    }

    /// Validated ephemeral debugging endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> DevToolsEndpoint {
        self.endpoint
    }
}

impl BrowserTransport for DevToolsBrowserTransport {
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError> {
        if let Some(version) = &self.version {
            return Ok(version.clone());
        }
        let value = self
            .http
            .get_json(self.endpoint, DevToolsResource::Version)?;
        let browser = required_string(&value, "Browser")?.to_owned();
        let protocol_version = required_string(&value, "Protocol-Version")?.to_owned();
        let websocket_raw = required_string(&value, "webSocketDebuggerUrl")?;
        parse_websocket_url(websocket_raw, self.endpoint.port())?;
        let version = BrowserVersion {
            browser,
            protocol_version,
        };
        run.append_event(
            "debugging_endpoint_discovered",
            json!({
                "address": "127.0.0.1",
                "port": self.endpoint.port(),
                "browser": version.browser,
                "protocol_version": version.protocol_version,
            }),
        )
        .map_err(TransportError::Journal)?;
        run.set_browser_versions(version.browser.clone(), version.protocol_version.clone())
            .map_err(TransportError::Journal)?;
        self.version = Some(version.clone());
        Ok(version)
    }

    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        let value = self
            .http
            .get_json(self.endpoint, DevToolsResource::Targets)?;
        let entries = value.as_array().ok_or_else(|| {
            TransportError::MalformedMessage("/json/list response is not an array".to_owned())
        })?;
        let mut targets = HashMap::new();
        for entry in entries {
            let target_type = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if target_type != "page" {
                continue;
            }
            let id = required_string(entry, "id")?.to_owned();
            let title = required_string(entry, "title")?.to_owned();
            let url = required_string(entry, "url")?.to_owned();
            let websocket_raw = required_string(entry, "webSocketDebuggerUrl")?;
            let websocket_url = parse_websocket_url(websocket_raw, self.endpoint.port())?;
            if targets.contains_key(&id) {
                return Err(TransportError::MalformedMessage(format!(
                    "/json/list contains duplicate page target ID '{id}'"
                )));
            }
            targets.insert(
                id.clone(),
                TargetInfo {
                    id,
                    target_type: target_type.to_owned(),
                    title,
                    url,
                    websocket_url,
                },
            );
        }
        let mut observed_targets = targets
            .values()
            .map(|target| json!({"id": target.id, "type": target.target_type}))
            .collect::<Vec<_>>();
        observed_targets.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        run.append_event(
            "cdp_page_targets_discovered",
            json!({
                "targets": observed_targets,
                "target_count": targets.len(),
            }),
        )
        .map_err(TransportError::Journal)?;
        let mut result = targets.values().cloned().collect::<Vec<_>>();
        result.sort_by(|left, right| left.id.cmp(&right.id));
        self.targets = targets;
        Ok(result)
    }

    fn attach(
        &mut self,
        target_id: &str,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PageSession>, TransportError> {
        let target = self
            .targets
            .get(target_id)
            .ok_or_else(|| TransportError::UnknownTarget(target_id.to_owned()))?;
        if !allowed_target_url(&target.url) {
            return Err(TransportError::InvalidEndpoint(
                "refusing to attach to a page outside about:blank or https://chatgpt.com"
                    .to_owned(),
            ));
        }
        let socket = self.websocket.connect(&target.websocket_url)?;
        if let Err(error) = run.append_event(
            "cdp_target_attached",
            json!({
                "target_id": target.id,
                "target_type": target.target_type,
            }),
        ) {
            let mut socket = socket;
            let _ = socket.close();
            return Err(TransportError::Journal(error));
        }
        Ok(Box::new(CdpPageSession::new(socket)))
    }
}

struct ParsedResponse {
    id: u64,
    result: Option<Value>,
    error: Option<(i64, String)>,
}

enum ParsedMessage {
    Event(CdpEvent),
    Response(ParsedResponse),
}

/// Parse a raw CDP message while preserving event method names and params.
fn parse_cdp_message(text: &str) -> Result<ParsedMessage, TransportError> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| TransportError::MalformedMessage(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| TransportError::MalformedMessage("message is not an object".to_owned()))?;
    if let Some(method_value) = object.get("method") {
        if object.contains_key("id") {
            return Err(TransportError::MalformedMessage(
                "message contains both method and id".to_owned(),
            ));
        }
        let method = method_value
            .as_str()
            .filter(|method| !method.is_empty())
            .ok_or_else(|| {
                TransportError::MalformedMessage(
                    "event method is missing or not a string".to_owned(),
                )
            })?;
        return Ok(ParsedMessage::Event(CdpEvent {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }));
    }
    let id = object.get("id").and_then(Value::as_u64).ok_or_else(|| {
        TransportError::MalformedMessage("response ID is missing or invalid".to_owned())
    })?;
    if object.contains_key("error") && object.contains_key("result") {
        return Err(TransportError::MalformedMessage(
            "response contains both result and error".to_owned(),
        ));
    }
    if let Some(error) = object.get("error") {
        let error_object = error.as_object().ok_or_else(|| {
            TransportError::MalformedMessage("response error is not an object".to_owned())
        })?;
        let code = error_object
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                TransportError::MalformedMessage(
                    "response error code is missing or invalid".to_owned(),
                )
            })?;
        let message = required_string(error, "message")?.to_owned();
        return Ok(ParsedMessage::Response(ParsedResponse {
            id,
            result: None,
            error: Some((code, message)),
        }));
    }
    let result = object.get("result").cloned().ok_or_else(|| {
        TransportError::MalformedMessage("response has neither result nor error".to_owned())
    })?;
    Ok(ParsedMessage::Response(ParsedResponse {
        id,
        result: Some(result),
        error: None,
    }))
}

/// One synchronous CDP command/event session with ID correlation and queued events.
pub struct CdpPageSession {
    websocket: Box<dyn WebSocketConnection>,
    next_id: u64,
    events: VecDeque<CdpEvent>,
    responses: HashMap<u64, ParsedResponse>,
    closed: bool,
}

impl CdpPageSession {
    /// Create a CDP session over an injected socket.
    #[must_use]
    pub fn new(websocket: Box<dyn WebSocketConnection>) -> Self {
        Self {
            websocket,
            next_id: 1,
            events: VecDeque::new(),
            responses: HashMap::new(),
            closed: false,
        }
    }
}

impl PageSession for CdpPageSession {
    fn command(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        if method.is_empty() || !params.is_object() {
            return Err(TransportError::MalformedMessage(
                "command requires a nonempty method and object params".to_owned(),
            ));
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| TransportError::MalformedMessage("command ID exhausted".to_owned()))?;
        let text = serde_json::to_string(&json!({ "id": id, "method": method, "params": params }))
            .map_err(|error| TransportError::MalformedMessage(error.to_string()))?;
        if let Err(error) = self.websocket.send_text(&text) {
            return Err(TransportError::CommandOutcomeUnknown {
                command_id: id,
                reason: error.to_string(),
            });
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return response_result(response);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::CommandOutcomeUnknown {
                    command_id: id,
                    reason: "timed out waiting for the matching response".to_owned(),
                });
            }
            let text = match self.websocket.receive_text(remaining) {
                Ok(Some(text)) => text,
                Ok(None) => {
                    return Err(TransportError::CommandOutcomeUnknown {
                        command_id: id,
                        reason: "timed out waiting for the matching response".to_owned(),
                    });
                }
                Err(error) => {
                    return Err(TransportError::CommandOutcomeUnknown {
                        command_id: id,
                        reason: error.to_string(),
                    });
                }
            };
            let parsed = parse_cdp_message(&text).map_err(|error| {
                TransportError::CommandOutcomeUnknown {
                    command_id: id,
                    reason: error.to_string(),
                }
            })?;
            match parsed {
                ParsedMessage::Event(event) => {
                    self.push_event(event).map_err(|error| {
                        TransportError::CommandOutcomeUnknown {
                            command_id: id,
                            reason: error.to_string(),
                        }
                    })?;
                }
                ParsedMessage::Response(response) if response.id == id => {
                    return response_result(response);
                }
                ParsedMessage::Response(response) => {
                    if self.responses.len() >= MAX_QUEUED_MESSAGES {
                        return Err(TransportError::CommandOutcomeUnknown {
                            command_id: id,
                            reason: "too many unmatched CDP responses".to_owned(),
                        });
                    }
                    self.responses.insert(response.id, response);
                }
            }
        }
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<CdpEvent>, TransportError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let Some(text) = self.websocket.receive_text(remaining)? else {
                return Ok(None);
            };
            match parse_cdp_message(&text)? {
                ParsedMessage::Event(event) => return Ok(Some(event)),
                ParsedMessage::Response(response) => {
                    if self.responses.len() >= MAX_QUEUED_MESSAGES {
                        return Err(TransportError::MalformedMessage(
                            "too many unmatched CDP responses".to_owned(),
                        ));
                    }
                    self.responses.insert(response.id, response);
                }
            }
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.websocket.close()
    }
}

impl CdpPageSession {
    fn push_event(&mut self, event: CdpEvent) -> Result<(), TransportError> {
        if self.events.len() >= MAX_QUEUED_MESSAGES {
            return Err(TransportError::MalformedMessage(
                "too many queued CDP events".to_owned(),
            ));
        }
        self.events.push_back(event);
        Ok(())
    }
}

impl Drop for CdpPageSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn response_result(response: ParsedResponse) -> Result<Value, TransportError> {
    if let Some((code, message)) = response.error {
        return Err(TransportError::CommandError { code, message });
    }
    response.result.ok_or_else(|| {
        TransportError::MalformedMessage("successful response has no result".to_owned())
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, TransportError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| TransportError::MalformedMessage(format!("missing string field '{field}'")))
}

fn parse_websocket_url(raw: &str, expected_port: u16) -> Result<Url, TransportError> {
    let endpoint =
        Url::parse(raw).map_err(|error| TransportError::InvalidEndpoint(error.to_string()))?;
    let port = validate_websocket_url(&endpoint, Some(expected_port))?;
    if port != expected_port {
        return Err(TransportError::InvalidEndpoint(
            "WebSocket port does not match the launched browser".to_owned(),
        ));
    }
    Ok(endpoint)
}

fn validate_websocket_url(
    endpoint: &Url,
    expected_port: Option<u16>,
) -> Result<u16, TransportError> {
    if endpoint.scheme() != "ws"
        || endpoint.host_str() != Some("127.0.0.1")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(TransportError::InvalidEndpoint(
            "only credential-free ws://127.0.0.1 endpoints are allowed".to_owned(),
        ));
    }
    let port = endpoint.port().ok_or_else(|| {
        TransportError::InvalidEndpoint("WebSocket URL has no explicit port".to_owned())
    })?;
    if expected_port.is_some_and(|expected| expected != port) {
        return Err(TransportError::InvalidEndpoint(
            "WebSocket port does not match the launched browser".to_owned(),
        ));
    }
    if endpoint.path().is_empty() || endpoint.path() == "/" {
        return Err(TransportError::InvalidEndpoint(
            "WebSocket URL has no endpoint path".to_owned(),
        ));
    }
    Ok(port)
}

fn allowed_target_url(raw: &str) -> bool {
    if raw == "about:blank" {
        return true;
    }
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.username().is_empty()
        && url.password().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_experiment;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct SocketState {
        incoming: VecDeque<Result<String, TransportError>>,
        outgoing: Vec<String>,
        closes: usize,
    }

    struct MockSocket(Arc<Mutex<SocketState>>);

    impl MockSocket {
        fn with_messages(
            messages: impl IntoIterator<Item = Result<String, TransportError>>,
        ) -> (Self, Arc<Mutex<SocketState>>) {
            let state = Arc::new(Mutex::new(SocketState {
                incoming: messages.into_iter().collect(),
                ..SocketState::default()
            }));
            (Self(state.clone()), state)
        }
    }

    impl WebSocketConnection for MockSocket {
        fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
            self.0.lock().unwrap().outgoing.push(text.to_owned());
            Ok(())
        }
        fn receive_text(&mut self, _timeout: Duration) -> Result<Option<String>, TransportError> {
            match self.0.lock().unwrap().incoming.pop_front() {
                Some(Ok(message)) => Ok(Some(message)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        }
        fn close(&mut self) -> Result<(), TransportError> {
            self.0.lock().unwrap().closes += 1;
            Ok(())
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-cdp-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    fn capture_run(label: &str) -> (CaptureRun, PathBuf) {
        let base = temp_dir(label);
        let experiment = canonical_experiment("C00-idle-load").unwrap();
        (CaptureRun::create(&base, &experiment).unwrap(), base)
    }

    #[test]
    fn command_id_correlates_matching_response_after_other_frames() {
        let messages = [
            Ok(r#"{"id":77,"result":{"wrong":true}}"#.to_owned()),
            Ok(r#"{"method":"Page.lifecycleEvent","params":{"name":"init"}}"#.to_owned()),
            Ok(r#"{"id":1,"result":{"enabled":true}}"#.to_owned()),
        ];
        let (socket, state) = MockSocket::with_messages(messages);
        let mut session = CdpPageSession::new(Box::new(socket));
        let result = session
            .command("Page.enable", json!({}), Duration::from_secs(1))
            .unwrap();
        assert_eq!(result, json!({"enabled":true}));
        let outbound: Value = serde_json::from_str(&state.lock().unwrap().outgoing[0]).unwrap();
        assert_eq!(outbound["id"], 1);
        assert_eq!(outbound["method"], "Page.enable");
        assert_eq!(
            session
                .next_event(Duration::from_millis(1))
                .unwrap()
                .unwrap()
                .method,
            "Page.lifecycleEvent"
        );
    }

    #[test]
    fn unsolicited_cdp_events_are_delivered_without_renaming() {
        let messages = [
            Ok(r#"{"method":"Network.requestWillBeSent","params":{"requestId":"r1"}}"#.to_owned()),
            Ok(r#"{"id":1,"result":{}}"#.to_owned()),
        ];
        let (socket, _) = MockSocket::with_messages(messages);
        let mut session = CdpPageSession::new(Box::new(socket));
        session
            .command("Page.enable", json!({}), Duration::from_secs(1))
            .unwrap();
        let event = session
            .next_event(Duration::from_millis(1))
            .unwrap()
            .unwrap();
        assert_eq!(event.method, "Network.requestWillBeSent");
        assert_eq!(event.params["requestId"], "r1");
    }

    #[test]
    fn cdp_reported_command_error_is_definitive() {
        let (socket, _) = MockSocket::with_messages([Ok(
            r#"{"id":1,"error":{"code":-32,"message":"method not found"}}"#.to_owned(),
        )]);
        let mut session = CdpPageSession::new(Box::new(socket));
        assert_eq!(
            session.command("Unknown.method", json!({}), Duration::from_secs(1)),
            Err(TransportError::CommandError {
                code: -32,
                message: "method not found".to_owned(),
            })
        );
    }

    #[test]
    fn malformed_protocol_messages_are_rejected() {
        for message in [
            "not-json",
            "[]",
            r#"{"method":"Page.enable","id":1}"#,
            r#"{"id":1}"#,
            r#"{"id":1,"result":{},"error":{"code":-1,"message":"bad"}}"#,
            r#"{"method":7}"#,
        ] {
            assert!(parse_cdp_message(message).is_err(), "accepted {message}");
        }
    }

    #[test]
    fn disconnected_or_closed_websocket_fails_visibly() {
        let (socket, state) = MockSocket::with_messages([Err(TransportError::Disconnected)]);
        let mut session = CdpPageSession::new(Box::new(socket));
        assert!(matches!(
            session.command("Page.enable", json!({}), Duration::from_secs(1)),
            Err(TransportError::CommandOutcomeUnknown { command_id: 1, .. })
        ));
        assert_eq!(state.lock().unwrap().outgoing.len(), 1);

        let (socket, state) = MockSocket::with_messages([]);
        let mut session = CdpPageSession::new(Box::new(socket));
        session.close().unwrap();
        assert_eq!(state.lock().unwrap().closes, 1);
        assert_eq!(
            session.next_event(Duration::from_secs(1)),
            Err(TransportError::Disconnected)
        );
    }

    struct MockHttp {
        version: Value,
        targets: Value,
    }

    impl DevToolsHttp for MockHttp {
        fn get_json(
            &self,
            _endpoint: DevToolsEndpoint,
            resource: DevToolsResource,
        ) -> Result<Value, TransportError> {
            Ok(match resource {
                DevToolsResource::Version => self.version.clone(),
                DevToolsResource::Targets => self.targets.clone(),
            })
        }
    }

    struct MockConnector {
        endpoints: Arc<Mutex<Vec<Url>>>,
    }

    impl WebSocketConnector for MockConnector {
        fn connect(&self, endpoint: &Url) -> Result<Box<dyn WebSocketConnection>, TransportError> {
            self.endpoints.lock().unwrap().push(endpoint.clone());
            Ok(Box::new(MockSocket::with_messages([]).0))
        }
    }

    #[test]
    fn mocked_target_discovery_and_attachment_are_journaled() {
        let endpoint = DevToolsEndpoint::loopback(9321).unwrap();
        let http = MockHttp {
            version: json!({"Browser":"Microsoft Edge/1.2","Protocol-Version":"1.3","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/browser/test"}),
            targets: json!([
                {"id":"page-1","type":"page","title":"blank","url":"about:blank","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/page-1"},
                {"id":"other-page","type":"page","title":"other","url":"https://example.com/","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/other-page"},
                {"id":"other-1","type":"service_worker"}
            ]),
        };
        let endpoints = Arc::new(Mutex::new(Vec::new()));
        let connector = MockConnector {
            endpoints: endpoints.clone(),
        };
        let mut browser =
            DevToolsBrowserTransport::with_clients(endpoint, Box::new(http), Box::new(connector));
        let (mut run, dir) = capture_run("targets");
        let version = browser.browser_version(&mut run).unwrap();
        assert_eq!(version.browser, "Microsoft Edge/1.2");
        assert_eq!(
            run.manifest().edge_version.as_deref(),
            Some("Microsoft Edge/1.2")
        );
        assert_eq!(run.manifest().cdp_protocol_version.as_deref(), Some("1.3"));
        let targets = browser.list_targets(&mut run).unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|target| target.id == "page-1"));
        assert!(matches!(
            browser.attach("other-page", &mut run),
            Err(TransportError::InvalidEndpoint(_))
        ));
        let mut session = browser.attach("page-1", &mut run).unwrap();
        session.close().unwrap();
        assert_eq!(endpoints.lock().unwrap()[0].port(), Some(9321));
        let kinds = run
            .events()
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"debugging_endpoint_discovered"));
        assert!(kinds.contains(&"cdp_page_targets_discovered"));
        assert!(kinds.contains(&"cdp_target_attached"));
        assert_eq!(
            run.events()
                .iter()
                .find(|event| event.kind == "cdp_page_targets_discovered")
                .unwrap()
                .payload["targets"][0]["type"],
            "page"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_or_stale_endpoint_values_are_rejected() {
        assert!(DevToolsEndpoint::loopback(0).is_err());
        assert!(DevToolsEndpoint::from_active_port("\n/devtools/browser/x").is_err());
        assert!(DevToolsEndpoint::from_active_port("65536\n/devtools/browser/x").is_err());
        assert!(parse_websocket_url("ws://192.168.1.10:9321/devtools/browser/x", 9321).is_err());
        assert!(parse_websocket_url("ws://127.0.0.1:9322/devtools/browser/x", 9321).is_err());
        assert!(parse_websocket_url("ws://user@127.0.0.1:9321/devtools/browser/x", 9321).is_err());
        assert!(allowed_target_url("about:blank"));
        assert!(allowed_target_url("https://chatgpt.com/"));
        assert!(!allowed_target_url("https://chatgpt.com.evil.example/"));
        assert!(!allowed_target_url("https://example.com/"));
    }

    #[test]
    fn duplicate_page_target_ids_are_rejected_as_malformed() {
        let endpoint = DevToolsEndpoint::loopback(9321).unwrap();
        let http = MockHttp {
            version: json!({}),
            targets: json!([
                {"id":"duplicate","type":"page","title":"one","url":"about:blank","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/one"},
                {"id":"duplicate","type":"page","title":"two","url":"about:blank","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/two"}
            ]),
        };
        let mut browser = DevToolsBrowserTransport::with_clients(
            endpoint,
            Box::new(http),
            Box::new(MockConnector {
                endpoints: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        let (mut run, dir) = capture_run("duplicate-targets");
        assert!(matches!(
            browser.list_targets(&mut run),
            Err(TransportError::MalformedMessage(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn loopback_http_client_reads_a_local_devtools_json_response() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 512];
                let read = stream.read(&mut chunk).unwrap();
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&chunk[..read]);
            }
            let request = std::str::from_utf8(&request).unwrap();
            assert!(
                request.starts_with("GET /json/version HTTP/1.1\r\n"),
                "{request:?}"
            );
            let body = r#"{"Browser":"Microsoft Edge/test"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let endpoint = DevToolsEndpoint::loopback(port).unwrap();
        let value = LoopbackDevToolsHttp
            .get_json(endpoint, DevToolsResource::Version)
            .unwrap();
        assert_eq!(value["Browser"], "Microsoft Edge/test");
        server.join().unwrap();
    }
}
