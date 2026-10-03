//! Loopback transport for credential-contained reads from the user's authenticated chatgpt.com tab.
//!
//! The Rust side never receives browser cookies, bearer/session tokens, request headers, Sentinel
//! material, browser storage, or account identifiers. It exposes two typed commands only:
//! authentication status probing and the exact evidence-backed C02 conversation GET.

use chatarium_core::authenticated_session::{
    AuthenticatedSessionLease, SessionAuthenticationEvidence, SessionLeaseError,
    UserAuthenticatedSessionProvider,
};
use chatarium_core::remote::{ProtocolObservationRevision, RemoteConversationId};
use chatarium_protocol::conversation_fetch_request::{
    CONVERSATION_FETCH_REQUEST_OBSERVATION, conversation_fetch_resource,
};
use chatarium_protocol::conversation_list::{
    CONVERSATION_LIST_FIRST_PAGE_RESOURCE, ConversationListPage,
    parse_conversation_list_first_page,
};
use chatarium_store::remote_mirror_runtime::RemoteConversationFetchProvider;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 43_117;
const BRIDGE_HEADER: &str = "x-chatarium-bridge";
const BRIDGE_HEADER_VALUE: &str = "1";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESULT_BODY_BYTES: usize = 6 * 1024 * 1024;
const NEXT_WAIT: Duration = Duration::from_secs(25);
const AUTH_RESULT_WAIT: Duration = Duration::from_secs(5);
const FETCH_RESULT_WAIT: Duration = Duration::from_secs(45);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserBridgeError {
    Unavailable(String),
    Timeout,
    Protocol(String),
    UnsupportedRevision(String),
    HttpStatus(u16),
    RateLimited,
    Unauthenticated,
}

impl fmt::Display for BrowserBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(formatter, "browser bridge unavailable: {detail}"),
            Self::Timeout => write!(formatter, "browser bridge timed out"),
            Self::Protocol(detail) => write!(formatter, "browser bridge protocol error: {detail}"),
            Self::UnsupportedRevision(revision) => {
                write!(
                    formatter,
                    "browser bridge does not implement C02 revision {revision:?}"
                )
            }
            Self::HttpStatus(status) => {
                write!(formatter, "browser-backed ChatGPT request returned HTTP {status}")
            }
            Self::RateLimited => write!(formatter, "browser-backed ChatGPT request returned HTTP 429"),
            Self::Unauthenticated => {
                write!(formatter, "browser ChatGPT session is unauthenticated")
            }
        }
    }
}

impl std::error::Error for BrowserBridgeError {}

#[derive(Debug, Clone)]
struct QueuedCommand {
    id: String,
    kind: &'static str,
    body: Value,
}

#[derive(Debug, Default)]
struct BridgeState {
    next_id: u64,
    queued: VecDeque<QueuedCommand>,
    inflight: BTreeMap<String, &'static str>,
    results: BTreeMap<String, Value>,
    shutdown: bool,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<BridgeState>,
    changed: Condvar,
}

pub struct AccountBridgeRuntime {
    shared: Arc<Shared>,
    address: SocketAddr,
    server: Option<JoinHandle<()>>,
}

