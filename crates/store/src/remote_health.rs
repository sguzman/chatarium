//! Conservative, durable remote-health gating for the production mirror controller.
//!
//! Health is deliberately separate from per-conversation queue state. A pending queue remains
//! pending while the remote service is rate limited, challenged, or unavailable. The journal
//! records only structural observations; no response body, cookie, header, or token is stored.

use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use serde_json::{Value, json};

const SCHEMA: &str = "chatarium-remote-health";
const VERSION: u64 = 1;

/// Long, deterministic cooldowns. Expiry enables a health check; it never starts capture.
pub const RATE_LIMIT_COOLDOWN_MS: u64 = 30 * 60 * 1000;
pub const CHALLENGE_COOLDOWN_MS: u64 = 60 * 60 * 1000;
pub const NETWORK_COOLDOWN_MS: u64 = 10 * 60 * 1000;
pub const BACKEND_COOLDOWN_MS: u64 = 30 * 60 * 1000;
pub const AUTH_COOLDOWN_MS: u64 = 60 * 60 * 1000;

/// Controller-level health classification, independent of queue item status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteHealthState {
    Unknown,
    Healthy,
    RateLimited,
    ServerChallenge,
    AuthenticationRequired,
    NetworkUnstable,
    BackendUnavailable,
    ManuallyPaused,
    EligibleForRecheck,
}

impl RemoteHealthState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Healthy => "HEALTHY",
            Self::RateLimited => "RATE LIMITED",
            Self::ServerChallenge => "SERVER CHALLENGE",
            Self::AuthenticationRequired => "AUTHENTICATION REQUIRED",
            Self::NetworkUnstable => "NETWORK UNSTABLE",
            Self::BackendUnavailable => "BACKEND UNAVAILABLE",
            Self::ManuallyPaused => "MANUALLY PAUSED",
            Self::EligibleForRecheck => "ELIGIBLE FOR RECHECK",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "UNKNOWN" => Self::Unknown,
            "HEALTHY" => Self::Healthy,
            "RATE LIMITED" => Self::RateLimited,
            "SERVER CHALLENGE" => Self::ServerChallenge,
            "AUTHENTICATION REQUIRED" => Self::AuthenticationRequired,
            "NETWORK UNSTABLE" => Self::NetworkUnstable,
            "BACKEND UNAVAILABLE" => Self::BackendUnavailable,
            "MANUALLY PAUSED" => Self::ManuallyPaused,
            "ELIGIBLE FOR RECHECK" => Self::EligibleForRecheck,
            _ => return None,
        })
    }
}

/// High-level principal intent. This is not a health result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorIntent {
    Stopped,
    Enabled,
    ManuallyPaused,
}

impl MirrorIntent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "STOPPED",
            Self::Enabled => "ENABLED",
            Self::ManuallyPaused => "MANUALLY PAUSED",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "STOPPED" => Self::Stopped,
            "ENABLED" => Self::Enabled,
            "MANUALLY PAUSED" => Self::ManuallyPaused,
            _ => return None,
        })
    }
}

/// Synthetic or provider-derived structural health observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteHealthSignal {
    Unknown,
    Healthy { http_status: u16 },
    RateLimited { http_status: u16 },
    ServerChallenge { http_status: u16 },
    AuthenticationRequired,
    NetworkUnstable,
    BackendUnavailable { http_status: u16 },
}

/// Durable controller-level state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHealthController {
    pub intent: MirrorIntent,
    pub state: RemoteHealthState,
    pub observed_at_ms: u64,
    pub cooldown_until_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub last_http_status: Option<u16>,
    pub challenge: bool,
}

impl Default for RemoteHealthController {
    fn default() -> Self {
        Self {
            intent: MirrorIntent::Stopped,
            state: RemoteHealthState::Unknown,
            observed_at_ms: 0,
            cooldown_until_ms: None,
            consecutive_failures: 0,
            last_http_status: None,
            challenge: false,
        }
    }
}

impl RemoteHealthController {
    /// Rehydrate the latest health event. Expired cooldowns become eligible only; no I/O occurs.
    pub fn from_events(events: &[EventEnvelope], now_ms: u64) -> Result<Self, String> {
        let mut controller = Self::default();
        for event in events
            .iter()
            .filter(|event| event.kind == EventKind::RemoteHealthObserved)
        {
            controller.apply_payload(&event.payload)?;
        }
        controller.advance(now_ms);
        Ok(controller)
    }

