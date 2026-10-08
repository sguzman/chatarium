//! Durable human-directed activation decisions for configured external tool providers.
//!
//! No event in this module spawns a process, opens a socket, consumes a
//! DispatchPermit, or executes a model-suggested call. The activation audit is
//! necessary, never sufficient, for future one-shot external execution.

use crate::tool_transport_config_audit::replay_tool_transport_config_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::tool::ToolProviderId;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SCHEMA: &str = "chatarium-tool-provider-activation-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderActivationDecision {
    Activate,
    Deactivate,
}

impl ProviderActivationDecision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProviderActivationRecord {
    pub provider_id: ToolProviderId,
    pub configured_sequence: u64,
    pub active: bool,
    /// The latest activation, or None if the current state is deactivated.
    pub activated_sequence: Option<u64>,
    pub last_sequence: u64,
}

/// Unchecked event construction for fixtures/imports. The checked API below
/// must be used by the persistence worker for real user decisions.
pub fn record_tool_provider_activation_decision(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    configured_sequence: u64,
    decision: ProviderActivationDecision,
) -> std::io::Result<u64> {
    store.append_scoped(
        Some(activation_scope(provider_id)),
        EventKind::ToolProviderActivationDecisionRecorded,
        activation_value(provider_id, configured_sequence, decision).to_string(),
    )
}

/// Validate the prospective entire replay *before* appending any decision.
/// A repeated Activate/Deactivate is an error, not an implicit renewal.
pub fn append_tool_provider_activation_decision_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    configured_sequence: u64,
    decision: ProviderActivationDecision,
) -> Result<EventEnvelope, String> {
    let next_sequence = u64::try_from(store.events().len())
        .map_err(|error| error.to_string())?
        .checked_add(1)
        .ok_or_else(|| "tool activation sequence exhausted".to_owned())?;
    let mut prospective = store.events().to_vec();
    prospective.push(EventEnvelope {
        sequence: next_sequence,
        at_unix_ms: 0,
        scope: Some(activation_scope(provider_id)),
        kind: EventKind::ToolProviderActivationDecisionRecorded,
        payload: activation_value(provider_id, configured_sequence, decision).to_string(),
    });
    let replayed = replay_tool_provider_activation_audit(&prospective)?;
    let expected_active = decision == ProviderActivationDecision::Activate;
    if !replayed.iter().any(|record| {
        record.provider_id == provider_id
            && record.configured_sequence == configured_sequence
            && record.active == expected_active
            && record.last_sequence == next_sequence
    }) {
        return Err("prospective provider activation did not replay".to_owned());
    }
    record_tool_provider_activation_decision(store, provider_id, configured_sequence, decision)
        .map_err(|error| error.to_string())?;
    store
        .events()
        .last()
        .cloned()
        .ok_or_else(|| "tool activation append produced no event".to_owned())
}

/// Rebuild active/inactive state and reject invalid transitions. Configuration
/// must predate each decision and the immutable configuration sequence must
/// match exactly; unrelated providers may not inherit activation state.
pub fn replay_tool_provider_activation_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolProviderActivationRecord>, String> {
    let mut records = BTreeMap::<ToolProviderId, ToolProviderActivationRecord>::new();
    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ToolProviderActivationDecisionRecorded {
            continue;
        }
        let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
            format!(
                "malformed provider activation at #{}: {error}",
                event.sequence
            )
        })?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
            || value.get("record").and_then(Value::as_str)
                != Some("tool_provider_activation_decision")
            || value.get("authority").and_then(Value::as_str) != Some("user")
        {
            return Err(format!(
                "unsupported provider activation schema at #{}",
                event.sequence
            ));
        }
        let provider_id = ToolProviderId::new(required_u64(&value, "provider_id")?);
        let configured_sequence = required_u64(&value, "configured_sequence")?;
        if event.scope.as_deref() != Some(activation_scope(provider_id).as_str()) {
            return Err(format!(
                "incorrect provider activation scope at #{}",
                event.sequence
            ));
        }
        let decision = match value.get("decision").and_then(Value::as_str) {
            Some("activate") => ProviderActivationDecision::Activate,
            Some("deactivate") => ProviderActivationDecision::Deactivate,
            _ => {
                return Err(format!(
                    "unknown provider activation decision at #{}",
                    event.sequence
                ));
            }
        };
        let config = replay_tool_transport_config_audit(&events[..index])?
            .into_iter()
            .find(|record| record.provider_id == provider_id)
            .ok_or_else(|| {
                format!(
                    "provider {} has no prior durable external transport configuration",
                    provider_id.get()
                )
            })?;
        if config.configured_sequence != configured_sequence
            || configured_sequence >= event.sequence
        {
            return Err(format!(
                "provider {} activation does not match its immutable configuration",
                provider_id.get()
            ));
        }
        let previous_active = records
            .get(&provider_id)
            .is_some_and(|record| record.active);
        let next_active = decision == ProviderActivationDecision::Activate;
        if previous_active == next_active {
            return Err(format!(
                "redundant or invalid provider {} activation transition at #{}",
                provider_id.get(),
                event.sequence
            ));
        }
        records.insert(
            provider_id,
            ToolProviderActivationRecord {
                provider_id,
                configured_sequence,
                active: next_active,
                activated_sequence: next_active.then_some(event.sequence),
                last_sequence: event.sequence,
            },
        );
    }
    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.last_sequence);
    Ok(values)
}

