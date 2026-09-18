//! Mockable localhost-only Chrome DevTools Protocol transport.

use crate::run::CaptureRun;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::Message;
use tungstenite::client;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::WebSocket;
use url::{Host, Url};

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
    /// A local DevTools socket operation failed in a way that can be transient during startup.
    ReadinessTransient(String),
    /// The bounded browser startup deadline expired while DevTools remained unavailable.
    ReadinessTimeout {
        /// Number of bounded DevTools metadata attempts made.
        attempts: u32,
        /// Last transient connection/readiness failure observed.
        last_error: String,
    },
    /// A DevTools HTTP endpoint returned an invalid response.
    Http(String),
    /// A CDP JSON message was malformed or unsupported.
    MalformedMessage(String),
    /// The WebSocket closed before the requested operation completed.
    Disconnected,
    /// A byte stream reached EOF; the flag identifies a truncated ASCIIZ message.
    Eof {
        /// EOF arrived after message bytes but before their NUL delimiter.
        unterminated_message: bool,
    },
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
    /// A flattened CDP response arrived for a different session than the command used.
    UnexpectedSessionId {
        /// Session ID attached to the command, if any.
        expected: Option<String>,
        /// Session ID returned by CDP, if any.
        received: Option<String>,
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
    /// Microsoft Edge policy explicitly disables remote debugging.
    RemoteDebuggingDisabled(String),
    /// Startup diagnostics could not be durably journaled; the primary failure is preserved.
    DiagnosticJournalFailure {
        /// Startup/readiness failure, if one occurred before the diagnostic append failure.
        primary_failure: Option<String>,
        /// One or more failed diagnostic event appends.
        journal_failure: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint(reason) => {
                write!(formatter, "invalid DevTools endpoint: {reason}")
            }
            Self::Io(reason) => write!(formatter, "transport I/O: {reason}"),
            Self::ReadinessTransient(reason) => {
                write!(formatter, "transient DevTools readiness I/O: {reason}")
            }
            Self::ReadinessTimeout {
                attempts,
                last_error,
            } => write!(
                formatter,
                "DevTools readiness deadline expired after {attempts} attempts; last error: {last_error}"
            ),
            Self::Http(reason) => write!(formatter, "DevTools HTTP: {reason}"),
            Self::MalformedMessage(reason) => write!(formatter, "malformed CDP message: {reason}"),
            Self::Disconnected => formatter.write_str("CDP WebSocket disconnected"),
            Self::Eof {
                unterminated_message: true,
            } => formatter.write_str("CDP pipe reached EOF before the message delimiter"),
            Self::Eof {
                unterminated_message: false,
            } => formatter.write_str("CDP pipe reached EOF between messages"),
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
            Self::UnexpectedSessionId { expected, received } => write!(
                formatter,
                "unexpected CDP session ID {received:?} while waiting for {expected:?}"
            ),
            Self::UnknownTarget(target) => {
                write!(formatter, "page target '{target}' was not discovered")
            }
            Self::Journal(reason) => write!(formatter, "capture journal: {reason}"),
            Self::Process(reason) => write!(formatter, "Edge process: {reason}"),
            Self::UnsafeProfile(reason) => write!(formatter, "unsafe Edge profile: {reason}"),
            Self::StaleState(reason) => write!(formatter, "stale Edge capture state: {reason}"),
            Self::RemoteDebuggingDisabled(reason) => write!(
                formatter,
                "Microsoft Edge remote debugging is disabled by policy: {reason}"
            ),
            Self::DiagnosticJournalFailure {
                primary_failure: Some(primary),
                journal_failure,
            } => write!(
                formatter,
                "{primary}; diagnostic journal failure: {journal_failure}"
            ),
            Self::DiagnosticJournalFailure {
                primary_failure: None,
                journal_failure,
            } => write!(formatter, "diagnostic journal failure: {journal_failure}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// Validated port selected by the harness-owned browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevToolsPort {
    port: u16,
}

impl DevToolsPort {
    /// Construct a selected DevTools port from a nonzero value.
    pub fn new(port: u16) -> Result<Self, TransportError> {
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
        Self::new(port)
    }

    /// Port selected by the harness-owned browser.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port
    }
}

/// The concrete loopback address selected by readiness for this browser launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackAddressFamily {
    /// IPv4 loopback address `127.0.0.1`.
    Ipv4,
    /// IPv6 loopback address `::1`.
    Ipv6,
}

