//! Visible, per-session ChatGPT delivery evidence. This is telemetry, not remote receipt proof.
use std::time::{Duration, Instant};

pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Durable,
    BridgeQueued,
    WireAttempted,
    Acknowledged,
    Streaming,
    RetryScheduled,
    Complete,
    Failed,
    Unconfirmed,
}

#[derive(Debug, Clone)]
pub struct Attempt {
    pub conversation_id: String,
    pub request_id: String,
    pub phase: Phase,
    pub started_at: Instant,
    pub wire_at: Option<Instant>,
    pub acknowledged_at: Option<Instant>,
    pub first_output_at: Option<Instant>,
    pub finished_at: Option<Instant>,
    pub retry_at: Option<Instant>,
    pub retries: u32,
    pub error: Option<String>,
}

impl Attempt {
    pub fn seconds(&self) -> f64 {
        self.finished_at
            .unwrap_or_else(Instant::now)
            .duration_since(self.started_at)
            .as_secs_f64()
    }

    pub fn time_to_first_output(&self) -> Option<f64> {
        self.first_output_at
            .map(|t| t.duration_since(self.started_at).as_secs_f64())
    }

    pub fn time_to_acknowledgment(&self) -> Option<f64> {
        self.acknowledged_at
            .map(|t| t.duration_since(self.started_at).as_secs_f64())
    }

    pub fn label(&self) -> &'static str {
        match self.phase {
            Phase::Durable => "Saved locally · not yet dispatched",
            Phase::BridgeQueued => "Queued to bridge · waiting for network",
            Phase::WireAttempted => "HTTP request started · delivery not confirmed",
            Phase::Acknowledged => "ChatGPT acknowledged · generating response",
            Phase::Streaming => "ChatGPT responding · streaming",
            Phase::RetryScheduled => "Rate-limited · automatic retry scheduled",
            Phase::Complete => "Assistant finished · waiting for you",
            Phase::Failed => "Request failed",
            Phase::Unconfirmed => "Connection lost · outcome unknown; not resent",
        }
    }

    pub fn slow(&self, warning_after: Duration) -> bool {
        self.finished_at.is_none()
            && self.phase != Phase::RetryScheduled
            && self.phase != Phase::Failed
            && self.phase != Phase::Unconfirmed
            && self.first_output_at.is_none()
            && self.started_at.elapsed() >= warning_after
    }
}

#[derive(Default)]
pub struct Timeline {
    attempts: Vec<Attempt>,
}

impl Timeline {
    pub fn begin(&mut self, conversation_id: String, request_id: String) {
        self.attempts.push(Attempt {
            conversation_id,
            request_id,
            phase: Phase::Durable,
            started_at: Instant::now(),
            wire_at: None,
            acknowledged_at: None,
            first_output_at: None,
            finished_at: None,
            retry_at: None,
            retries: 0,
            error: None,
        });
        if self.attempts.len() > 100 {
            self.attempts.remove(0);
        }
    }

    pub fn set(&mut self, request_id: &str, phase: Phase) {
        if let Some(attempt) = self
            .attempts
            .iter_mut()
            .rev()
            .find(|a| a.request_id == request_id)
        {
            attempt.phase = phase;
            let now = Instant::now();
            match phase {
                Phase::WireAttempted => attempt.wire_at = Some(now),
                Phase::Acknowledged => attempt.acknowledged_at = Some(now),
                Phase::Streaming => {
                    if attempt.first_output_at.is_none() {
                        attempt.first_output_at = Some(now);
                    }
                }
                Phase::Complete | Phase::Failed | Phase::Unconfirmed => {
                    attempt.finished_at = Some(now);
                    attempt.retry_at = None;
                }
                _ => {}
            }
        }
    }

