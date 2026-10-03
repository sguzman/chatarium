//! Loopback transport for credential-contained reads from the user's authenticated chatgpt.com tab.
//!
//! The Rust side never receives browser cookies, bearer/session tokens, request headers, Sentinel
//! material, browser storage, or account identifiers. It exposes only typed account-history
//! commands: authentication probing, bounded history discovery/bootstrap, and exact C02 capture.

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

const DEFAULT_PORT: u16 = 43_117;
const BRIDGE_HEADER: &str = "x-chatarium-bridge";
const BRIDGE_HEADER_VALUE: &str = "edge-mv3-v1";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESULT_BODY_BYTES: usize = 6 * 1024 * 1024;
const NEXT_WAIT: Duration = Duration::from_secs(25);
const AUTH_RESULT_WAIT: Duration = Duration::from_secs(5);
const FETCH_RESULT_WAIT: Duration = Duration::from_secs(45);
const AUTH_REQUEST_PROFILE: &str = "chatgpt-me-v1";
const HISTORY_DISCOVERY_PROFILE: &str = "cdp-history-discovery-v1";
const DISCOVERY_RESULT_WAIT: Duration = Duration::from_secs(30);
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
                write!(
                    formatter,
                    "browser-backed ChatGPT request returned HTTP {status}"
                )
            }
            Self::RateLimited => write!(
                formatter,
                "browser-backed ChatGPT request returned HTTP 429"
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
    pub application_context_header_count: u64,
    pub cache_disabled: bool,
    pub ui_stimulus_attempted: bool,
    pub ui_stimulus_attempts: u64,
    pub ui_stimulus_targets: u64,
    pub ui_stimulus_steps: u64,
    pub ui_stimulus_chat_links_before: u64,
    pub ui_stimulus_chat_links_after: u64,
    pub ui_stimulus_error: Option<String>,
    pub request_profile: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryDiscoveryObservation {
    pub discovery: String,
    pub proof: HistoryDiscoveryProof,
    pub candidates: Vec<HistorySurfaceCandidate>,
}

#[derive(Clone)]
pub struct BrowserBridgeProvider {
    shared: Arc<Shared>,
}

impl BrowserBridgeProvider {
    /// Probe the instrumented extension against an authenticated ChatGPT browser session.
    pub fn probe_authentication(
        &mut self,
    ) -> Result<AuthenticationObservation, BrowserBridgeError> {
        self.probe_authentication_observation()
    }

    /// Discover the current successful first-party history/list surfaces through a bounded CDP
    /// observation window. Candidate observation is not proof of complete account enumeration.
    pub fn discover_history_surfaces(
        &mut self,
    ) -> Result<HistoryDiscoveryObservation, BrowserBridgeError> {
        let result = self.call(
            "discover_history_surfaces",
            |object| {
                object.insert(
                    "request_profile".to_owned(),
                    json!(HISTORY_DISCOVERY_PROFILE),
                );
            },
            DISCOVERY_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }
        if parse_bridge_transport(&result)? != BridgeTransport::ExtensionCdp {
            return Err(BrowserBridgeError::Protocol(
                "history discovery did not use the CDP extension transport".to_owned(),
            ));
        }

        let request_profile = result
            .get("request_profile")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing request profile".to_owned(),
                )
            })?;
        if request_profile != HISTORY_DISCOVERY_PROFILE {
            return Err(BrowserBridgeError::Protocol(format!(
                "history discovery profile {request_profile:?} does not match expected {HISTORY_DISCOVERY_PROFILE:?}"
            )));
        }

        let extension_version = result
            .get("extension_version")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 64)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing extension version".to_owned(),
                )
            })?
            .to_owned();
        let required_bool = |field: &str| {
            result.get(field).and_then(Value::as_bool).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery result is missing boolean proof field {field:?}"
                ))
            })
        };
        let required_u64 = |field: &str| {
            result.get(field).and_then(Value::as_u64).ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery result is missing numeric proof field {field:?}"
                ))
            })
        };

        let proof = HistoryDiscoveryProof {
            extension_version,
            desktop_roundtrip: true,
            chatgpt_tab_found: required_bool("chatgpt_tab_found")?,
            debugger_attached: required_bool("debugger_attached")?,
            network_enabled: required_bool("network_enabled")?,
            reload_started: required_bool("reload_started")?,
            account_context: required_bool("account_context")?,
            responses_seen: required_u64("responses_seen")?,
            backend_http_200_seen: required_u64("backend_http_200_seen")?,
            json_candidates_seen: required_u64("json_candidates_seen")?,
            body_read_failures: required_u64("body_read_failures")?,
            body_too_large: required_u64("body_too_large")?,
            invalid_json: required_u64("invalid_json")?,
            application_context_header_count: required_u64("application_context_header_count")?,
            cache_disabled: required_bool("cache_disabled")?,
            ui_stimulus_attempted: required_bool("ui_stimulus_attempted")?,
            ui_stimulus_attempts: required_u64("ui_stimulus_attempts")?,
            ui_stimulus_targets: required_u64("ui_stimulus_targets")?,
            ui_stimulus_steps: required_u64("ui_stimulus_steps")?,
            ui_stimulus_chat_links_before: required_u64("ui_stimulus_chat_links_before")?,
            ui_stimulus_chat_links_after: required_u64("ui_stimulus_chat_links_after")?,
            ui_stimulus_error: result
                .get("ui_stimulus_error")
                .and_then(Value::as_str)
                .map(str::to_owned),
            request_profile: request_profile.to_owned(),
        };
        if !proof.chatgpt_tab_found
            || !proof.debugger_attached
            || !proof.network_enabled
            || !proof.reload_started
            || proof.responses_seen == 0
        {
            return Err(BrowserBridgeError::Protocol(
                "history discovery returned success without complete CDP boundary proof".to_owned(),
            ));
        }

        let candidates = result
            .get("candidates")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing candidates".to_owned(),
                )
            })?
            .iter()
            .enumerate()
            .map(|(index, value)| parse_history_surface_candidate(value, index))
            .collect::<Result<Vec<_>, _>>()?;

        let discovery = result
            .get("discovery")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(
                    "history discovery result is missing semantic state".to_owned(),
                )
            })?
            .to_owned();

        Ok(HistoryDiscoveryObservation {
            discovery,
            proof,
            candidates,
        })
    }

    /// Historical exact-C01 replay is retained only for forensic compatibility and must not be
    /// used by the desktop history-discovery flow.
    pub fn list_recent_conversations(
        &mut self,
    ) -> Result<ConversationListObservation, BrowserBridgeError> {
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| provider.fetch_history_first_page())
            .map_err(map_session_lease_error)?
    }

    fn fetch_history_first_page(&self) -> Result<ConversationListObservation, BrowserBridgeError> {
        let result = self.call(
            "list_conversations",
            |object| {
                object.insert(
                    "resource".to_owned(),
                    json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE),
                );
                object.insert(
                    "request_profile".to_owned(),
                    json!(CONVERSATION_LIST_OBSERVATION),
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
        let proof = parse_extension_proof(&result, CONVERSATION_LIST_OBSERVATION, true, true)?;

        let body = result.get("body").ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "successful conversation-list result is missing body".to_owned(),
            )
        })?;
        let page = parse_conversation_list_first_page(body)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        Ok(ConversationListObservation {
            page,
            proof,
            http_status: status,
        })
    }

    /// Fetch one exact existing conversation using only the live browser-held session.
    ///
    /// Reusable authentication material never crosses this boundary. Authentication is probed
    /// and revalidated through the same browser bridge immediately before the C02 read.
    pub fn fetch_authenticated_conversation(
        &mut self,
        remote_conversation_id: &str,
    ) -> Result<ConversationFetchObservation, BrowserBridgeError> {
        let remote_conversation_id = RemoteConversationId::new(remote_conversation_id)
            .map_err(|error| BrowserBridgeError::Protocol(error.to_string()))?;
        let protocol_revision =
            ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION)
                .expect("hard-coded C02 observation revision is non-empty");
        let mut lease =
            AuthenticatedSessionLease::acquire(self).map_err(map_session_lease_error)?;
        lease
            .with_authenticated_provider(|provider| {
                provider.fetch_conversation_observation(&remote_conversation_id, &protocol_revision)
            })
            .map_err(map_session_lease_error)?
    }

    fn probe_authentication_observation(
        &self,
    ) -> Result<AuthenticationObservation, BrowserBridgeError> {
        let result = self.call(
            "probe_auth",
            |object| {
                object.insert("request_profile".to_owned(), json!(AUTH_REQUEST_PROFILE));
            },
            AUTH_RESULT_WAIT,
        )?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }
        let http_status = required_http_status(&result, "authentication")?;
        let evidence = match result.get("authentication").and_then(Value::as_str) {
            Some("authenticated") => SessionAuthenticationEvidence::Authenticated,
            Some("unauthenticated") => SessionAuthenticationEvidence::Unauthenticated,
            Some("unknown") => SessionAuthenticationEvidence::Unknown,
            _ => {
                return Err(BrowserBridgeError::Protocol(
                    "probe_auth result has no supported authentication state".to_owned(),
                ));
            }
        };
        let proof = parse_extension_proof(&result, AUTH_REQUEST_PROFILE, false, false)?;
        Ok(AuthenticationObservation {
            evidence,
            proof,
            http_status,
        })
    }

    fn fetch_conversation_observation(
        &self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<ConversationFetchObservation, BrowserBridgeError> {
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
                object.insert(
                    "request_profile".to_owned(),
                    json!(protocol_revision.as_str()),
                );
            },
            FETCH_RESULT_WAIT,
        )?;

        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(remote_result_error(&result));
        }

        let http_status = required_http_status(&result, "conversation fetch")?;
        if http_status != 200 {
            return Err(status_error(http_status));
        }
        let proof = parse_extension_proof(&result, protocol_revision.as_str(), true, false)?;
        if !proof.exact_response_seen {
            return Err(BrowserBridgeError::Protocol(
                "conversation mirror result did not prove the exact first-party response was observed"
                    .to_owned(),
            ));
        }
        let body = result.get("body").cloned().ok_or_else(|| {
            BrowserBridgeError::Protocol("successful fetch result is missing body".to_owned())
        })?;

        Ok(ConversationFetchObservation {
            body,
            proof,
            http_status,
        })
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
        self.probe_authentication_observation()
            .map(|observation| observation.evidence)
    }
}