impl AccountBridgeRuntime {
    pub fn start() -> io::Result<Self> {
        Self::start_on(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            DEFAULT_PORT,
        ))
    }

    fn start_on(address: SocketAddr) -> io::Result<Self> {
        if !address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "account bridge must bind loopback only",
            ));
        }

        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let server_shared = Arc::clone(&shared);
        let server = thread::Builder::new()
            .name("chatarium-account-bridge".to_owned())
            .spawn(move || serve(listener, server_shared))?;

        Ok(Self {
            shared,
            address,
            server: Some(server),
        })
    }

    pub fn provider(&self) -> BrowserBridgeProvider {
        BrowserBridgeProvider {
            shared: Arc::clone(&self.shared),
        }
    }

    #[cfg(test)]
    fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for AccountBridgeRuntime {
    fn drop(&mut self) {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.shutdown = true;
            self.shared.changed.notify_all();
        }

        // Wake the blocking accept loop. No bridge headers are needed because this connection is
        // only a shutdown nudge and is never treated as an authorized request.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

#[derive(Clone)]
pub struct BrowserBridgeProvider {
    shared: Arc<Shared>,
}

impl BrowserBridgeProvider {
    /// Probe whether the userscript is connected to an authenticated ChatGPT browser session.
    pub fn probe_authentication(
        &mut self,
    ) -> Result<SessionAuthenticationEvidence, BrowserBridgeError> {
        self.authentication_evidence()
    }

    /// Fetch and validate the single evidence-backed first page of ordinary account history.
    pub fn list_recent_conversations(
        &mut self,
    ) -> Result<ConversationListPage, BrowserBridgeError> {
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| provider.fetch_history_first_page())
            .map_err(map_session_lease_error)?
    }

    fn fetch_history_first_page(&self) -> Result<ConversationListPage, BrowserBridgeError> {
        let result = self.call(
            "list_conversations",
            |object| {
                object.insert(
                    "resource".to_owned(),
                    json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE),
                );
            },
            FETCH_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }

        let status = result
            .get("http_status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "successful conversation-list result is missing HTTP status".to_owned(),
                )
            })?;
        if status != 200 {
            return Err(status_error(status));
        }

        let body = result.get("body").ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "successful conversation-list result is missing body".to_owned(),
            )
        })?;
        parse_conversation_list_first_page(body)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))
    }

    /// Fetch one exact existing conversation using only the live browser-held session.
    ///
    /// Reusable authentication material never crosses this boundary. Authentication is probed
    /// and revalidated through the same browser bridge immediately before the C02 read.
    pub fn fetch_authenticated_conversation(
        &mut self,
        remote_conversation_id: &str,
    ) -> Result<Value, BrowserBridgeError> {
        let remote_conversation_id = RemoteConversationId::new(remote_conversation_id)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        let protocol_revision =
            ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION)
                .expect("hard-coded C02 observation revision is non-empty");
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| {
                provider.fetch_conversation(&remote_conversation_id, &protocol_revision)
            })
            .map_err(map_session_lease_error)?
    }

    fn call(
        &self,
        kind: &'static str,
        fields: impl FnOnce(&mut serde_json::Map<String, Value>),
        timeout: Duration,
    ) -> Result<Value, BrowserBridgeError> {
        let id;
        {
            let mut state =
                self.shared.state.lock().map_err(|_| {
                    BrowserBridgeError::Unavailable("bridge state poisoned".to_owned())
                })?;
            if state.shutdown {
                return Err(BrowserBridgeError::Unavailable(
                    "bridge server is shutting down".to_owned(),
                ));
            }
            state.next_id = state.next_id.saturating_add(1);
            id = format!("c{}", state.next_id);

            let mut object = serde_json::Map::new();
            object.insert("version".to_owned(), json!(1));
            object.insert("id".to_owned(), json!(id));
            object.insert("kind".to_owned(), json!(kind));
            fields(&mut object);
            state.queued.push_back(QueuedCommand {
                id: id.clone(),
                kind,
                body: Value::Object(object),
            });
            self.shared.changed.notify_all();
        }

        let deadline = Instant::now() + timeout;
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| BrowserBridgeError::Unavailable("bridge state poisoned".to_owned()))?;

        loop {
            if let Some(result) = state.results.remove(&id) {
                return validate_result(&id, kind, result);
            }
            if state.shutdown {
                cleanup_command(&mut state, &id);
                return Err(BrowserBridgeError::Unavailable(
                    "bridge server shut down before result".to_owned(),
                ));
            }

            let now = Instant::now();
            if now >= deadline {
                cleanup_command(&mut state, &id);
                return Err(BrowserBridgeError::Timeout);
            }
            let remaining = deadline.saturating_duration_since(now);
            let waited = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| BrowserBridgeError::Unavailable("bridge state poisoned".to_owned()))?;
            state = waited.0;
        }
    }
}

impl UserAuthenticatedSessionProvider for BrowserBridgeProvider {
    type Error = BrowserBridgeError;