    /// Set principal intent locally. It does not perform a health check or capture.
    pub fn set_intent(&mut self, intent: MirrorIntent) {
        self.intent = intent;
        if intent == MirrorIntent::ManuallyPaused {
            self.state = RemoteHealthState::ManuallyPaused;
            self.cooldown_until_ms = None;
        } else if intent == MirrorIntent::Enabled && self.state == RemoteHealthState::ManuallyPaused
        {
            self.state = RemoteHealthState::EligibleForRecheck;
        } else if intent == MirrorIntent::Stopped && self.state == RemoteHealthState::ManuallyPaused
        {
            self.state = RemoteHealthState::Unknown;
        }
    }

    /// Advance time without appending an event or starting work.
    pub fn advance(&mut self, now_ms: u64) {
        if self.cooldown_until_ms.is_some_and(|until| now_ms >= until)
            && matches!(
                self.state,
                RemoteHealthState::RateLimited
                    | RemoteHealthState::ServerChallenge
                    | RemoteHealthState::NetworkUnstable
                    | RemoteHealthState::BackendUnavailable
                    | RemoteHealthState::AuthenticationRequired
            )
        {
            self.cooldown_until_ms = None;
            self.state = RemoteHealthState::EligibleForRecheck;
        }
    }

    #[must_use]
    pub fn cooldown_active(&self, now_ms: u64) -> bool {
        self.cooldown_until_ms.is_some_and(|until| until > now_ms)
    }

    #[must_use]
    pub fn retry_eligible(&self, now_ms: u64) -> bool {
        !self.cooldown_active(now_ms)
            && matches!(
                self.state,
                RemoteHealthState::Unknown
                    | RemoteHealthState::EligibleForRecheck
                    | RemoteHealthState::Healthy
            )
            && self.intent == MirrorIntent::Enabled
    }

    #[must_use]
    pub fn capture_allowed(&self, now_ms: u64) -> bool {
        self.intent == MirrorIntent::Enabled
            && self.state == RemoteHealthState::Healthy
            && !self.cooldown_active(now_ms)
    }

    /// Apply one observation using deterministic cooldown policy.
    pub fn observe(&mut self, signal: RemoteHealthSignal, now_ms: u64) {
        let (state, status, challenge, cooldown) = match signal {
            RemoteHealthSignal::Unknown => (RemoteHealthState::Unknown, None, false, None),
            RemoteHealthSignal::Healthy { http_status } => {
                (RemoteHealthState::Healthy, Some(http_status), false, None)
            }
            RemoteHealthSignal::RateLimited { http_status } => (
                RemoteHealthState::RateLimited,
                Some(http_status),
                false,
                Some(RATE_LIMIT_COOLDOWN_MS),
            ),
            RemoteHealthSignal::ServerChallenge { http_status } => (
                RemoteHealthState::ServerChallenge,
                Some(http_status),
                true,
                Some(CHALLENGE_COOLDOWN_MS),
            ),
            RemoteHealthSignal::AuthenticationRequired => (
                RemoteHealthState::AuthenticationRequired,
                None,
                false,
                Some(AUTH_COOLDOWN_MS),
            ),
            RemoteHealthSignal::NetworkUnstable => (
                RemoteHealthState::NetworkUnstable,
                None,
                false,
                Some(NETWORK_COOLDOWN_MS),
            ),
            RemoteHealthSignal::BackendUnavailable { http_status } => (
                RemoteHealthState::BackendUnavailable,
                Some(http_status),
                false,
                Some(BACKEND_COOLDOWN_MS),
            ),
        };
        self.state = state;
        self.observed_at_ms = now_ms;
        self.last_http_status = status;
        self.challenge = challenge;
        self.cooldown_until_ms = cooldown.map(|duration| now_ms.saturating_add(duration));
        if matches!(
            state,
            RemoteHealthState::Healthy | RemoteHealthState::Unknown
        ) {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        }
    }

    fn apply_payload(&mut self, payload: &str) -> Result<(), String> {
        let value: Value =
            serde_json::from_str(payload).map_err(|e| format!("remote health JSON: {e}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err("unsupported remote health payload".to_owned());
        }
        self.state = RemoteHealthState::parse(
            value
                .get("state")
                .and_then(Value::as_str)
                .ok_or("remote health state missing")?,
        )
        .ok_or("unknown remote health state")?;
        self.intent = MirrorIntent::parse(
            value
                .get("intent")
                .and_then(Value::as_str)
                .ok_or("remote health intent missing")?,
        )
        .ok_or("unknown mirror intent")?;
        self.observed_at_ms = value
            .get("observed_at_ms")
            .and_then(Value::as_u64)
            .ok_or("remote health timestamp missing")?;
        self.cooldown_until_ms = value.get("cooldown_until_ms").and_then(Value::as_u64);
        self.consecutive_failures = value
            .get("consecutive_failures")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("remote health failure count missing")?;
        self.last_http_status = value
            .get("http_status")
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok());
        self.challenge = value
            .get("challenge")
            .and_then(Value::as_bool)
            .ok_or("remote health challenge flag missing")?;
        Ok(())
    }
}