impl RemoteConversationFetchProvider for BrowserBridgeProvider {
    type FetchError = BrowserBridgeError;

    fn fetch_conversation(
        &mut self,
        remote_conversation_id: &RemoteConversationId,
        protocol_revision: &ProtocolObservationRevision,
    ) -> Result<Value, Self::FetchError> {
        self.fetch_conversation_observation(remote_conversation_id, protocol_revision)
            .map(|observation| observation.body)
    }
}

fn parse_history_surface_candidate(
    value: &Value,
    index: usize,
) -> Result<HistorySurfaceCandidate, BrowserBridgeError> {
    let object = value.as_object().ok_or_else(|| {
        BrowserBridgeError::Protocol(format!(
            "history discovery candidate {index} is not an object"
        ))
    })?;
    let string_field = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 4096)
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} has invalid {field:?}"
                ))
            })
    };
    let u64_field = |field: &str| {
        object.get(field).and_then(Value::as_u64).ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} has invalid {field:?}"
            ))
        })
    };

    let query_keys = object
        .get("query_keys")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} is missing query keys"
            ))
        })?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .map(str::to_owned)
                .ok_or_else(|| {
                    BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} has an invalid query key"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let items = object
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "history discovery candidate {index} is missing items"
            ))
        })?
        .iter()
        .enumerate()
        .map(|(item_index, item)| {
            let item = item.as_object().ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} item {item_index} is not an object"
                ))
            })?;
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .ok_or_else(|| {
                    BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} item {item_index} has invalid id"
                    ))
                })?
                .to_owned();
            let title = match item.get("title") {
                None | Some(Value::Null) => None,
                Some(Value::String(title)) if title.len() <= 4096 => Some(title.clone()),
                _ => {
                    return Err(BrowserBridgeError::Protocol(format!(
                        "history discovery candidate {index} item {item_index} has invalid title"
                    )));
                }
            };
            Ok(ConversationListItem {
                id,
                title,
                create_time: item.get("create_time").cloned(),
                update_time: item.get("update_time").cloned(),
            })
        })
        .collect::<Result<Vec<_>, BrowserBridgeError>>()?;

    Ok(HistorySurfaceCandidate {
        path: string_field("path")?,
        query_keys,
        surface_kind: string_field("surface_kind")?,
        conversation_count: u64_field("conversation_count")?,
        cursor_count: u64_field("cursor_count")?,
        top_level_cursor: string_field("top_level_cursor")?,
        traversal_truncated: object
            .get("traversal_truncated")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                BrowserBridgeError::Protocol(format!(
                    "history discovery candidate {index} is missing traversal_truncated"
                ))
            })?,
        observations: u64_field("observations")?,
        items,
    })
}