    fn authentication_evidence(&mut self) -> Result<SessionAuthenticationEvidence, Self::Error> {
        let result = self.call("probe_auth", |_| {}, AUTH_RESULT_WAIT)?;
        match result.get("authentication").and_then(Value::as_str) {
            Some("authenticated") => Ok(SessionAuthenticationEvidence::Authenticated),
            Some("unauthenticated") => Ok(SessionAuthenticationEvidence::Unauthenticated),
            Some("unknown") => Ok(SessionAuthenticationEvidence::Unknown),
            _ => Err(BrowserBridgeError::Protocol(
                "probe_auth result has no supported authentication state".to_owned(),
            )),
        }
    }
}

impl RemoteConversationFetchProvider for BrowserBridgeProvider {
    type FetchError = BrowserBridgeError;

    fn fetch_conversation(
        &mut self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<Value, Self::FetchError> {
        if protocol_revision.as_str() != CONVERSATION_FETCH_REQUEST_OBSERVATION {
            return Err(BrowserBridgeError::UnsupportedRevision(
                protocol_revision.as_str().to_owned(),
            ));
        }

        let resource = conversation_fetch_resource(remote_conversation_id.as_str())
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        let result = self.call(
            "fetch_conversation",
            |object| {
                object.insert(
                    "remote_conversation_id".to_owned(),
                    json!(remote_conversation_id.as_str()),
                );
                object.insert("resource".to_owned(), json!(resource));
            },
            FETCH_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }

        let status = result
            .get("http_status")
            .and_then(Value::as_u64)
            .and_then(|status| u16::try_from(status).ok())
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "successful fetch result is missing HTTP status".to_owned(),
                )
            })?;
        if status != 200 {
            return Err(status_error(status));
        }

        result.get("body").cloned().ok_or_else(|| {
            BrowserBridgeError::Protocol("successful fetch result is missing body".to_owned())
        })
    }
}

fn validate_result(id: &str, kind: &str, result: Value) -> Result<Value, BrowserBridgeError> {
    let object = result.as_object().ok_or_else(|| {
        BrowserBridgeError::Protocol("bridge result must be an object".to_owned())
    })?;
    if object.get("version").and_then(Value::as_u64) != Some(1)
        || object.get("id").and_then(Value::as_str) != Some(id)
        || object.get("kind").and_then(Value::as_str) != Some(kind)
    {
        return Err(BrowserBridgeError::Protocol(
            "bridge result identity/version does not match command".to_owned(),
        ));
    }
    Ok(result)
}

fn remote_result_error(result: &Value) -> BrowserBridgeError {
    if let Some(status) = result
        .get("http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
    {
        return status_error(status);
    }
    BrowserBridgeError::Protocol(
        result
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("remote fetch failed without structured reason")
            .to_owned(),
    )
}

fn map_session_lease_error(error: SessionLeaseError<BrowserBridgeError>) -> BrowserBridgeError {
    match error {
        SessionLeaseError::Provider(error) => error,
        SessionLeaseError::Unauthenticated => BrowserBridgeError::Unauthenticated,
        SessionLeaseError::Unknown => {
            BrowserBridgeError::Unavailable("browser authentication state is unknown".to_owned())
        }
    }
}

fn status_error(status: u16) -> BrowserBridgeError {
    match status {
        401 | 403 => BrowserBridgeError::Unauthenticated,
        429 => BrowserBridgeError::RateLimited,
        other => BrowserBridgeError::HttpStatus(other),
    }
}

fn cleanup_command(state: &mut BridgeState, id: &str) {
    state.queued.retain(|command| command.id != id);
    state.inflight.remove(id);
    state.results.remove(id);
}

fn serve(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        if shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown
        {
            break;
        }

        let (stream, peer) = match listener.accept() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        if shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown
        {
            break;
        }

        let connection_shared = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name("chatarium-account-bridge-connection".to_owned())
            .spawn(move || {
                let _ = handle_connection(stream, &connection_shared);
            });
    }
}