impl LoopbackAddressFamily {
    /// Candidate IP address probed for this family.
    #[must_use]
    pub const fn address(self) -> IpAddr {
        match self {
            Self::Ipv4 => IpAddr::V4(Ipv4Addr::LOCALHOST),
            Self::Ipv6 => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    }

    /// Stable journal value for the concrete loopback family.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
        }
    }
}

/// Validated DevTools socket endpoint: one selected port and one concrete loopback address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevToolsEndpoint {
    port: DevToolsPort,
    family: LoopbackAddressFamily,
}

impl DevToolsEndpoint {
    /// Construct one of the two permitted concrete loopback endpoints.
    pub fn loopback(port: DevToolsPort, family: LoopbackAddressFamily) -> Self {
        Self { port, family }
    }

    /// Port selected by the harness-owned browser.
    #[must_use]
    pub const fn port(self) -> u16 {
        self.port.port()
    }

    /// Concrete loopback family pinned for network connections.
    #[must_use]
    pub const fn family(self) -> LoopbackAddressFamily {
        self.family
    }

    /// Concrete IP address pinned for network connections.
    #[must_use]
    pub const fn address(self) -> IpAddr {
        self.family.address()
    }

    fn socket_addr(self) -> SocketAddr {
        SocketAddr::new(self.address(), self.port())
    }

    fn host_header(self) -> String {
        match self.family {
            LoopbackAddressFamily::Ipv4 => format!("127.0.0.1:{}", self.port()),
            LoopbackAddressFamily::Ipv6 => format!("[::1]:{}", self.port()),
        }
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
    pub(crate) websocket_url: Option<Url>,
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
    /// Raw flattened-session provenance when this event belongs to an attached target.
    pub session_id: Option<String>,
}

/// Mockable browser discovery and page-attachment boundary.
pub trait BrowserTransport: Send {
    /// Fetch browser and CDP version metadata and journal endpoint discovery.
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError>;
    /// Fetch browser metadata with a bounded transport attempt when supported.
    fn browser_version_with_timeout(
        &mut self,
        run: &mut CaptureRun,
        _timeout: Duration,
    ) -> Result<BrowserVersion, TransportError> {
        self.browser_version(run)
    }
    /// Discover page targets and journal the raw target identifiers/types.
    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError>;
    /// Re-query page targets after an external wait rather than relying on an earlier snapshot.
    fn refresh_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        self.list_targets(run)
    }
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

    /// Fetch one resource with a caller-supplied upper bound for the complete I/O attempt.
    ///
    /// Mock clients may keep the default implementation because they do not perform blocking I/O.
    fn get_json_with_timeout(
        &self,
        endpoint: DevToolsEndpoint,
        resource: DevToolsResource,
        _timeout: Duration,
    ) -> Result<Value, TransportError> {
        self.get_json(endpoint, resource)
    }
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

/// Mockable WebSocket connection factory with an explicitly pinned socket destination.
pub trait WebSocketConnector: Send + Sync {
    /// Preserve URL handshake metadata while connecting only to the selected loopback endpoint.
    fn connect(
        &self,
        browser_url: &Url,
        selected_endpoint: DevToolsEndpoint,
    ) -> Result<Box<dyn WebSocketConnection>, TransportError>;
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
        self.get_json_with_timeout(endpoint, resource, IO_TIMEOUT)
    }

    fn get_json_with_timeout(
        &self,
        endpoint: DevToolsEndpoint,
        resource: DevToolsResource,
        timeout: Duration,
    ) -> Result<Value, TransportError> {
        let deadline = Instant::now() + timeout;
        let mut stream =
            TcpStream::connect_timeout(&endpoint.socket_addr(), remaining_io_time(deadline)?)
                .map_err(map_readiness_io)?;
        stream
            .set_write_timeout(Some(remaining_io_time(deadline)?))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        write!(
            stream,
            "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
            resource.path(),
            endpoint.host_header()
        )
        .map_err(map_readiness_io)?;
        stream
            .set_read_timeout(Some(remaining_io_time(deadline)?))
            .map_err(|error| TransportError::Io(error.to_string()))?;

        let mut response = Vec::new();
        stream
            .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut response)
            .map_err(map_readiness_io)?;
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
            let reason = format!("DevTools returned HTTP {status}");
            return Err(if matches!(status, 500 | 502 | 503 | 504) {
                TransportError::ReadinessTransient(reason)
            } else {
                TransportError::Http(reason)
            });
        }
        let body = &response[body_start + 4..];
        serde_json::from_slice(body)
            .map_err(|error| TransportError::Http(format!("invalid JSON response: {error}")))
    }
}

