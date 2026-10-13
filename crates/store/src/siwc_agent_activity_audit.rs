//! Sanitized, local-conversation-owned SIWC inference activity journal.
//!
//! Bridge events contain timing and evidence, not reasoning, prompt bodies,
//! model output, credentials, or an implicit license to retry a POST.
//! No activity record is automatically included in inference context.

use crate::{EventEnvelope, EventStore};
use chatarium_core::{EventKind, LocalConversationId, LocalTurnId};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-siwc-agent-activity";
const VERSION: u64 = 1;
const MAX_FRAME_BYTES: usize = 4096;
const SOURCE: &str = "siwc_bridge_observation";

#[derive(Debug, Clone, PartialEq)]
pub struct SiwcAgentActivityRecord {
    pub conversation_id: LocalConversationId,
    pub turn_id: LocalTurnId,
    pub request_id: String,
    pub source_sequence: u64,
    pub stage: String,
    pub observed_at_utc: String,
    pub elapsed_ms: f64,
    pub delta_chars: Option<u64>,
    pub http_status: Option<u16>,
    pub delivery_evidence: Option<String>,
    pub cancelled: Option<bool>,
    pub recorded_sequence: u64,
}

/// Validate a bridge observation and its prospective replay before appending.
/// The owner and local turn must be supplied by the native request dispatcher,
/// not taken from untrusted sidecar JSON. Call from the persistence worker.
pub fn append_siwc_agent_activity_checked(
    store: &mut impl EventStore,
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    frame: &str,
) -> Result<EventEnvelope, String> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err("SIWC activity frame is oversized".to_owned());
    }
    let observation: Value =
        serde_json::from_str(frame).map_err(|_| "SIWC activity frame is not JSON".to_owned())?;
    // Parse first, then serialize *only* validated metadata. The untrusted
    // original frame is never persisted, even on a malformed-frame failure.
    let parsed = parse_observation(&observation, conversation_id, turn_id, 0)?;
    let safe = observation_value(&parsed);
    let payload = json!({
        "schema": SCHEMA,
        "version": VERSION,
        "conversation_id": conversation_id.to_string(),
        "turn_id": turn_id.to_string(),
        "observation": safe,
    })
    .to_string();
    let scope = activity_scope(conversation_id, turn_id);
    // Streaming emits many small activity frames. Do not clone the entire
    // native archive (potentially gigabytes of transcripts) on every delta.
    // Replay only the activity records; all other EventKinds are ignored by
    // this validator. Full archive integrity still replays every record.
    let mut prospective = store
        .events()
        .iter()
        .filter(|event| event.kind == EventKind::SiwcAgentActivityObserved)
        .cloned()
        .collect::<Vec<_>>();
    let sequence = store.events().last().map_or(1, |last| last.sequence + 1);
    prospective.push(EventEnvelope {
        sequence,
        at_unix_ms: 0,
        scope: Some(scope.clone()),
        kind: EventKind::SiwcAgentActivityObserved,
        payload: payload.clone(),
    });
    replay_siwc_agent_activity_audit(&prospective)?;
    let durable_sequence = store
        .append_scoped(Some(scope), EventKind::SiwcAgentActivityObserved, payload)
        .map_err(|error| format!("SIWC activity durable append failed: {error}"))?;
    store
        .events()
        .last()
        .filter(|event| event.sequence == durable_sequence)
        .cloned()
        .ok_or_else(|| "SIWC activity append returned no matching event".to_owned())
}