/// Append a structural health observation to the authoritative journal.
pub fn record_remote_health_signal(
    store: &mut impl EventStore,
    controller: &mut RemoteHealthController,
    signal: RemoteHealthSignal,
    now_ms: u64,
) -> std::io::Result<u64> {
    controller.observe(signal, now_ms);
    record_controller_state(store, controller)
}

/// Append an explicit principal intent change without contacting the remote service.
pub fn record_remote_health_intent(
    store: &mut impl EventStore,
    controller: &mut RemoteHealthController,
    intent: MirrorIntent,
    now_ms: u64,
) -> std::io::Result<u64> {
    controller.set_intent(intent);
    controller.observed_at_ms = now_ms;
    record_controller_state(store, controller)
}

fn record_controller_state(
    store: &mut impl EventStore,
    controller: &RemoteHealthController,
) -> std::io::Result<u64> {
    let payload = serde_json::to_string(&json!({
        "schema": SCHEMA, "version": VERSION, "record": "remote_health_observed",
        "state": controller.state.as_str(), "intent": controller.intent.as_str(),
        "observed_at_ms": controller.observed_at_ms, "cooldown_until_ms": controller.cooldown_until_ms,
        "consecutive_failures": controller.consecutive_failures, "http_status": controller.last_http_status,
        "challenge": controller.challenge,
    })).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    store.append(EventKind::RemoteHealthObserved, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, MemoryEventStore};

    fn assert_cooldown(signal: RemoteHealthSignal, expected: RemoteHealthState, duration: u64) {
        let now = 1_000_000;
        let mut controller = RemoteHealthController::default();
        controller.set_intent(MirrorIntent::Enabled);
        controller.observe(signal, now);
        assert_eq!(controller.state, expected);
        assert_eq!(controller.cooldown_until_ms, Some(now + duration));
        assert!(!controller.capture_allowed(now));
        controller.advance(now + duration);
        assert_eq!(controller.state, RemoteHealthState::EligibleForRecheck);
        assert!(controller.retry_eligible(now + duration));
        assert!(!controller.capture_allowed(now + duration));
    }

    #[test]
    fn transition_table_and_expiry_never_start_capture() {
        let now = 1_000_000;
        let mut controller = RemoteHealthController::default();
        controller.set_intent(MirrorIntent::Enabled);
        controller.observe(RemoteHealthSignal::Healthy { http_status: 200 }, now);
        assert!(controller.capture_allowed(now));
        assert_cooldown(
            RemoteHealthSignal::RateLimited { http_status: 429 },
            RemoteHealthState::RateLimited,
            RATE_LIMIT_COOLDOWN_MS,
        );
        assert_cooldown(
            RemoteHealthSignal::ServerChallenge { http_status: 403 },
            RemoteHealthState::ServerChallenge,
            CHALLENGE_COOLDOWN_MS,
        );
        assert_cooldown(
            RemoteHealthSignal::NetworkUnstable,
            RemoteHealthState::NetworkUnstable,
            NETWORK_COOLDOWN_MS,
        );
        assert_cooldown(
            RemoteHealthSignal::BackendUnavailable { http_status: 503 },
            RemoteHealthState::BackendUnavailable,
            BACKEND_COOLDOWN_MS,
        );
        assert_cooldown(
            RemoteHealthSignal::AuthenticationRequired,
            RemoteHealthState::AuthenticationRequired,
            AUTH_COOLDOWN_MS,
        );
        controller.observe(RemoteHealthSignal::Unknown, now);
        assert_eq!(controller.state, RemoteHealthState::Unknown);
    }

    #[test]
    fn health_history_replays_and_keeps_queue_events_separate() {
        let mut store = MemoryEventStore::default();
        let mut controller = RemoteHealthController::default();
        controller.set_intent(MirrorIntent::Enabled);
        record_remote_health_signal(
            &mut store,
            &mut controller,
            RemoteHealthSignal::RateLimited { http_status: 429 },
            42,
        )
        .unwrap();
        let restored = RemoteHealthController::from_events(store.events(), 42).unwrap();
        assert_eq!(restored, controller);
        assert_eq!(store.events().len(), 1);
    }

    #[test]
    fn manual_pause_is_persisted_without_remote_work() {
        let mut store = MemoryEventStore::default();
        let mut controller = RemoteHealthController::default();
        record_remote_health_intent(&mut store, &mut controller, MirrorIntent::ManuallyPaused, 7)
            .unwrap();
        let restored = RemoteHealthController::from_events(store.events(), 7).unwrap();
        assert_eq!(restored.intent, MirrorIntent::ManuallyPaused);
        assert_eq!(restored.state, RemoteHealthState::ManuallyPaused);
    }
}