fn remaining_io_time(deadline: Instant) -> Result<Duration, TransportError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(TransportError::ReadinessTransient(
            "DevTools I/O attempt deadline expired".to_owned(),
        ))
    } else {
        Ok(remaining)
    }
}

fn map_readiness_io(error: std::io::Error) -> TransportError {
    let transient_kind = matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
    );
    let transient_windows_code =
        matches!(error.raw_os_error(), Some(10035 | 10054 | 10060 | 10061));
    if transient_kind || transient_windows_code {
        TransportError::ReadinessTransient(error.to_string())
    } else {
        TransportError::Io(error.to_string())
    }
}

/// Real WebSocket connector restricted to the selected concrete loopback endpoint.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoopbackWebSocketConnector;

impl WebSocketConnector for LoopbackWebSocketConnector {
    fn connect(
        &self,
        browser_url: &Url,
        selected_endpoint: DevToolsEndpoint,
    ) -> Result<Box<dyn WebSocketConnection>, TransportError> {
        let destination = validated_websocket_destination(browser_url, selected_endpoint)?;
        let stream = TcpStream::connect_timeout(&destination, IO_TIMEOUT)
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
        let request = browser_url
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
    port: DevToolsPort,
    selected_endpoint: Option<DevToolsEndpoint>,
    http: Box<dyn DevToolsHttp>,
    websocket: Box<dyn WebSocketConnector>,
    version: Option<BrowserVersion>,
    targets: HashMap<String, TargetInfo>,
}

impl DevToolsBrowserTransport {
    /// Build a transport pinned to one validated concrete loopback endpoint.
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
            port: DevToolsPort {
                port: endpoint.port(),
            },
            selected_endpoint: Some(endpoint),
            http,
            websocket,
            version: None,
            targets: HashMap::new(),
        }
    }

    pub(crate) fn for_readiness(
        port: DevToolsPort,
        http: Box<dyn DevToolsHttp>,
        websocket: Box<dyn WebSocketConnector>,
    ) -> Self {
        Self {
            port,
            selected_endpoint: None,
            http,
            websocket,
            version: None,
            targets: HashMap::new(),
        }
    }

    /// Concrete endpoint selected by readiness, if one has succeeded.
    #[must_use]
    pub const fn selected_endpoint(&self) -> Option<DevToolsEndpoint> {
        self.selected_endpoint
    }
}

impl DevToolsBrowserTransport {
    pub(crate) fn probe_browser_version_at(
        &mut self,
        endpoint: DevToolsEndpoint,
        run: &mut CaptureRun,
        timeout: Duration,
    ) -> Result<BrowserVersion, TransportError> {
        if endpoint.port() != self.port.port() {
            return Err(TransportError::InvalidEndpoint(
                "readiness endpoint port does not match DevToolsActivePort".to_owned(),
            ));
        }
        self.fetch_browser_version_at(endpoint, run, Some(timeout))
    }

    fn require_selected_endpoint(&self) -> Result<DevToolsEndpoint, TransportError> {
        self.selected_endpoint.ok_or_else(|| {
            TransportError::InvalidEndpoint(
                "DevTools loopback address has not passed readiness".to_owned(),
            )
        })
    }

    fn fetch_browser_version(
        &mut self,
        run: &mut CaptureRun,
        timeout: Option<Duration>,
    ) -> Result<BrowserVersion, TransportError> {
        let endpoint = self.require_selected_endpoint()?;
        self.fetch_browser_version_at(endpoint, run, timeout)
    }