/// The same replay is used for crash recovery, independent of the UI reader.
pub fn replay_siwc_agent_activity_audit(
    events: &[EventEnvelope],
) -> Result<Vec<SiwcAgentActivityRecord>, String> {
    let mut all = Vec::new();
    let mut tracks: BTreeMap<
        (LocalConversationId, LocalTurnId, String),
        Vec<SiwcAgentActivityRecord>,
    > = BTreeMap::new();
    let mut turn_owners = BTreeMap::<LocalTurnId, LocalConversationId>::new();

    for event in events {
        if event.kind != EventKind::SiwcAgentActivityObserved {
            continue;
        }
        let payload: Value = serde_json::from_str(&event.payload).map_err(|_| {
            format!(
                "SIWC activity at journal {} has malformed JSON",
                event.sequence
            )
        })?;
        let object = payload
            .as_object()
            .ok_or_else(|| "SIWC activity payload must be an object".to_owned())?;
        if object.len() != 5
            || payload.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || payload.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err(format!(
                "SIWC activity at journal {} has unknown schema/fields",
                event.sequence
            ));
        }
        let conversation_id = parse_id::<LocalConversationId>(&payload, "conversation_id")?;
        let turn_id = parse_id::<LocalTurnId>(&payload, "turn_id")?;
        let expected_scope = activity_scope(conversation_id, turn_id);
        if event.scope.as_deref() != Some(expected_scope.as_str()) {
            return Err(format!(
                "SIWC activity at journal {} has mismatched scope",
                event.sequence
            ));
        }
        if turn_owners
            .insert(turn_id, conversation_id)
            .is_some_and(|owner| owner != conversation_id)
        {
            return Err(
                "SIWC activity local turn was assigned to multiple conversations".to_owned(),
            );
        }
        let observation = payload
            .get("observation")
            .ok_or_else(|| "SIWC activity is missing observation".to_owned())?;
        let record = parse_observation(observation, conversation_id, turn_id, event.sequence)?;
        let key = (conversation_id, turn_id, record.request_id.clone());
        let track = tracks.entry(key).or_default();
        // A definite rejected 429 can be retried by the native delivery
        // controller using the same Responses request identity. The bridge
        // produces a new attempt with source sequence 1 and fresh elapsed
        // time. Accept that reset ONLY after a durably terminal attempt,
        // while retaining the earlier attempt in the immutable journal.
        if track.last().is_some_and(|previous| {
            previous.stage == "failed"
                && previous.delivery_evidence.as_deref() == Some("http_rejected")
                && previous.http_status == Some(429)
                && record.stage == "started"
                && record.source_sequence == 1
        }) {
            track.clear();
        }
        validate_transition(track, &record)?;
        track.push(record.clone());
        all.push(record);
    }
    Ok(all)
}

/// Read a single conversation's activity without admitting unrelated turns.
#[must_use]
pub fn activity_for_conversation(
    records: &[SiwcAgentActivityRecord],
    conversation_id: LocalConversationId,
) -> Vec<SiwcAgentActivityRecord> {
    records
        .iter()
        .filter(|record| record.conversation_id == conversation_id)
        .cloned()
        .collect()
}

#[must_use]
pub fn activity_scope(conversation_id: LocalConversationId, turn_id: LocalTurnId) -> String {
    format!("siwc-activity:{conversation_id}:{turn_id}")
}

fn parse_id<T>(payload: &Value, name: &str) -> Result<T, String>
where
    T: FromStr,
{
    payload
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("SIWC activity missing {name}"))?
        .parse::<T>()
        .map_err(|_| format!("SIWC activity invalid {name}"))
}

