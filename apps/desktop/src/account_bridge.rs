//! Loopback transport for credential-contained reads from the user's authenticated chatgpt.com tab.
//!
//! The Rust side never receives browser cookies, bearer/session tokens, request headers, Sentinel
//! material, browser storage, or account identifiers. It exposes only typed account-history
//! commands: authentication probing, bounded history discovery/bootstrap, and exact C02 capture.

use crate::diagnostics;
use chatarium_core::authenticated_session::{
    AuthenticatedSessionLease, SessionAuthenticationEvidence, SessionLeaseError,
    UserAuthenticatedSessionProvider,
};
use chatarium_core::remote::{ProtocolObservationRevision, RemoteConversationId};
use chatarium_protocol::conversation_fetch_request::{
    CONVERSATION_FETCH_REQUEST_OBSERVATION, conversation_fetch_resource,
};
use chatarium_protocol::conversation_list::{
    CONVERSATION_LIST_FIRST_PAGE_RESOURCE, CONVERSATION_LIST_OBSERVATION, ConversationListItem,
    ConversationListPage, parse_conversation_list_first_page,
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

mod parsing;
mod provider;
mod server;
pub(super) use parsing::*;
pub(super) use server::*;
#[cfg(test)]
mod tests;

const DEFAULT_PORT: u16 = 43_117;
const BRIDGE_HEADER: &str = "x-chatarium-bridge";
const BRIDGE_HEADER_VALUE: &str = "edge-mv3-v1";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESULT_BODY_BYTES: usize = 6 * 1024 * 1024;
const NEXT_WAIT: Duration = Duration::from_secs(25);
const AUTH_RESULT_WAIT: Duration = Duration::from_secs(12);
const FETCH_RESULT_WAIT: Duration = Duration::from_secs(45);
const AUTH_REQUEST_PROFILE: &str = "chatgpt-me-v1";
const HISTORY_DISCOVERY_PROFILE: &str = "cdp-history-discovery-v1";
const FRESH_HISTORY_DISCOVERY_PROFILE: &str = "cdp-history-fresh-tab-v1";
const DISCOVERY_RESULT_WAIT: Duration = Duration::from_secs(30);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserBridgeError {
    Unavailable(String),
    Timeout,
    Protocol(String),
    UnsupportedRevision(String),
    HttpStatus(u16),
    RateLimited(String),
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
                write!(
                    formatter,
                    "browser-backed ChatGPT request returned HTTP {status}"
                )
            }
            Self::RateLimited(detail) => write!(
                formatter,
                "browser-backed ChatGPT request returned HTTP 429 ({detail})"
            ),
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
        diagnostics::info("bridge", format!("loopback listener ready on {address}"));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeTransport {
    Extension,
    ExtensionCdp,
}

impl BridgeTransport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::ExtensionCdp => "extension-cdp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserProof {
    pub extension_version: String,
    pub desktop_roundtrip: bool,
    pub chatgpt_tab_found: bool,
    pub main_world_execution: bool,
    pub debugger_attached: bool,
    pub network_enabled: bool,
    pub capture_tab_created: bool,
    pub navigation_started: bool,
    pub exact_response_seen: bool,
    pub account_context: bool,
    pub request_context_observed: bool,
    pub first_party_http_status: Option<u16>,
    pub context_header_count: u64,
    pub request_profile: String,
}

#[derive(Debug, Clone)]
pub struct AuthenticationObservation {
    pub evidence: SessionAuthenticationEvidence,
    pub proof: BrowserProof,
    pub http_status: u16,
    pub security_challenge: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationListObservation {
    pub page: ConversationListPage,
    pub proof: BrowserProof,
    pub http_status: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationFetchObservation {
    pub body: Value,
    pub proof: BrowserProof,
    pub http_status: u16,
    pub content_type: String,
    pub responses_seen: u64,
    pub exact_response_count: u64,
    pub rate_limited_responses: u64,
    pub rate_limit_reload_count: u64,
    pub body_read_failures: u64,
    pub body_too_large: u64,
    pub invalid_json: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistorySurfaceCandidate {
    pub path: String,
    pub query_keys: Vec<String>,
    pub surface_kind: String,
    pub conversation_count: u64,
    pub cursor_count: u64,
    pub top_level_cursor: String,
    pub traversal_truncated: bool,
    pub observations: u64,
    pub items: Vec<ConversationListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDiscoveryProof {
    pub extension_version: String,
    pub desktop_roundtrip: bool,
    pub chatgpt_tab_found: bool,
    pub debugger_attached: bool,
    pub network_enabled: bool,
    pub reload_started: bool,
    pub account_context: bool,
    pub responses_seen: u64,
    pub backend_http_200_seen: u64,
    pub json_candidates_seen: u64,
    pub body_read_failures: u64,
    pub body_too_large: u64,
    pub invalid_json: u64,
    pub request_profile: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryDiscoveryObservation {
    pub discovery: String,
    pub proof: HistoryDiscoveryProof,
    pub candidates: Vec<HistorySurfaceCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshTabHistoryDiscoveryProof {
    pub extension_version: String,
    pub desktop_roundtrip: bool,
    pub chatgpt_tab_found: bool,
    pub capture_tab_created: bool,
    pub debugger_attached: bool,
    pub network_enabled: bool,
    pub navigation_started: bool,
    pub account_context: bool,
    pub responses_seen: u64,
    pub backend_http_200_seen: u64,
    pub json_candidates_seen: u64,
    pub body_read_failures: u64,
    pub body_too_large: u64,
    pub invalid_json: u64,
    pub request_profile: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FreshTabHistoryDiscoveryObservation {
    pub discovery: String,
    pub proof: FreshTabHistoryDiscoveryProof,
    pub candidates: Vec<HistorySurfaceCandidate>,
}

#[derive(Clone)]
pub struct BrowserBridgeProvider {
    shared: Arc<Shared>,
}