    fn fetch_browser_version_at(
        &mut self,
        endpoint: DevToolsEndpoint,
        run: &mut CaptureRun,
        timeout: Option<Duration>,
    ) -> Result<BrowserVersion, TransportError> {
        if let Some(version) = &self.version {
            return Ok(version.clone());
        }
        let value = match timeout {
            Some(timeout) => {
                self.http
                    .get_json_with_timeout(endpoint, DevToolsResource::Version, timeout)?
            }
            None => self.http.get_json(endpoint, DevToolsResource::Version)?,
        };
        let browser = required_string(&value, "Browser")?.to_owned();
        let protocol_version = required_string(&value, "Protocol-Version")?.to_owned();
        let websocket_raw = required_string(&value, "webSocketDebuggerUrl")?;
        parse_websocket_url(websocket_raw, self.port.port())?;
        let version = BrowserVersion {
            browser,
            protocol_version,
        };
        run.append_event(
            "debugging_endpoint_discovered",
            json!({
                "address": endpoint.address().to_string(),
                "address_family": endpoint.family().as_str(),
                "port": endpoint.port(),
                "browser": version.browser,
                "protocol_version": version.protocol_version,
            }),
        )
        .map_err(TransportError::Journal)?;
        run.set_browser_versions(version.browser.clone(), version.protocol_version.clone())
            .map_err(TransportError::Journal)?;
        self.selected_endpoint = Some(endpoint);
        self.version = Some(version.clone());
        Ok(version)
    }
}

impl BrowserTransport for DevToolsBrowserTransport {
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError> {
        self.fetch_browser_version(run, None)
    }

    fn browser_version_with_timeout(
        &mut self,
        run: &mut CaptureRun,
        timeout: Duration,
    ) -> Result<BrowserVersion, TransportError> {
        self.fetch_browser_version(run, Some(timeout))
    }

    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        let endpoint = self.require_selected_endpoint()?;
        let value = self.http.get_json(endpoint, DevToolsResource::Targets)?;
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
            let websocket_url = Some(parse_websocket_url(websocket_raw, self.port.port())?);
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
        let socket = self.websocket.connect(
            target.websocket_url.as_ref().ok_or_else(|| {
                TransportError::InvalidEndpoint("page target has no WebSocket URL".to_owned())
            })?,
            self.require_selected_endpoint()?,
        )?;
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
    session_id: Option<String>,
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
            session_id: object
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
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
            session_id: object
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            result: None,
            error: Some((code, message)),
        }));
    }
    let result = object.get("result").cloned().ok_or_else(|| {
        TransportError::MalformedMessage("response has neither result nor error".to_owned())
    })?;
    Ok(ParsedMessage::Response(ParsedResponse {
        id,
        session_id: object
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        result: Some(result),
        error: None,
    }))
}

/// Wire-level CDP message channel shared by WebSocket and ASCIIZ pipe sessions.
pub trait CdpMessageChannel: Send {
    /// Send one raw JSON message.
    fn send_message(&mut self, text: &str) -> Result<(), TransportError>;
    /// Receive one complete raw JSON message, or `None` when the wait expires.
    fn receive_message(&mut self, timeout: Duration) -> Result<Option<String>, TransportError>;
    /// Close the underlying transport.
    fn close(&mut self) -> Result<(), TransportError>;
    /// Whether the message channel has already observed a closed transport.
    fn is_closed(&self) -> bool {
        false
    }
}

struct WebSocketMessageChannel(Box<dyn WebSocketConnection>);

impl CdpMessageChannel for WebSocketMessageChannel {
    fn send_message(&mut self, text: &str) -> Result<(), TransportError> {
        self.0.send_text(text)
    }
    fn receive_message(&mut self, timeout: Duration) -> Result<Option<String>, TransportError> {
        self.0.receive_text(timeout)
    }
    fn close(&mut self) -> Result<(), TransportError> {
        self.0.close()
    }
}

pub(crate) struct CdpSessionCore {
    channel: Box<dyn CdpMessageChannel>,
    next_id: u64,
    events: HashMap<Option<String>, VecDeque<CdpEvent>>,
    responses: HashMap<u64, ParsedResponse>,
    closed: bool,
}

/// One synchronous CDP command/event session with shared ID correlation and session routing.
pub struct CdpPageSession {
    core: std::sync::Arc<std::sync::Mutex<CdpSessionCore>>,
    session_id: Option<String>,
    close_channel_on_close: bool,
    closed: bool,
}