fn required_http_status(result: &Value, operation: &str) -> Result<u16, BrowserBridgeError> {
    result
        .get("http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(format!(
                "successful {operation} result is missing HTTP status"
            ))
        })
}

fn parse_extension_proof(
    result: &Value,
    expected_profile: &str,
    require_account_context: bool,
    require_request_context: bool,
) -> Result<BrowserProof, BrowserBridgeError> {
    let transport = parse_bridge_transport(result)?;
    if !matches!(
        transport,
        BridgeTransport::Extension | BridgeTransport::ExtensionCdp
    ) {
        return Err(BrowserBridgeError::Protocol(
            "critical history result did not use an Edge extension transport".to_owned(),
        ));
    }

    let extension_version = result
        .get("extension_version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "extension result is missing a usable extension version".to_owned(),
            )
        })?
        .to_owned();
    let chatgpt_tab_found = result
        .get("chatgpt_tab_found")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let main_world_execution = result
        .get("main_world_execution")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let debugger_attached = result
        .get("debugger_attached")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let network_enabled = result
        .get("network_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let capture_tab_created = result
        .get("capture_tab_created")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let navigation_started = result
        .get("navigation_started")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exact_response_seen = result
        .get("exact_response_seen")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let account_context = result
        .get("account_context")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_context_observed = result
        .get("request_context_observed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let first_party_http_status = result
        .get("first_party_http_status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    let context_header_count = result
        .get("context_header_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let request_profile = result
        .get("request_profile")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BrowserBridgeError::Protocol(
                "extension result is missing request-profile evidence".to_owned(),
            )
        })?;
    if request_profile != expected_profile {
        return Err(BrowserBridgeError::Protocol(format!(
            "extension request profile {request_profile:?} does not match expected {expected_profile:?}"
        )));
    }
    if !chatgpt_tab_found {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove an exact ChatGPT tab was found".to_owned(),
        ));
    }
    let cdp_execution = debugger_attached && network_enabled && navigation_started;
    if !main_world_execution && !cdp_execution {
        return Err(BrowserBridgeError::Protocol(
            "extension result proves neither MAIN-world execution nor bounded CDP navigation"
                .to_owned(),
        ));
    }
    if require_account_context && !account_context {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove ChatGPT account context".to_owned(),
        ));
    }
    if require_request_context && !request_context_observed {
        return Err(BrowserBridgeError::Protocol(
            "extension result does not prove observed first-party request context".to_owned(),
        ));
    }
    if require_request_context && context_header_count == 0 {
        return Err(BrowserBridgeError::Protocol(
            "extension result observed no replayable first-party context headers".to_owned(),
        ));
    }

    Ok(BrowserProof {
        extension_version,
        desktop_roundtrip: true,
        chatgpt_tab_found,
        main_world_execution,
        debugger_attached,
        network_enabled,
        capture_tab_created,
        navigation_started,
        exact_response_seen,
        account_context,
        request_context_observed,
        first_party_http_status,
        context_header_count,
        request_profile: request_profile.to_owned(),
    })
}