fn parse_observation(
    raw: &Value,
    conversation_id: LocalConversationId,
    turn_id: LocalTurnId,
    recorded_sequence: u64,
) -> Result<SiwcAgentActivityRecord, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "SIWC observation must be an object".to_owned())?;
    const FIELDS: &[&str] = &[
        "type",
        "source",
        "request_id",
        "sequence",
        "stage",
        "observed_at_utc",
        "elapsed_ms",
        "delta_chars",
        "http_status",
        "delivery_evidence",
        "cancelled",
    ];
    if object.keys().any(|key| !FIELDS.contains(&key.as_str()))
        || raw.get("type").and_then(Value::as_str) != Some("agent_activity")
        || raw.get("source").and_then(Value::as_str) != Some(SOURCE)
    {
        return Err("SIWC activity has unsupported or untrusted fields".to_owned());
    }
    let request_id = raw
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "SIWC activity has no request identity".to_owned())?;
    if request_id.is_empty()
        || request_id.len() > 256
        || !request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        return Err("SIWC activity request identity is invalid".to_owned());
    }
    let source_sequence = raw
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| "SIWC activity has no source sequence".to_owned())?;
    if !(1..=100_000).contains(&source_sequence) {
        return Err("SIWC activity sequence is out of bounds".to_owned());
    }
    let stage = raw
        .get("stage")
        .and_then(Value::as_str)
        .ok_or_else(|| "SIWC activity stage is absent".to_owned())?;
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
        return Err("SIWC activity stage is unknown".to_owned());
    }
    let observed_at_utc = raw
        .get("observed_at_utc")
        .and_then(Value::as_str)
        .ok_or_else(|| "SIWC activity timestamp is absent".to_owned())?;
    let bytes = observed_at_utc.as_bytes();
    if bytes.len() != 24
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| !matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 23) && !b.is_ascii_digit())
    {
        return Err("SIWC activity timestamp format is invalid".to_owned());
    }
    let elapsed_ms = raw
        .get("elapsed_ms")
        .and_then(Value::as_f64)
        .ok_or_else(|| "SIWC activity duration is absent".to_owned())?;
    if !elapsed_ms.is_finite() || !(0.0..=86_400_000.0).contains(&elapsed_ms) {
        return Err("SIWC activity duration is invalid".to_owned());
    }
    let delta_chars = raw
        .get("delta_chars")
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| "SIWC activity delta count is invalid".to_owned())
        })
        .transpose()?;
    let http_status = raw
        .get("http_status")
        .map(|value| {
            value
                .as_u64()
                .and_then(|number| u16::try_from(number).ok())
                .filter(|status| (400..=599).contains(status))
                .ok_or_else(|| "SIWC activity HTTP status is invalid".to_owned())
        })
        .transpose()?;
    let delivery_evidence = raw
        .get("delivery_evidence")
        .map(|value| {
            value
                .as_str()
                .filter(|value| {
                    matches!(
                        *value,
                        "not_dispatched"
                            | "wire_unconfirmed"
                            | "http_rejected"
                            | "http_acknowledged"
                            | "assistant_delta_seen"
                    )
                })
                .map(str::to_owned)
                .ok_or_else(|| "SIWC activity delivery evidence is invalid".to_owned())
        })
        .transpose()?;
    let cancelled = raw
        .get("cancelled")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "SIWC activity cancellation evidence is invalid".to_owned())
        })
        .transpose()?;
    if matches!(stage, "first_text_delta" | "text_delta") {
        if !delta_chars.is_some_and(|n| n <= 1_000_000)
            || http_status.is_some()
            || delivery_evidence.is_some()
            || cancelled.is_some()
        {
            return Err("SIWC activity text delta fields are invalid".to_owned());
        }
    } else if stage == "http_rejected" {
        if http_status.is_none()
            || delta_chars.is_some()
            || delivery_evidence.is_some()
            || cancelled.is_some()
        {
            return Err("SIWC activity rejection fields are invalid".to_owned());
        }
    } else if matches!(stage, "completed" | "failed") {
        if delivery_evidence.is_none()
            || delta_chars.is_some()
            || (stage == "completed" && (cancelled.is_some() || http_status.is_some()))
            || (stage == "failed"
                && (cancelled.is_none()
                    || (delivery_evidence.as_deref() == Some("http_rejected"))
                        != http_status.is_some()))
        {
            return Err("SIWC activity terminal fields are invalid".to_owned());
        }
    } else if delta_chars.is_some()
        || http_status.is_some()
        || delivery_evidence.is_some()
        || cancelled.is_some()
    {
        return Err("SIWC activity nonterminal fields are invalid".to_owned());
    }
    Ok(SiwcAgentActivityRecord {
        conversation_id,
        turn_id,
        request_id: request_id.to_owned(),
        source_sequence,
        stage: stage.to_owned(),
        observed_at_utc: observed_at_utc.to_owned(),
        elapsed_ms,
        delta_chars,
        http_status,
        delivery_evidence,
        cancelled,
        recorded_sequence,
    })
}

fn observation_value(record: &SiwcAgentActivityRecord) -> Value {
    let mut value = json!({
        "type": "agent_activity",
        "source": SOURCE,
        "request_id": record.request_id,
        "sequence": record.source_sequence,
        "stage": record.stage,
        "observed_at_utc": record.observed_at_utc,
        "elapsed_ms": record.elapsed_ms,
    });
    let object = value.as_object_mut().expect("constructed JSON object");
    if let Some(n) = record.delta_chars {
        object.insert("delta_chars".to_owned(), json!(n));
    }
    if let Some(n) = record.http_status {
        object.insert("http_status".to_owned(), json!(n));
    }
    if let Some(ref evidence) = record.delivery_evidence {
        object.insert("delivery_evidence".to_owned(), json!(evidence));
    }
    if let Some(cancelled) = record.cancelled {
        object.insert("cancelled".to_owned(), json!(cancelled));
    }
    value
}

