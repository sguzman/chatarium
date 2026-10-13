//! Journal-verified, conversation-owned inference activity in native chat.
//!
//! This projection is UI-only. It never enters user/assistant inference
//! context. Malformed audit replay fails closed rather than leaking an event.

use super::{DisplayMessage, DisplayRole};
use chatarium_core::{LocalConversationId, LocalTurnId};
use chatarium_store::EventEnvelope;
use chatarium_store::siwc_agent_activity_audit::{
    SiwcAgentActivityRecord, replay_siwc_agent_activity_audit,
};
use std::collections::BTreeMap;

type ActivityKey = (LocalConversationId, LocalTurnId, String);

pub fn verified_activity(
    events: &[EventEnvelope],
) -> BTreeMap<LocalConversationId, Vec<DisplayMessage>> {
    let Ok(records) = replay_siwc_agent_activity_audit(events) else {
        return BTreeMap::new();
    };
    let mut groups = BTreeMap::<ActivityKey, Vec<SiwcAgentActivityRecord>>::new();
    for record in records {
        groups
            .entry((
                record.conversation_id,
                record.turn_id,
                record.request_id.clone(),
            ))
            .or_default()
            .push(record);
    }
    let timestamps = events
        .iter()
        .map(|event| (event.sequence, event.at_unix_ms))
        .collect::<BTreeMap<_, _>>();
    let mut by_owner = BTreeMap::<LocalConversationId, Vec<DisplayMessage>>::new();
    for ((owner, _turn, request), observations) in groups {
        let Some(first) = observations.first() else {
            continue;
        };
        let sequence = first.recorded_sequence;
        let timestamp = timestamps.get(&sequence).map(|ms| *ms as f64 / 1_000.0);
        let mut text = format!(
            "Observed SIWC inference lifecycle · request {request}\n\
             {} recorded event{}\n",
            observations.len(),
            if observations.len() == 1 { "" } else { "s" },
        );
        for event in &observations {
            use std::fmt::Write;
            let _ = write!(
                text,
                "\n#{}  {}  +{:.3} ms  {}\n",
                event.source_sequence, event.stage, event.elapsed_ms, event.observed_at_utc,
            );
            if let Some(chars) = event.delta_chars {
                let _ = writeln!(text, "  received {chars} text characters");
            }
            if let Some(status) = event.http_status {
                let _ = writeln!(text, "  HTTP {status}");
            }
            if let Some(ref evidence) = event.delivery_evidence {
                let _ = writeln!(text, "  delivery evidence: {evidence}");
            }
            if let Some(cancelled) = event.cancelled {
                let _ = writeln!(text, "  cancelled: {cancelled}");
            }
        }
        by_owner.entry(owner).or_default().push(DisplayMessage {
            role: DisplayRole::Activity,
            text,
            sequence,
            timestamp,
            provenance_label: Some("SIWC bridge observation · verified durable journal".to_owned()),
        });
    }
    for entries in by_owner.values_mut() {
        entries.sort_by_key(|message| message.sequence);
    }
    by_owner
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
    use chatarium_store::{EventStore, MemoryEventStore};
    use serde_json::{Value, json};

    fn frame(request: &str, sequence: u64, stage: &str, extra: Value) -> String {
        let mut value = json!({
            "type": "agent_activity",
            "source": "siwc_bridge_observation",
            "request_id": request,
            "sequence": sequence,
            "stage": stage,
            "observed_at_utc": "2026-10-13T19:00:00.000Z",
            "elapsed_ms": sequence as f64 * 5.0,
        });
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        value.to_string()
    }

    #[test]
    fn activity_is_grouped_by_turn_and_conversation_without_text_admission() {
        let a = LocalConversationId::new();
        let b = LocalConversationId::new();
        let first = LocalTurnId::new();
        let second = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        for (owner, turn, frame) in [
            (a, first, frame("r1", 1, "started", json!({}))),
            (a, first, frame("r1", 2, "wire_attempted", json!({}))),
            (
                a,
                first,
                frame(
                    "r1",
                    3,
                    "failed",
                    json!({
                        "delivery_evidence": "wire_unconfirmed", "cancelled": false
                    }),
                ),
            ),
            (b, second, frame("r2", 1, "started", json!({}))),
        ] {
            append_siwc_agent_activity_checked(&mut store, owner, turn, &frame).unwrap();
        }
        let map = verified_activity(store.events());
        assert_eq!(map[&a].len(), 1);
        assert_eq!(map[&b].len(), 1);
        assert_eq!(map[&a][0].role, DisplayRole::Activity);
        assert!(
            map[&a][0]
                .text
                .contains("delivery evidence: wire_unconfirmed")
        );
        assert!(!map[&b][0].text.contains("r1"));
        assert!(!map[&a][0].text.contains("r2"));
    }

    #[test]
    fn malformed_audit_cannot_become_visible_chat_activity() {
        let a = LocalConversationId::new();
        let turn = LocalTurnId::new();
        let mut store = MemoryEventStore::default();
        append_siwc_agent_activity_checked(
            &mut store,
            a,
            turn,
            &frame("r", 1, "started", json!({})),
        )
        .unwrap();
        let scope = store.events()[0].scope.clone();
        store
            .append_scoped(
                scope,
                chatarium_core::EventKind::SiwcAgentActivityObserved,
                "{malformed".to_owned(),
            )
            .unwrap();
        assert!(verified_activity(store.events()).is_empty());
    }
}