    pub fn retry(&mut self, request_id: &str, backoff: Duration, error: String) {
        if let Some(a) = self
            .attempts
            .iter_mut()
            .rev()
            .find(|a| a.request_id == request_id)
        {
            a.retries += 1;
            a.phase = Phase::RetryScheduled;
            a.retry_at = Some(Instant::now() + backoff);
            a.error = Some(error);
        }
    }

    pub fn error(&mut self, request_id: &str, phase: Phase, detail: String) {
        self.set(request_id, phase);
        if let Some(a) = self
            .attempts
            .iter_mut()
            .rev()
            .find(|a| a.request_id == request_id)
        {
            a.error = Some(detail);
        }
    }

    pub fn for_conversation<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> impl Iterator<Item = &'a Attempt> {
        self.attempts
            .iter()
            .filter(move |a| a.conversation_id == conversation_id)
    }

    pub fn mean_first_output_seconds(&self, conversation_id: &str) -> Option<f64> {
        let (sum, count) = self
            .for_conversation(conversation_id)
            .filter(|attempt| attempt.phase == Phase::Complete)
            .filter_map(Attempt::time_to_first_output)
            .fold((0.0, 0), |(sum, count), seconds| (sum + seconds, count + 1));
        (count > 0).then(|| sum / f64::from(count))
    }

    pub fn mean_response_seconds(&self, conversation_id: &str) -> Option<f64> {
        let completed = self
            .for_conversation(conversation_id)
            .filter(|a| a.phase == Phase::Complete)
            .collect::<Vec<_>>();
        (!completed.is_empty())
            .then(|| completed.iter().map(|a| a.seconds()).sum::<f64>() / completed.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_receipt_claim_before_server_ack() {
        let mut timeline = Timeline::default();
        timeline.begin("chat".to_owned(), "turn".to_owned());
        timeline.set("turn", Phase::BridgeQueued);
        timeline.set("turn", Phase::WireAttempted);
        let status = timeline.for_conversation("chat").next().unwrap();
        assert!(status.acknowledged_at.is_none());
        assert_eq!(status.phase, Phase::WireAttempted);
    }
    #[test]
    fn rate_limit_retry_records_attempt_count() {
        let mut timeline = Timeline::default();
        timeline.begin("chat".to_owned(), "turn".to_owned());
        timeline.retry("turn", Duration::from_secs(2), "HTTP 429".to_owned());
        let a = timeline.for_conversation("chat").next().unwrap();
        assert_eq!(a.retries, 1);
        assert_eq!(a.phase, Phase::RetryScheduled);
        assert!(a.retry_at.is_some());
    }
    #[test]
    fn slow_warning_respects_threshold_and_terminal_state() {
        let now = Instant::now();
        let mut attempt = Attempt {
            conversation_id: "chat".to_owned(),
            request_id: "turn".to_owned(),
            phase: Phase::Acknowledged,
            started_at: now - Duration::from_secs(15),
            wire_at: None,
            acknowledged_at: Some(now - Duration::from_secs(12)),
            first_output_at: None,
            finished_at: None,
            retry_at: None,
            retries: 0,
            error: None,
        };
        assert!(attempt.slow(Duration::from_secs(12)));
        assert!(!attempt.slow(Duration::from_secs(30)));
        assert!(attempt.time_to_acknowledgment().is_some());
        attempt.first_output_at = Some(now);
        assert!(!attempt.slow(Duration::from_secs(3)));
        attempt.first_output_at = None;
        attempt.phase = Phase::Unconfirmed;
        attempt.finished_at = Some(now);
        assert!(!attempt.slow(Duration::from_secs(3)));
    }

    #[test]
    fn session_average_ignores_failures() {
        let mut timeline = Timeline::default();
        timeline.begin("chat".to_owned(), "ok".to_owned());
        timeline.set("ok", Phase::Complete);
        timeline.begin("chat".to_owned(), "no".to_owned());
        timeline.set("no", Phase::Failed);
        assert!(timeline.mean_response_seconds("chat").is_some());
        assert_eq!(timeline.for_conversation("chat").count(), 2);
    }
}