impl CdpPageSession {
    /// Create a CDP session over an injected WebSocket.
    #[must_use]
    pub fn new(websocket: Box<dyn WebSocketConnection>) -> Self {
        Self::with_channel(Box::new(WebSocketMessageChannel(websocket)))
    }

    /// Create a browser-wide CDP root session over a reusable message channel.
    #[must_use]
    pub fn with_channel(channel: Box<dyn CdpMessageChannel>) -> Self {
        Self {
            core: std::sync::Arc::new(std::sync::Mutex::new(CdpSessionCore {
                channel,
                next_id: 1,
                events: HashMap::new(),
                responses: HashMap::new(),
                closed: false,
            })),
            session_id: None,
            close_channel_on_close: true,
            closed: false,
        }
    }

    pub(crate) fn attached_to(&self, _target_id: String, session_id: String) -> Self {
        Self {
            core: self.core.clone(),
            session_id: Some(session_id),
            close_channel_on_close: false,
            closed: false,
        }
    }

    /// Whether this root session or its underlying channel is already closed.
    pub(crate) fn is_closed(&self) -> bool {
        if self.closed {
            return true;
        }
        self.core
            .lock()
            .map(|core| core.closed || core.channel.is_closed())
            .unwrap_or(true)
    }
}

fn cdp_command(
    core: &mut CdpSessionCore,
    session_id: Option<&str>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, TransportError> {
    if core.closed {
        return Err(TransportError::Disconnected);
    }
    if method.is_empty() || !params.is_object() {
        return Err(TransportError::MalformedMessage(
            "command requires a nonempty method and object params".to_owned(),
        ));
    }
    let id = core.next_id;
    core.next_id = core
        .next_id
        .checked_add(1)
        .ok_or_else(|| TransportError::MalformedMessage("command ID exhausted".to_owned()))?;
    let mut command = json!({ "id": id, "method": method, "params": params });
    if let Some(session_id) = session_id {
        command["sessionId"] = json!(session_id);
    }
    let text = serde_json::to_string(&command)
        .map_err(|error| TransportError::MalformedMessage(error.to_string()))?;
    if let Err(error) = core.channel.send_message(&text) {
        return Err(TransportError::CommandOutcomeUnknown {
            command_id: id,
            reason: error.to_string(),
        });
    }
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(response) = core.responses.remove(&id) {
            if response.session_id.as_deref() != session_id {
                return Err(TransportError::UnexpectedSessionId {
                    expected: session_id.map(str::to_owned),
                    received: response.session_id,
                });
            }
            return response_result(response);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TransportError::CommandOutcomeUnknown {
                command_id: id,
                reason: "timed out waiting for the matching response".to_owned(),
            });
        }
        let text = match core.channel.receive_message(remaining) {
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
        match parse_cdp_message(&text).map_err(|error| TransportError::CommandOutcomeUnknown {
            command_id: id,
            reason: error.to_string(),
        })? {
            ParsedMessage::Event(event) => queue_cdp_event(core, event).map_err(|error| {
                TransportError::CommandOutcomeUnknown {
                    command_id: id,
                    reason: error.to_string(),
                }
            })?,
            ParsedMessage::Response(response) if response.id == id => {
                if response.session_id.as_deref() != session_id {
                    return Err(TransportError::UnexpectedSessionId {
                        expected: session_id.map(str::to_owned),
                        received: response.session_id,
                    });
                }
                return response_result(response);
            }
            ParsedMessage::Response(response) => {
                if core.responses.len() >= MAX_QUEUED_MESSAGES {
                    return Err(TransportError::CommandOutcomeUnknown {
                        command_id: id,
                        reason: "too many unmatched CDP responses".to_owned(),
                    });
                }
                core.responses.insert(response.id, response);
            }
        }
    }
}