fn handle_connection(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(HttpReadError::PayloadTooLarge) => {
            return write_response(&mut stream, 413, None);
        }
        Err(HttpReadError::Malformed) => {
            return write_response(&mut stream, 400, None);
        }
        Err(HttpReadError::Io(error)) => return Err(error),
    };

    if request.headers.get(BRIDGE_HEADER).map(String::as_str) != Some(BRIDGE_HEADER_VALUE) {
        return write_response(&mut stream, 403, None);
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/next") => handle_next(&mut stream, shared),
        ("POST", "/v1/result") => handle_result(&mut stream, shared, &request.body),
        _ => write_response(&mut stream, 404, None),
    }
}

fn handle_next(stream: &mut TcpStream, shared: &Shared) -> io::Result<()> {
    let deadline = Instant::now() + NEXT_WAIT;
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    loop {
        if state.shutdown {
            return write_response(stream, 204, None);
        }
        if let Some(command) = state.queued.pop_front() {
            state.inflight.insert(command.id.clone(), command.kind);
            let body = serde_json::to_vec(&command.body)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            return write_response(stream, 200, Some(("application/json", &body)));
        }

        let now = Instant::now();
        if now >= deadline {
            return write_response(stream, 204, None);
        }
        let remaining = deadline.saturating_duration_since(now);
        let waited = shared
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|error| error.into_inner());
        state = waited.0;
    }
}

fn handle_result(stream: &mut TcpStream, shared: &Shared, body: &[u8]) -> io::Result<()> {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return write_response(stream, 400, None),
    };
    let Some(object) = value.as_object() else {
        return write_response(stream, 400, None);
    };
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return write_response(stream, 400, None);
    }
    let Some(id) = object.get("id").and_then(Value::as_str) else {
        return write_response(stream, 400, None);
    };
    let Some(kind) = object.get("kind").and_then(Value::as_str) else {
        return write_response(stream, 400, None);
    };

    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(expected_kind) = state.inflight.get(id).copied() else {
        return write_response(stream, 409, None);
    };
    if expected_kind != kind {
        return write_response(stream, 409, None);
    }
    state.inflight.remove(id);
    state.results.insert(id.to_owned(), value);
    shared.changed.notify_all();
    write_response(stream, 204, None)
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