fn validate_transition(
    previous: &[SiwcAgentActivityRecord],
    record: &SiwcAgentActivityRecord,
) -> Result<(), String> {
    if previous.is_empty() {
        if record.stage != "started" || record.source_sequence != 1 {
            return Err("SIWC activity must begin with the first started event".to_owned());
        }
        return Ok(());
    }
    let last = previous.last().expect("nonempty track");
    // The local client can schedule another attempt only following an
    // explicitly observed HTTP 429 rejection, never a timeout/unknown POST
    // outcome. The bridge starts a fresh source sequence for that attempt.
    if record.stage == "started" {
        if record.source_sequence == 1
            && last.stage == "failed"
            && last.delivery_evidence.as_deref() == Some("http_rejected")
            && last.http_status == Some(429)
        {
            return Ok(());
        }
        return Err("SIWC activity cannot restart after ambiguous or completed delivery".to_owned());
    }
    // Validate only the current attempt. An earlier rejected 429 is kept
    // durably but must not make this attempt look already acknowledged.
    let start = previous
        .iter()
        .rposition(|row| row.stage == "started")
        .ok_or_else(|| "SIWC activity has no start event".to_owned())?;
    let current = &previous[start..];
    let last = current.last().expect("current attempt is nonempty");
    if record.source_sequence != last.source_sequence + 1
        || record.elapsed_ms < last.elapsed_ms
        || matches!(last.stage.as_str(), "completed" | "failed")
    {
        return Err("SIWC activity has nonmonotonic sequence, time, or terminal replay".to_owned());
    }
    let observed = |stage: &str| current.iter().any(|row| row.stage == stage);
    let wire = observed("wire_attempted");
    let ack = observed("acknowledged");
    let rejected = observed("http_rejected");
    let text = observed("first_text_delta");
    let okay = match record.stage.as_str() {
        "started" => false,
        "wire_attempted" => !wire,
        "acknowledged" => wire && !ack && !rejected,
        "http_rejected" => wire && !ack && !rejected,
        "first_text_delta" => ack && !text && !rejected,
        "text_delta" => text && !rejected,
        "completed" => !rejected,
        "failed" => true,
        _ => false,
    };
    if !okay {
        return Err("SIWC activity has invalid stage ordering".to_owned());
    }
    if matches!(record.stage.as_str(), "completed" | "failed") {
        let expected = if text {
            "assistant_delta_seen"
        } else if ack {
            "http_acknowledged"
        } else if rejected {
            "http_rejected"
        } else if wire {
            "wire_unconfirmed"
        } else {
            "not_dispatched"
        };
        if record.delivery_evidence.as_deref() != Some(expected)
            || (rejected
                && record.http_status
                    != current
                        .iter()
                        .find(|row| row.stage == "http_rejected")
                        .and_then(|row| row.http_status))
        {
            return Err(
                "SIWC activity terminal claim exceeds observed transport evidence".to_owned(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JsonlEventStore, MemoryEventStore};
    use std::fs;

    fn frame(request_id: &str, sequence: u64, stage: &str, extra: Value) -> String {
        let mut value = json!({
            "type": "agent_activity",
            "source": SOURCE,
            "request_id": request_id,
            "sequence": sequence,
            "stage": stage,
            "observed_at_utc": "2026-10-13T18:30:00.000Z",
            "elapsed_ms": sequence as f64 * 10.0,
        });
        for (key, value_extra) in extra.as_object().expect("test extra object") {
            value
                .as_object_mut()
                .unwrap()
                .insert(key.clone(), value_extra.clone());
        }
        value.to_string()
    }

    #[test]
    fn evidence_is_durable_scoped_and_excluded_from_other_conversations() {
        let mut store = MemoryEventStore::default();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let first_turn = LocalTurnId::new();
        let second_turn = LocalTurnId::new();
        let sequence = [
            frame("req-A", 1, "started", json!({})),
            frame("req-A", 2, "wire_attempted", json!({})),
            frame("req-A", 3, "acknowledged", json!({})),
            frame("req-A", 4, "first_text_delta", json!({"delta_chars": 17})),
            frame(
                "req-A",
                5,
                "completed",
                json!({"delivery_evidence": "assistant_delta_seen"}),
            ),
        ];
        for raw in &sequence {
            append_siwc_agent_activity_checked(&mut store, first, first_turn, raw).unwrap();
        }
        append_siwc_agent_activity_checked(
            &mut store,
            second,
            second_turn,
            &frame("req-B", 1, "started", json!({})),
        )
        .unwrap();
        let rows = replay_siwc_agent_activity_audit(store.events()).unwrap();
        assert_eq!(rows.len(), 6);
        assert_eq!(activity_for_conversation(&rows, first).len(), 5);
        assert_eq!(activity_for_conversation(&rows, second).len(), 1);
        assert_eq!(
            rows[4].delivery_evidence.as_deref(),
            Some("assistant_delta_seen")
        );
        assert!(
            !store
                .events()
                .iter()
                .any(|event| event.payload.contains("private body"))
        );
    }

    #[test]
    fn malformed_private_frames_and_forged_delivery_do_not_mutate_history() {
        let mut store = MemoryEventStore::default();
        let conversation_id = LocalConversationId::new();
        let turn_id = LocalTurnId::new();
        let start = frame("req-A", 1, "started", json!({}));
        append_siwc_agent_activity_checked(&mut store, conversation_id, turn_id, &start).unwrap();
        for malformed in [
            frame(
                "req-A",
                2,
                "wire_attempted",
                json!({"access_token":"private"}),
            ),
            frame(
                "req-A",
                2,
                "completed",
                json!({"delivery_evidence":"http_acknowledged"}),
            ),
            frame("req-A", 5, "wire_attempted", json!({})),
            frame("req-A", 2, "text_delta", json!({"delta_chars":1})),
            frame("req-A", 2, "http_rejected", json!({"http_status":200})),
        ] {
            assert!(
                append_siwc_agent_activity_checked(
                    &mut store,
                    conversation_id,
                    turn_id,
                    &malformed
                )
                .is_err()
            );
        }
        assert_eq!(store.events().len(), 1);
    }

    #[test]
    fn explicit_http_rejection_is_never_an_unknown_delivery_claim() {
        let mut store = MemoryEventStore::default();
        let c = LocalConversationId::new();
        let t = LocalTurnId::new();
        for frame in [
            frame("r", 1, "started", json!({})),
            frame("r", 2, "wire_attempted", json!({})),
            frame("r", 3, "http_rejected", json!({"http_status":429})),
            frame(
                "r",
                4,
                "failed",
                json!({
                    "http_status":429, "cancelled":false, "delivery_evidence":"http_rejected"
                }),
            ),
        ] {
            append_siwc_agent_activity_checked(&mut store, c, t, &frame).unwrap();
        }
        let rows = replay_siwc_agent_activity_audit(store.events()).unwrap();
        assert_eq!(
            rows.last().unwrap().delivery_evidence.as_deref(),
            Some("http_rejected")
        );
        assert_eq!(rows.last().unwrap().http_status, Some(429));
    }

    #[test]
    fn retry_after_observed_http_429_keeps_two_attempts_under_one_request_id() {
        let c = LocalConversationId::new();
        let t = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        for (seq, stage, extra) in [
            (1, "started", json!({})),
            (2, "wire_attempted", json!({})),
            (3, "http_rejected", json!({"http_status": 429})),
            (4, "failed", json!({
                "http_status": 429,
                "cancelled": false,
                "delivery_evidence": "http_rejected"
            })),
            // Same request ID, but a new genuine inference attempt.
            (1, "started", json!({})),
            (2, "wire_attempted", json!({})),
            (3, "acknowledged", json!({})),
            (4, "first_text_delta", json!({"delta_chars": 5})),
            (5, "completed", json!({
                "delivery_evidence": "assistant_delta_seen"
            })),
        ] {
            let event = frame("same-request", seq, stage, extra);
            append_siwc_agent_activity_checked(&mut store, c, t, &event).unwrap();
        }
        let records = replay_siwc_agent_activity_audit(store.events()).unwrap();
        assert_eq!(records.len(), 9);
        assert_eq!(
            records.iter().filter(|row| row.stage == "started").count(), 2
        );
        assert_eq!(records[3].delivery_evidence.as_deref(), Some("http_rejected"));
        assert_eq!(
            records.last().unwrap().delivery_evidence.as_deref(),
            Some("assistant_delta_seen")
        );
        assert!(records.windows(2).all(|pair| {
            pair[0].recorded_sequence < pair[1].recorded_sequence
        }));
    }

    #[test]
    fn new_attempt_cannot_reset_a_nonterminal_inference() {
        let mut store = MemoryEventStore::default();
        let c = LocalConversationId::new();
        let t = LocalTurnId::new();
        append_siwc_agent_activity_checked(
            &mut store, c, t, &frame("r", 1, "started", json!({}))
        ).unwrap();
        assert!(append_siwc_agent_activity_checked(
            &mut store, c, t, &frame("r", 1, "started", json!({}))
        ).is_err());
        assert_eq!(store.events().len(), 1);
    }

    #[test]
    fn explicit_429_can_begin_a_second_attempt_but_unknown_delivery_cannot() {
        let c = LocalConversationId::new();
        let t = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        for frame in [
            frame("same-request", 1, "started", json!({})),
            frame("same-request", 2, "wire_attempted", json!({})),
            frame("same-request", 3, "http_rejected", json!({"http_status":429})),
            frame("same-request", 4, "failed", json!({
                "http_status":429, "cancelled":false, "delivery_evidence":"http_rejected"
            })),
            frame("same-request", 1, "started", json!({})),
            frame("same-request", 2, "wire_attempted", json!({})),
            frame("same-request", 3, "acknowledged", json!({})),
            frame("same-request", 4, "first_text_delta", json!({"delta_chars":5})),
            frame("same-request", 5, "completed", json!({
                "delivery_evidence":"assistant_delta_seen"
            })),
        ] {
            append_siwc_agent_activity_checked(&mut store, c, t, &frame).unwrap();
        }
        assert_eq!(replay_siwc_agent_activity_audit(store.events()).unwrap().len(), 9);
        assert!(append_siwc_agent_activity_checked(
            &mut store, c, t, &frame("same-request", 1, "started", json!({}))
        ).is_err());

        let uncertain_turn = LocalTurnId::new();
        for frame in [
            frame("uncertain", 1, "started", json!({})),
            frame("uncertain", 2, "wire_attempted", json!({})),
            frame("uncertain", 3, "failed", json!({
                "cancelled":false, "delivery_evidence":"wire_unconfirmed"
            })),
        ] {
            append_siwc_agent_activity_checked(&mut store, c, uncertain_turn, &frame).unwrap();
        }
        let prior_len = store.events().len();
        assert!(append_siwc_agent_activity_checked(
            &mut store, c, uncertain_turn, &frame("uncertain", 1, "started", json!({}))
        ).is_err());
        assert_eq!(store.events().len(), prior_len);
    }

    #[test]
    fn file_reopen_preserves_source_order_and_exact_owner() {
        let c = LocalConversationId::new();
        let t = LocalTurnId::new();
        let path = std::env::temp_dir().join(format!("chatarium-siwc-activity-{t}.jsonl"));
        {
            let mut store = JsonlEventStore::open(&path).unwrap();
            append_siwc_agent_activity_checked(
                &mut store,
                c,
                t,
                &frame("r", 1, "started", json!({})),
            )
            .unwrap();
            append_siwc_agent_activity_checked(
                &mut store,
                c,
                t,
                &frame("r", 2, "wire_attempted", json!({})),
            )
            .unwrap();
        }
        let reopened = JsonlEventStore::open(&path).unwrap();
        let rows = replay_siwc_agent_activity_audit(reopened.events()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].source_sequence, 1);
        assert_eq!(rows[1].source_sequence, 2);
        assert_eq!(rows[1].conversation_id, c);
        drop(reopened);
        fs::remove_file(path).unwrap();
    }
}