fn queue_cdp_event(core: &mut CdpSessionCore, event: CdpEvent) -> Result<(), TransportError> {
    let queue = core.events.entry(event.session_id.clone()).or_default();
    if queue.len() >= MAX_QUEUED_MESSAGES {
        return Err(TransportError::MalformedMessage(
            "too many queued CDP events".to_owned(),
        ));
    }
    queue.push_back(event);
    Ok(())
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
        let mut core = self.core.lock().map_err(|_| TransportError::Disconnected)?;
        cdp_command(
            &mut core,
            self.session_id.as_deref(),
            method,
            params,
            timeout,
        )
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<CdpEvent>, TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        let mut core = self.core.lock().map_err(|_| TransportError::Disconnected)?;
        if let Some(event) = core
            .events
            .entry(self.session_id.clone())
            .or_default()
            .pop_front()
        {
            return Ok(Some(event));
        }
        if core.closed {
            return Err(TransportError::Disconnected);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let Some(text) = core.channel.receive_message(remaining)? else {
                return Ok(None);
            };
            match parse_cdp_message(&text)? {
                ParsedMessage::Event(event) if event.session_id == self.session_id => {
                    return Ok(Some(event));
                }
                ParsedMessage::Event(event) => queue_cdp_event(&mut core, event)?,
                ParsedMessage::Response(response) => {
                    if core.responses.len() >= MAX_QUEUED_MESSAGES {
                        return Err(TransportError::MalformedMessage(
                            "too many unmatched CDP responses".to_owned(),
                        ));
                    }
                    core.responses.insert(response.id, response);
                }
            }
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if let Some(session_id) = &self.session_id {
            let mut core = self.core.lock().map_err(|_| TransportError::Disconnected)?;
            let result = cdp_command(
                &mut core,
                None,
                "Target.detachFromTarget",
                json!({ "sessionId": session_id }),
                Duration::from_secs(2),
            );
            result.map(|_| ())
        } else if self.close_channel_on_close {
            let mut core = self.core.lock().map_err(|_| TransportError::Disconnected)?;
            core.closed = true;
            core.channel.close()
        } else {
            Ok(())
        }
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

fn validated_websocket_destination(
    browser_url: &Url,
    selected_endpoint: DevToolsEndpoint,
) -> Result<SocketAddr, TransportError> {
    validate_websocket_url(browser_url, Some(selected_endpoint.port()))?;
    Ok(selected_endpoint.socket_addr())
}

fn validate_websocket_url(
    endpoint: &Url,
    expected_port: Option<u16>,
) -> Result<u16, TransportError> {
    let loopback_host = match endpoint.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if endpoint.scheme() != "ws"
        || !loopback_host
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(TransportError::InvalidEndpoint(
            "only credential-free ws://localhost or literal loopback endpoints are allowed"
                .to_owned(),
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
    fn flattened_pipe_sessions_preserve_session_id_for_commands_and_route_events() {
        let messages = [
            Ok(r#"{"method":"Page.loadEventFired","params":{"timestamp":1},"sessionId":"session-a"}"#.to_owned()),
            Ok(r#"{"id":1,"result":{"product":"Microsoft Edge/1","protocolVersion":"1.3"}}"#.to_owned()),
            Ok(r#"{"id":2,"result":{"frameTree":{"frame":{"url":"about:blank"}}},"sessionId":"session-a"}"#.to_owned()),
            Ok(r#"{"id":3,"result":{}}"#.to_owned()),
        ];
        let (socket, state) = MockSocket::with_messages(messages);
        let mut browser = CdpPageSession::new(Box::new(socket));
        browser
            .command("Browser.getVersion", json!({}), Duration::from_secs(1))
            .unwrap();
        let mut page = browser.attached_to("target-a".to_owned(), "session-a".to_owned());
        let event = page.next_event(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(event.method, "Page.loadEventFired");
        assert_eq!(event.session_id.as_deref(), Some("session-a"));
        let result = page
            .command("Page.getFrameTree", json!({}), Duration::from_secs(1))
            .unwrap();
        assert_eq!(result["frameTree"]["frame"]["url"], "about:blank");
        page.close().unwrap();

        let outgoing = &state.lock().unwrap().outgoing;
        assert_eq!(outgoing.len(), 3);
        assert_eq!(
            serde_json::from_str::<Value>(&outgoing[1]).unwrap()["sessionId"],
            "session-a"
        );
        let detach = serde_json::from_str::<Value>(&outgoing[2]).unwrap();
        assert_eq!(detach["method"], "Target.detachFromTarget");
        assert_eq!(detach["params"]["sessionId"], "session-a");
    }

    #[test]
    fn malformed_pipe_json_and_wrong_flattened_response_session_are_visible() {
        let (socket, _) = MockSocket::with_messages([Ok("not-json".to_owned())]);
        let mut session = CdpPageSession::new(Box::new(socket));
        assert!(matches!(
            session.command("Browser.getVersion", json!({}), Duration::from_secs(1)),
            Err(TransportError::CommandOutcomeUnknown { .. })
        ));

        let (socket, _) = MockSocket::with_messages([Ok(
            r#"{"id":1,"result":{},"sessionId":"wrong"}"#.to_owned(),
        )]);
        let browser = CdpPageSession::new(Box::new(socket));
        let mut session = browser.attached_to("target-a".to_owned(), "session-a".to_owned());
        assert!(matches!(
            session.command("Page.getFrameTree", json!({}), Duration::from_secs(1)),
            Err(TransportError::UnexpectedSessionId { .. })
        ));
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
        observed_endpoints: Option<Arc<Mutex<Vec<(DevToolsResource, DevToolsEndpoint)>>>>,
    }

    impl DevToolsHttp for MockHttp {
        fn get_json(
            &self,
            endpoint: DevToolsEndpoint,
            resource: DevToolsResource,
        ) -> Result<Value, TransportError> {
            if let Some(observed) = &self.observed_endpoints {
                observed.lock().unwrap().push((resource, endpoint));
            }
            Ok(match resource {
                DevToolsResource::Version => self.version.clone(),
                DevToolsResource::Targets => self.targets.clone(),
            })
        }
    }

    struct MockConnector {
        endpoints: Arc<Mutex<Vec<(Url, DevToolsEndpoint)>>>,
    }

    impl WebSocketConnector for MockConnector {
        fn connect(
            &self,
            browser_url: &Url,
            selected_endpoint: DevToolsEndpoint,
        ) -> Result<Box<dyn WebSocketConnection>, TransportError> {
            self.endpoints
                .lock()
                .unwrap()
                .push((browser_url.clone(), selected_endpoint));
            Ok(Box::new(MockSocket::with_messages([]).0))
        }
    }

    #[test]
    fn mocked_target_discovery_and_attachment_are_journaled() {
        let port = DevToolsPort::new(9321).unwrap();
        let endpoint = DevToolsEndpoint::loopback(port, LoopbackAddressFamily::Ipv6);
        let observed_http = Arc::new(Mutex::new(Vec::new()));
        let http = MockHttp {
            version: json!({"Browser":"Microsoft Edge/1.2","Protocol-Version":"1.3","webSocketDebuggerUrl":"ws://localhost:9321/devtools/browser/test"}),
            targets: json!([
                {"id":"page-1","type":"page","title":"blank","url":"about:blank","webSocketDebuggerUrl":"ws://localhost:9321/devtools/page/page-1"},
                {"id":"other-page","type":"page","title":"other","url":"https://example.com/","webSocketDebuggerUrl":"ws://localhost:9321/devtools/page/other-page"},
                {"id":"other-1","type":"service_worker"}
            ]),
            observed_endpoints: Some(observed_http.clone()),
        };
        let endpoints: Arc<Mutex<Vec<(Url, DevToolsEndpoint)>>> = Arc::new(Mutex::new(Vec::new()));
        let connector = MockConnector {
            endpoints: endpoints.clone(),
        };
        let mut browser =
            DevToolsBrowserTransport::with_clients(endpoint, Box::new(http), Box::new(connector));
        let (mut run, dir) = capture_run("targets");
        let version = browser
            .probe_browser_version_at(endpoint, &mut run, Duration::from_millis(50))
            .unwrap();
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
        assert_eq!(endpoints.lock().unwrap()[0].0.port(), Some(9321));
        assert_eq!(endpoints.lock().unwrap()[0].0.host_str(), Some("localhost"));
        assert_eq!(
            endpoints.lock().unwrap()[0].1.family(),
            LoopbackAddressFamily::Ipv6
        );
        assert!(
            observed_http
                .lock()
                .unwrap()
                .iter()
                .any(|(resource, selected)| {
                    *resource == DevToolsResource::Targets && *selected == endpoint
                })
        );
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
        assert!(DevToolsPort::new(0).is_err());
        assert!(DevToolsPort::from_active_port("\n/devtools/browser/x").is_err());
        assert!(DevToolsPort::from_active_port("65536\n/devtools/browser/x").is_err());
        assert!(parse_websocket_url("ws://192.168.1.10:9321/devtools/browser/x", 9321).is_err());
        assert!(parse_websocket_url("ws://[2001:db8::1]:9321/devtools/browser/x", 9321).is_err());
        assert!(
            parse_websocket_url("ws://not-localhost.example:9321/devtools/browser/x", 9321)
                .is_err()
        );
        assert!(parse_websocket_url("ws://127.0.0.1:9322/devtools/browser/x", 9321).is_err());
        assert!(parse_websocket_url("ws://user@127.0.0.1:9321/devtools/browser/x", 9321).is_err());
        assert!(
            parse_websocket_url("ws://localhost:9321/devtools/browser/x?token=1", 9321).is_err()
        );
        for url in [
            "ws://localhost:9321/devtools/browser/x",
            "ws://127.0.0.1:9321/devtools/browser/x",
            "ws://[::1]:9321/devtools/browser/x",
        ] {
            assert!(parse_websocket_url(url, 9321).is_ok(), "rejected {url}");
        }
        assert!(allowed_target_url("about:blank"));
        assert!(allowed_target_url("https://chatgpt.com/"));
        assert!(!allowed_target_url("https://chatgpt.com.evil.example/"));
        assert!(!allowed_target_url("https://example.com/"));
    }

    #[test]
    fn localhost_websocket_metadata_keeps_path_but_uses_only_selected_ipv6_socket() {
        let port = DevToolsPort::new(9321).unwrap();
        let selected = DevToolsEndpoint::loopback(port, LoopbackAddressFamily::Ipv6);
        let browser_url = Url::parse("ws://localhost:9321/devtools/page/page-7").unwrap();

        let destination = validated_websocket_destination(&browser_url, selected).unwrap();

        assert_eq!(destination, "[::1]:9321".parse().unwrap());
        assert_eq!(browser_url.host_str(), Some("localhost"));
        assert_eq!(browser_url.path(), "/devtools/page/page-7");
    }

    #[test]
    fn duplicate_page_target_ids_are_rejected_as_malformed() {
        let port = DevToolsPort::new(9321).unwrap();
        let endpoint = DevToolsEndpoint::loopback(port, LoopbackAddressFamily::Ipv4);
        let http = MockHttp {
            version: json!({}),
            targets: json!([
                {"id":"duplicate","type":"page","title":"one","url":"about:blank","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/one"},
                {"id":"duplicate","type":"page","title":"two","url":"about:blank","webSocketDebuggerUrl":"ws://127.0.0.1:9321/devtools/page/two"}
            ]),
            observed_endpoints: None,
        };
        let mut browser = DevToolsBrowserTransport::with_clients(
            endpoint,
            Box::new(http),
            Box::new(MockConnector {
                endpoints: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        browser.selected_endpoint = Some(endpoint);
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
        let endpoint = DevToolsEndpoint::loopback(
            DevToolsPort::new(port).unwrap(),
            LoopbackAddressFamily::Ipv4,
        );
        let value = LoopbackDevToolsHttp
            .get_json(endpoint, DevToolsResource::Version)
            .unwrap();
        assert_eq!(value["Browser"], "Microsoft Edge/test");
        server.join().unwrap();
    }

    #[test]
    fn readiness_http_attempt_obeys_its_single_io_deadline() {
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
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
            }
            thread::sleep(Duration::from_millis(180));
        });

        let started = Instant::now();
        let result = LoopbackDevToolsHttp.get_json_with_timeout(
            DevToolsEndpoint::loopback(
                DevToolsPort::new(port).unwrap(),
                LoopbackAddressFamily::Ipv4,
            ),
            DevToolsResource::Version,
            Duration::from_millis(35),
        );
        let elapsed = started.elapsed();
        assert!(matches!(result, Err(TransportError::ReadinessTransient(_))));
        assert!(elapsed < Duration::from_millis(140), "elapsed: {elapsed:?}");
        server.join().unwrap();
    }
}