enum HttpReadError {
    Io(io::Error),
    Malformed,
    PayloadTooLarge,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpReadError> {
    let mut bytes = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).map_err(HttpReadError::Io)?;
        if read == 0 {
            return Err(HttpReadError::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES && find_header_end(&bytes).is_none() {
            return Err(HttpReadError::PayloadTooLarge);
        }
        if let Some(end) = find_header_end(&bytes) {
            header_end = end;
            break;
        }
    }

    let header_text =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpReadError::Malformed)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(HttpReadError::Malformed)?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or(HttpReadError::Malformed)?
        .to_owned();
    let path = request_parts
        .next()
        .ok_or(HttpReadError::Malformed)?
        .to_owned();
    let version = request_parts.next().ok_or(HttpReadError::Malformed)?;
    if request_parts.next().is_some() || version != "HTTP/1.1" || !path.starts_with('/') {
        return Err(HttpReadError::Malformed);
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpReadError::Malformed);
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    if headers.contains_key("transfer-encoding") {
        return Err(HttpReadError::Malformed);
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpReadError::Malformed)?,
        None => 0,
    };
    if content_length > MAX_RESULT_BODY_BYTES {
        return Err(HttpReadError::PayloadTooLarge);
    }

    let body_start = header_end + 4;
    while bytes.len() < body_start + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).map_err(HttpReadError::Io)?;
        if read == 0 {
            return Err(HttpReadError::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > body_start + MAX_RESULT_BODY_BYTES {
            return Err(HttpReadError::PayloadTooLarge);
        }
    }
    let body = bytes[body_start..body_start + content_length].to_vec();

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: Option<(&str, &[u8])>,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let (content_type, bytes) = body.unwrap_or(("", &[]));
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        bytes.len()
    )?;
    if !content_type.is_empty() {
        write!(stream, "Content-Type: {content_type}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(bytes)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn request(address: SocketAddr, raw: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(raw).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn browser_exchange(
        address: SocketAddr,
        inspect: impl FnOnce(&Value) + Send + 'static,
        result: impl FnOnce(&Value) -> Value + Send + 'static,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            let raw = request(
                address,
                b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\n\r\n",
            );
            let split = raw
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
            inspect(&command);
            let result = result(&command);
            let body = serde_json::to_vec(&result).unwrap();
            let request_head = format!(
                "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let mut raw = request_head.into_bytes();
            raw.extend_from_slice(&body);
            let response = request(address, &raw);
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
        })
    }

    #[test]
    fn loopback_endpoint_rejects_requests_without_bridge_marker() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn auth_probe_crosses_only_typed_state() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
                assert_eq!(command.as_object().unwrap().len(), 3);
            },
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "probe_auth",
                    "ok": true,
                    "authentication": "authenticated",
                    "http_status": 200
                })
            },
        );

        let mut provider = runtime.provider();
        assert_eq!(
            provider.authentication_evidence().unwrap(),
            SessionAuthenticationEvidence::Authenticated
        );
        browser.join().unwrap();
    }

    #[test]
    fn c01_command_uses_exact_first_page_resource_and_parses_response() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            for expected_kind in ["probe_auth", "probe_auth", "list_conversations"] {
                let raw = request(
                    address,
                    b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\n\r\n",
                );
                let split = raw
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap();
                let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
                assert_eq!(command["kind"], json!(expected_kind));

                if expected_kind == "list_conversations" {
                    assert_eq!(
                        command["resource"],
                        json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE)
                    );
                    assert_eq!(command.as_object().unwrap().len(), 4);
                }

                let result = if expected_kind == "probe_auth" {
                    json!({
                        "version": 1,
                        "id": command["id"],
                        "kind": "probe_auth",
                        "ok": true,
                        "authentication": "authenticated",
                        "http_status": 200
                    })
                } else {
                    json!({
                        "version": 1,
                        "id": command["id"],
                        "kind": "list_conversations",
                        "ok": true,
                        "http_status": 200,
                        "content_type": "application/json",
                        "body": {
                            "items": [
                                {
                                    "id": "remote-1",
                                    "title": "One",
                                    "create_time": "2026-09-30T18:32:58Z",
                                    "update_time": "2026-09-30T21:04:06Z"
                                }
                            ],
                            "total": 21,
                            "limit": 20,
                            "offset": 0
                        }
                    })
                };
                let body = serde_json::to_vec(&result).unwrap();
                let request_head = format!(
                    "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let mut raw = request_head.into_bytes();
                raw.extend_from_slice(&body);
                let response = request(address, &raw);
                assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
            }
        });

        let mut provider = runtime.provider();
        let page = provider.list_recent_conversations().unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "remote-1");
        assert_eq!(page.total, 21);
        browser.join().unwrap();
    }

    #[test]
    fn c02_command_uses_exact_evidence_backed_resource() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("fetch_conversation"));
                assert_eq!(
                    command["resource"],
                    json!(
                        "/backend-api/conversations/opaque%2Fremote%20id?num_turns=10&include_has_versions=true"
                    )
                );
            },
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "fetch_conversation",
                    "ok": true,
                    "http_status": 200,
                    "content_type": "application/json",
                    "body": {"conversation_id": "opaque/remote id"}
                })
            },
        );

        let mut provider = runtime.provider();
        let body = provider
            .fetch_conversation(
                &RemoteConversationId::new("opaque/remote id").unwrap(),
                &ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION).unwrap(),
            )
            .unwrap();
        assert_eq!(body["conversation_id"], json!("opaque/remote id"));
        browser.join().unwrap();
    }

    #[test]
    fn rate_limit_is_distinct_and_never_retried_here() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |_| {},
            |command| {
                json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "fetch_conversation",
                    "ok": false,
                    "http_status": 429,
                    "content_type": "application/json",
                    "error": "remote_http_status"
                })
            },
        );

        let mut provider = runtime.provider();
        assert_eq!(
            provider.fetch_conversation(
                &RemoteConversationId::new("remote").unwrap(),
                &ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION).unwrap(),
            ),
            Err(BrowserBridgeError::RateLimited)
        );
        browser.join().unwrap();
    }
}