fn parse_bridge_transport(result: &Value) -> Result<BridgeTransport, BrowserBridgeError> {
    match result.get("bridge_transport").and_then(Value::as_str) {
        Some("extension") => Ok(BridgeTransport::Extension),
        Some("extension-cdp") => Ok(BridgeTransport::ExtensionCdp),
        Some("page") | Some("tampermonkey") => Err(BrowserBridgeError::Protocol(
            "retired userscript transport is not accepted on the critical history path".to_owned(),
        )),
        Some(other) => Err(BrowserBridgeError::Protocol(format!(
            "unsupported bridge transport {other:?}"
        ))),
        None => Err(BrowserBridgeError::Protocol(
            "bridge result is missing transport evidence".to_owned(),
        )),
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
    let reason = result
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("remote fetch failed without structured reason");
    let transport = result
        .get("bridge_transport")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let extension_version = result
        .get("extension_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let tab = proof_bool(result, "chatgpt_tab_found");
    let main_world = proof_bool(result, "main_world_execution");
    let debugger = proof_bool(result, "debugger_attached");
    let network = proof_bool(result, "network_enabled");
    let capture_tab = proof_bool(result, "capture_tab_created");
    let navigation = proof_bool(result, "navigation_started");
    let exact_response = proof_bool(result, "exact_response_seen");
    let account_context = proof_bool(result, "account_context");
    let request_context = proof_bool(result, "request_context_observed");
    let first_party_http = result
        .get("first_party_http_status")
        .and_then(Value::as_u64)
        .map(|status| status.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let context_header_count = result
        .get("context_header_count")
        .and_then(Value::as_u64)
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let request_profile = result
        .get("request_profile")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    BrowserBridgeError::Protocol(format!(
        "{reason}; transport={transport}; extension={extension_version}; tab={tab}; MAIN={main_world}; debugger={debugger}; network={network}; capture-tab={capture_tab}; navigation={navigation}; exact-response={exact_response}; account-context={account_context}; request-context={request_context}; first-party-http={first_party_http}; context-headers={context_header_count}; profile={request_profile}"
    ))
}

fn proof_bool(result: &Value, field: &str) -> &'static str {
    match result.get(field).and_then(Value::as_bool) {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
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

    if request.method == "OPTIONS" {
        return write_response(&mut stream, 403, None);
    }

    if request
        .headers
        .get("origin")
        .is_some_and(|origin| !is_extension_origin(origin))
    {
        return write_response(&mut stream, 403, None);
    }

    if request.headers.get(BRIDGE_HEADER).map(String::as_str) != Some(BRIDGE_HEADER_VALUE) {
        return write_response(&mut stream, 403, None);
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/next") => handle_next(&mut stream, shared),
        ("POST", "/v1/result") => handle_result(&mut stream, shared, &request.body),
        _ => write_response(&mut stream, 404, None),
    }
}

fn is_extension_origin(origin: &str) -> bool {
    let Some(extension_id) = origin
        .strip_prefix("chrome-extension://")
        .map(|value| value.trim_end_matches('/'))
    else {
        return false;
    };
    extension_id.len() == 32 && extension_id.bytes().all(|byte| matches!(byte, b'a'..=b'p'))
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
    write_response_with_extra_headers(stream, status, body, None)
}

fn write_response_with_extra_headers(
    stream: &mut TcpStream,
    status: u16,
    body: Option<(&str, &[u8])>,
    extra_headers: Option<&[(&str, &str)]>,
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
    if let Some(extra_headers) = extra_headers {
        for (name, value) in extra_headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
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
                b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
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
                "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let mut raw = request_head.into_bytes();
            raw.extend_from_slice(&body);
            let response = request(address, &raw);
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
        })
    }

    fn extension_auth_result(command: &Value) -> Value {
        json!({
            "version": 1,
            "id": command["id"],
            "kind": "probe_auth",
            "ok": true,
            "authentication": "authenticated",
            "http_status": 200,
            "bridge_transport": "extension",
            "extension_version": "0.2.0",
            "chatgpt_tab_found": true,
            "main_world_execution": true,
            "account_context": true,
            "request_context_observed": false,
            "first_party_http_status": null,
            "context_header_count": 0,
            "request_profile": AUTH_REQUEST_PROFILE
        })
    }

    fn exchange_auth(address: SocketAddr) {
        let raw = request(
            address,
            b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
        assert_eq!(command["kind"], json!("probe_auth"));
        assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
        let body = serde_json::to_vec(&extension_auth_result(&command)).unwrap();
        let request_head = format!(
            "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut raw = request_head.into_bytes();
        raw.extend_from_slice(&body);
        let response = request(address, &raw);
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
    }

    #[test]
    fn web_page_origins_cannot_reach_the_retired_direct_loopback_path() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        for origin in ["https://chatgpt.com", "https://example.com"] {
            let raw = format!(
                "GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {origin}\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n"
            );
            let response = request(runtime.address(), raw.as_bytes());
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
        }
    }

    #[test]
    fn chromium_extension_origin_can_reach_the_typed_loopback_surface() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: chrome-extension://abcdefghijklmnopabcdefghijklmnop\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn browser_preflight_is_rejected_instead_of_reviving_page_cors() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"OPTIONS /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://chatgpt.com\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: x-chatarium-bridge\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
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
    fn retired_userscript_marker_cannot_consume_extension_commands() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let response = request(
            runtime.address(),
            b"GET /unknown HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: 1\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    }

    #[test]
    fn auth_probe_crosses_only_typed_state_and_extension_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
                assert_eq!(command["request_profile"], json!(AUTH_REQUEST_PROFILE));
                assert_eq!(command.as_object().unwrap().len(), 4);
            },
            extension_auth_result,
        );

        let mut provider = runtime.provider();
        let observation = provider.probe_authentication().unwrap();
        assert_eq!(
            observation.evidence,
            SessionAuthenticationEvidence::Authenticated
        );
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.2.0");
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.chatgpt_tab_found);
        assert!(observation.proof.main_world_execution);
        assert!(observation.proof.account_context);
        assert!(!observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, None);
        assert_eq!(observation.proof.context_header_count, 0);
        assert_eq!(observation.proof.request_profile, AUTH_REQUEST_PROFILE);
        browser.join().unwrap();
    }

    #[test]
    fn cdp_history_discovery_crosses_only_typed_candidates_and_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("discover_history_surfaces"));
                assert_eq!(command["request_profile"], json!(HISTORY_DISCOVERY_PROFILE));
                assert_eq!(command.as_object().unwrap().len(), 4);
            },
            |command| {
                let candidate = json!({
                    "path": "/backend-api/gizmos/snorlax/sidebar",
                    "query_keys": [
                        "conversations_per_gizmo",
                        "limit",
                        "owned_only"
                    ],
                    "surface_kind": "snorlax_sidebar",
                    "conversation_count": 1,
                    "cursor_count": 2,
                    "string_cursor_count": 1,
                    "null_cursor_count": 1,
                    "top_level_cursor": "string",
                    "top_level_keys": ["cursor", "items"],
                    "traversal_truncated": false,
                    "observations": 1,
                    "items": [{
                        "id": "remote-1",
                        "title": "Observed live",
                        "create_time": "2026-10-03T00:00:00Z",
                        "update_time": "2026-10-03T00:01:00Z"
                    }]
                });
                let mut result = json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "discover_history_surfaces",
                    "ok": true,
                    "bridge_transport": "extension-cdp",
                    "extension_version": "0.4.4",
                    "chatgpt_tab_found": true,
                    "main_world_execution": false,
                    "account_context": true,
                    "debugger_attached": true,
                    "network_enabled": true,
                    "reload_started": true,
                    "request_profile": HISTORY_DISCOVERY_PROFILE,
                    "responses_seen": 37,
                    "backend_http_200_seen": 12,
                    "json_candidates_seen": 3,
                    "body_read_failures": 0,
                    "body_too_large": 0,
                    "invalid_json": 0,
                    "application_context_header_count": 6,
                    "cache_disabled": true,
                    "ui_stimulus_attempted": true,
                    "ui_stimulus_attempts": 2,
                    "ui_stimulus_targets": 1,
                    "ui_stimulus_steps": 12,
                    "ui_stimulus_chat_links_before": 8,
                    "ui_stimulus_chat_links_after": 20,
                    "ui_stimulus_error": null,
                    "candidate_count": 1,
                    "discovery": "candidates_observed"
                });
                result["candidates"] = json!([candidate]);
                result
            },
        );

        let mut provider = runtime.provider();
        let observation = provider.discover_history_surfaces().unwrap();
        assert_eq!(observation.discovery, "candidates_observed");
        assert_eq!(observation.proof.extension_version, "0.4.4");
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.debugger_attached);
        assert!(observation.proof.network_enabled);
        assert!(observation.proof.reload_started);
        assert_eq!(observation.proof.responses_seen, 37);
        assert_eq!(observation.proof.application_context_header_count, 6);
        assert!(observation.proof.cache_disabled);
        assert!(observation.proof.ui_stimulus_attempted);
        assert_eq!(observation.proof.ui_stimulus_attempts, 2);
        assert_eq!(observation.proof.ui_stimulus_targets, 1);
        assert_eq!(observation.proof.ui_stimulus_steps, 12);
        assert_eq!(observation.proof.ui_stimulus_chat_links_before, 8);
        assert_eq!(observation.proof.ui_stimulus_chat_links_after, 20);
        assert_eq!(observation.proof.ui_stimulus_error, None);
        assert_eq!(observation.candidates.len(), 1);
        assert_eq!(
            observation.candidates[0].path,
            "/backend-api/gizmos/snorlax/sidebar"
        );
        assert_eq!(observation.candidates[0].items.len(), 1);
        assert_eq!(observation.candidates[0].items[0].id, "remote-1");
        browser.join().unwrap();
    }

    #[test]
    fn c01_command_uses_exact_first_page_resource_and_parses_extension_proof() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            for expected_kind in ["probe_auth", "probe_auth", "list_conversations"] {
                if expected_kind == "probe_auth" {
                    exchange_auth(address);
                    continue;
                }

                let raw = request(
                    address,
                    b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
                );
                let split = raw
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap();
                let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
                assert_eq!(command["kind"], json!(expected_kind));
                assert_eq!(
                    command["resource"],
                    json!(CONVERSATION_LIST_FIRST_PAGE_RESOURCE)
                );
                assert_eq!(
                    command["request_profile"],
                    json!(CONVERSATION_LIST_OBSERVATION)
                );
                assert_eq!(command.as_object().unwrap().len(), 5);

                let result = json!({
                    "version": 1,
                    "id": command["id"],
                    "kind": "list_conversations",
                    "ok": true,
                    "http_status": 200,
                    "content_type": "application/json",
                    "bridge_transport": "extension",
                    "extension_version": "0.2.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": true,
                    "account_context": true,
                    "request_context_observed": true,
                    "first_party_http_status": 200,
                    "context_header_count": 7,
                    "request_profile": CONVERSATION_LIST_OBSERVATION,
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
                });
                let body = serde_json::to_vec(&result).unwrap();
                let request_head = format!(
                    "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let mut raw = request_head.into_bytes();
                raw.extend_from_slice(&body);
                let response = request(address, &raw);
                assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
            }
        });

        let mut provider = runtime.provider();
        let observation = provider.list_recent_conversations().unwrap();
        assert_eq!(observation.page.items.len(), 1);
        assert_eq!(observation.page.items[0].id, "remote-1");
        assert_eq!(observation.page.total, 21);
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.2.0");
        assert_eq!(
            observation.proof.request_profile,
            CONVERSATION_LIST_OBSERVATION
        );
        assert!(observation.proof.desktop_roundtrip);
        assert!(observation.proof.chatgpt_tab_found);
        assert!(observation.proof.main_world_execution);
        assert!(observation.proof.account_context);
        assert!(observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, Some(200));
        assert_eq!(observation.proof.context_header_count, 7);
        browser.join().unwrap();
    }

    #[test]
    fn c01_rejects_false_success_without_account_context() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();

        let browser = thread::spawn(move || {
            exchange_auth(address);
            exchange_auth(address);

            let raw = request(
                address,
                b"GET /v1/next HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\n\r\n",
            );
            let split = raw
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let command: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
            let result = json!({
                "version": 1,
                "id": command["id"],
                "kind": "list_conversations",
                "ok": true,
                "http_status": 200,
                "content_type": "application/json",
                "bridge_transport": "extension",
                "extension_version": "0.2.0",
                "chatgpt_tab_found": true,
                "main_world_execution": true,
                "account_context": false,
                "request_context_observed": true,
                "first_party_http_status": 200,
                "context_header_count": 7,
                "request_profile": CONVERSATION_LIST_OBSERVATION,
                "body": {
                    "items": [],
                    "total": 0,
                    "limit": 20,
                    "offset": 0
                }
            });
            let body = serde_json::to_vec(&result).unwrap();
            let request_head = format!(
                "POST /v1/result HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Chatarium-Bridge: edge-mv3-v1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let mut raw = request_head.into_bytes();
            raw.extend_from_slice(&body);
            let response = request(address, &raw);
            assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 204"));
        });

        let mut provider = runtime.provider();
        let error = provider.list_recent_conversations().unwrap_err();
        assert!(error.to_string().contains("account context"));
        browser.join().unwrap();
    }

    #[test]
    fn retired_userscript_transport_is_rejected_even_with_success_shape() {
        let runtime = AccountBridgeRuntime::start_on("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = runtime.address();
        let browser = browser_exchange(
            address,
            |command| {
                assert_eq!(command["kind"], json!("probe_auth"));
            },
            |command| {
                let mut result = extension_auth_result(command);
                result["bridge_transport"] = json!("page");
                result
            },
        );

        let mut provider = runtime.provider();
        let error = provider.probe_authentication().unwrap_err();
        assert!(error.to_string().contains("retired userscript transport"));
        browser.join().unwrap();
    }

    #[test]
    fn c02_command_uses_exact_evidence_backed_resource_and_extension_proof() {
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
                assert_eq!(
                    command["request_profile"],
                    json!(CONVERSATION_FETCH_REQUEST_OBSERVATION)
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
                    "bridge_transport": "extension-cdp",
                    "extension_version": "0.4.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": false,
                    "debugger_attached": true,
                    "network_enabled": true,
                    "capture_tab_created": true,
                    "navigation_started": true,
                    "exact_response_seen": true,
                    "account_context": true,
                    "request_context_observed": false,
                    "first_party_http_status": 200,
                    "context_header_count": 0,
                    "request_profile": CONVERSATION_FETCH_REQUEST_OBSERVATION,
                    "body": {"conversation_id": "opaque/remote id"}
                })
            },
        );

        let mut provider = runtime.provider();
        let observation = provider
            .fetch_conversation_observation(
                &RemoteConversationId::new("opaque/remote id").unwrap(),
                &ProtocolObservationRevision::new(CONVERSATION_FETCH_REQUEST_OBSERVATION).unwrap(),
            )
            .unwrap();
        assert_eq!(
            observation.body["conversation_id"],
            json!("opaque/remote id")
        );
        assert_eq!(observation.http_status, 200);
        assert_eq!(observation.proof.extension_version, "0.4.0");
        assert_eq!(
            observation.proof.request_profile,
            CONVERSATION_FETCH_REQUEST_OBSERVATION
        );
        assert!(observation.proof.debugger_attached);
        assert!(observation.proof.network_enabled);
        assert!(observation.proof.capture_tab_created);
        assert!(observation.proof.navigation_started);
        assert!(observation.proof.exact_response_seen);
        assert!(!observation.proof.request_context_observed);
        assert_eq!(observation.proof.first_party_http_status, Some(200));
        assert_eq!(observation.proof.context_header_count, 0);
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
                    "bridge_transport": "extension",
                    "extension_version": "0.2.0",
                    "chatgpt_tab_found": true,
                    "main_world_execution": true,
                    "account_context": true,
                    "request_profile": CONVERSATION_FETCH_REQUEST_OBSERVATION,
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