fn activation_value(
    provider_id: ToolProviderId,
    configured_sequence: u64,
    decision: ProviderActivationDecision,
) -> Value {
    json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "tool_provider_activation_decision",
        "authority": "user",
        "provider_id": provider_id.get(),
        "configured_sequence": configured_sequence,
        "decision": decision.as_str(),
    })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("provider activation missing integer '{field}'"))
}

#[must_use]
pub fn activation_scope(provider_id: ToolProviderId) -> String {
    format!("tool-provider-activation:{}", provider_id.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use crate::tool_transport_config_audit::append_tool_transport_config_checked;
    use chatarium_core::routing::RouteEndpointId;
    use chatarium_core::tool::{
        StdioToolProviderConfig, ToolOperationName, ToolProviderEndpointBinding, ToolProviderName,
    };

    const PROVIDER: ToolProviderId = ToolProviderId::new(8);

    fn setup() -> (MemoryEventStore, u64) {
        let mut store = MemoryEventStore::default();
        record_tool_provider_registered(
            &mut store,
            PROVIDER,
            &ToolProviderName::new("example.external").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(PROVIDER, RouteEndpointId::new(88)),
        )
        .unwrap();
        let config = StdioToolProviderConfig::new(
            "/usr/bin/example-mcp",
            Vec::new(),
            vec![ToolOperationName::new("read").unwrap()],
        )
        .unwrap();
        let seq = append_tool_transport_config_checked(&mut store, PROVIDER, &config)
            .unwrap()
            .sequence;
        (store, seq)
    }

    #[test]
    fn activation_and_revocation_are_durable_but_never_dispatch() {
        let (mut store, config_seq) = setup();
        let first = append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            config_seq,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        assert_eq!(
            first.kind,
            EventKind::ToolProviderActivationDecisionRecorded
        );
        let state = replay_tool_provider_activation_audit(store.events()).unwrap();
        assert!(state[0].active);
        assert_eq!(state[0].activated_sequence, Some(first.sequence));
        let revoked = append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            config_seq,
            ProviderActivationDecision::Deactivate,
        )
        .unwrap();
        assert!(revoked.sequence > first.sequence);
        let state = replay_tool_provider_activation_audit(store.events()).unwrap();
        assert!(!state[0].active);
        assert_eq!(state[0].activated_sequence, None);
        assert!(!store.events().iter().any(|event| matches!(
            event.kind,
            EventKind::RouteDispatched | EventKind::ToolCallOutcomeObserved
        )));
        append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            config_seq,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        assert!(replay_tool_provider_activation_audit(store.events()).unwrap()[0].active);
    }

    #[test]
    fn missing_config_mismatched_sequence_and_redundant_decisions_fail_before_append() {
        let mut unconfigured = MemoryEventStore::default();
        let error = append_tool_provider_activation_decision_checked(
            &mut unconfigured,
            PROVIDER,
            1,
            ProviderActivationDecision::Activate,
        )
        .unwrap_err();
        assert!(error.contains("no prior durable"));
        assert!(unconfigured.events().is_empty());

        let (mut store, config_seq) = setup();
        let before = store.events().len();
        assert!(
            append_tool_provider_activation_decision_checked(
                &mut store,
                PROVIDER,
                config_seq + 1,
                ProviderActivationDecision::Activate
            )
            .unwrap_err()
            .contains("immutable configuration")
        );
        assert_eq!(store.events().len(), before);
        assert!(
            append_tool_provider_activation_decision_checked(
                &mut store,
                PROVIDER,
                config_seq,
                ProviderActivationDecision::Deactivate
            )
            .unwrap_err()
            .contains("redundant")
        );
        assert_eq!(store.events().len(), before);
        append_tool_provider_activation_decision_checked(
            &mut store,
            PROVIDER,
            config_seq,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        let before = store.events().len();
        assert!(
            append_tool_provider_activation_decision_checked(
                &mut store,
                PROVIDER,
                config_seq,
                ProviderActivationDecision::Activate
            )
            .unwrap_err()
            .contains("redundant")
        );
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn corrupted_activation_payload_is_rejected_during_replay() {
        let (mut store, config_seq) = setup();
        record_tool_provider_activation_decision(
            &mut store,
            PROVIDER,
            config_seq,
            ProviderActivationDecision::Activate,
        )
        .unwrap();
        let latest = store.events().last().unwrap().clone();
        let mut corrupted = store.events().to_vec();
        corrupted.last_mut().unwrap().payload = latest.payload.replace("\"user\"", "\"model\"");
        assert!(replay_tool_provider_activation_audit(&corrupted).is_err());
        corrupted.last_mut().unwrap().scope = Some("bad-scope".to_owned());
        assert!(replay_tool_provider_activation_audit(&corrupted).is_err());
    }
}
