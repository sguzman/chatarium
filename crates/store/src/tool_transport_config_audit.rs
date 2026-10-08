//! Durable, *inert* stdio transport configuration for registered tool providers.
//!
//! Storing a provider executable path is NOT permission to launch a process.
//! No process is spawned, no network address is contacted, no secrets are read,
//! and no ToolCall route is dispatched by this module. A future adapter must
//! still require explicit activation and the one-shot user-approved RouteGate.

use crate::tool_call_audit::replay_tool_call_audit;
use crate::tool_provider_audit::replay_tool_provider_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::tool::{
    StdioToolProviderConfig, ToolOperationName, ToolProviderId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SCHEMA: &str = "chatarium-tool-transport-config-audit";
const VERSION: u64 = 1;
/// The existing side-effect-free smoke provider cannot be replaced by an
/// arbitrary external executable under the same privileged identity.
pub const RESERVED_BUILTIN_PROVIDER: &str = "chatarium.builtin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTransportConfigRecord {
    pub provider_id: ToolProviderId,
    pub config: StdioToolProviderConfig,
    pub configured_sequence: u64,
}

/// Append an inert provider transport configuration. Use the checked variant
/// in the persistence worker so malformed config cannot poison the journal.
pub fn record_tool_transport_configured(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    config: &StdioToolProviderConfig,
) -> std::io::Result<u64> {
    let payload = config_value(provider_id, config);
    store.append_scoped(
        Some(config_scope(provider_id)),
        EventKind::ToolProviderTransportConfigured,
        serde_json::to_string(&payload).map_err(invalid_data)?,
    )
}

/// Fail closed before durable append. A provider is immutable once configured
/// and must not have any earlier recorded calls, even if still pending.
pub fn append_tool_transport_config_checked(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    config: &StdioToolProviderConfig,
) -> Result<EventEnvelope, String> {
    let next_sequence = u64::try_from(store.events().len())
        .map_err(|error| error.to_string())?
        .checked_add(1)
        .ok_or_else(|| "transport configuration sequence exhausted".to_owned())?;
    let payload = config_value(provider_id, config).to_string();
    let mut prospective = store.events().to_vec();
    prospective.push(EventEnvelope {
        sequence: next_sequence,
        at_unix_ms: 0,
        scope: Some(config_scope(provider_id)),
        kind: EventKind::ToolProviderTransportConfigured,
        payload,
    });
    let replayed = replay_tool_transport_config_audit(&prospective)?;
    if !replayed.iter().any(|record| {
        record.provider_id == provider_id
            && record.config == *config
            && record.configured_sequence == next_sequence
    }) {
        return Err("prospective provider transport configuration did not replay".to_owned());
    }
    record_tool_transport_configured(store, provider_id, config)
        .map_err(|error| error.to_string())?;
    store.events().last().cloned()
        .ok_or_else(|| "transport configuration append produced no event".to_owned())
}

/// Replay immutable transport configuration without activating it.
///
/// At configuration time, provider identity and endpoint must already exist,
/// and no call for that provider may be in the journal prefix. This prevents
/// reinterpreting a formerly inert call under a newly supplied executable.
pub fn replay_tool_transport_config_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolTransportConfigRecord>, String> {
    let mut records = BTreeMap::<ToolProviderId, ToolTransportConfigRecord>::new();
    for (index, event) in events.iter().enumerate() {
        if event.kind != EventKind::ToolProviderTransportConfigured {
            continue;
        }
        let value = typed_payload(event)?;
        let provider_id = ToolProviderId::new(required_u64(&value, "provider_id")?);
        if event.scope.as_deref() != Some(config_scope(provider_id).as_str()) {
            return Err(format!(
                "tool transport configuration at #{} has incorrect scope",
                event.sequence
            ));
        }
        if records.contains_key(&provider_id) {
            return Err(format!(
                "duplicate transport configuration for provider {} at #{}",
                provider_id.get(), event.sequence
            ));
        }
        let executable = required_string(&value, "executable")?;
        let args = required_string_array(&value, "argv")?;
        let operations = required_string_array(&value, "allowed_operations")?
            .into_iter()
            .map(|value| {
                ToolOperationName::new(value).map_err(|error| {
                    format!(
                        "invalid tool transport operation at #{}: {error:?}",
                        event.sequence
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = StdioToolProviderConfig::new(executable, args, operations).map_err(|error| {
            format!(
                "invalid stdio provider configuration at #{}: {error:?}",
                event.sequence
            )
        })?;

        let prior = &events[..index];
        let provider = replay_tool_provider_audit(prior)?
            .into_iter()
            .find(|record| record.provider_id == provider_id)
            .ok_or_else(|| format!(
                "transport configuration at #{} references missing provider {}",
                event.sequence, provider_id.get()
            ))?;
        if provider.registered_sequence >= event.sequence
            || provider.endpoint_bound_sequence.is_none_or(|bound| bound >= event.sequence)
        {
            return Err(format!(
                "provider {} must have an earlier durable routing endpoint before transport configuration",
                provider_id.get()
            ));
        }
        if provider.name.as_str() == RESERVED_BUILTIN_PROVIDER {
            return Err("reserved builtin provider cannot receive an external stdio transport".to_owned());
        }
        if replay_tool_call_audit(prior)?
            .iter()
            .any(|call| call.provider_id == provider_id)
        {
            return Err(format!(
                "provider {} already has immutable tool call history and cannot be configured retroactively",
                provider_id.get()
            ));
        }

        records.insert(provider_id, ToolTransportConfigRecord {
            provider_id,
            config,
            configured_sequence: event.sequence,
        });
    }

    let mut values = records.into_values().collect::<Vec<_>>();
    values.sort_by_key(|record| record.configured_sequence);
    Ok(values)
}

fn config_value(provider_id: ToolProviderId, config: &StdioToolProviderConfig) -> Value {
    json!({
        "schema": SCHEMA,
        "version": VERSION,
        "record": "tool_transport_configured",
        "provider_id": provider_id.get(),
        "transport": "stdio",
        "executable": config.executable(),
        "argv": config.args(),
        "allowed_operations": config
            .allowed_operations()
            .iter()
            .map(ToolOperationName::as_str)
            .collect::<Vec<_>>(),
    })
}

fn typed_payload(event: &EventEnvelope) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload)
        .map_err(|error| format!("malformed tool transport at #{}: {error}", event.sequence))?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
        || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        || value.get("record").and_then(Value::as_str) != Some("tool_transport_configured")
        || value.get("transport").and_then(Value::as_str) != Some("stdio")
    {
        return Err(format!(
            "tool transport configuration at #{} has unsupported schema, version, record or transport",
            event.sequence
        ));
    }
    Ok(value)
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value.get(field).and_then(Value::as_u64)
        .ok_or_else(|| format!("tool transport config missing integer '{field}'"))
}
fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str)
        .ok_or_else(|| format!("tool transport config missing string '{field}'"))
}
fn required_string_array(value: &Value, field: &str) -> Result<Vec<String>, String> {
    let array = value.get(field).and_then(Value::as_array)
        .ok_or_else(|| format!("tool transport config missing array '{field}'"))?;
    array
        .iter()
        .map(|item| item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
            format!("tool transport config array '{field}' contains a non-string")
        }))
        .collect()
}
#[must_use]
pub fn config_scope(provider_id: ToolProviderId) -> String {
    format!("tool-transport-config:{}", provider_id.get())
}
fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::session_audit::record_local_session_registered;
    use crate::tool_call_audit::record_tool_call;
    use crate::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use chatarium_core::routing::RouteEndpointId;
    use chatarium_core::session::SessionId;
    use chatarium_core::tool::{
        ToolCallId, ToolProviderEndpointBinding, ToolProviderName,
    };

    const PROVIDER: ToolProviderId = ToolProviderId::new(9);
    fn setup(store: &mut impl EventStore, name: &str) {
        record_tool_provider_registered(store, PROVIDER, &ToolProviderName::new(name).unwrap())
            .unwrap();
        record_tool_provider_endpoint_bound(
            store,
            ToolProviderEndpointBinding::new(PROVIDER, RouteEndpointId::new(100)),
        ).unwrap();
    }
    fn config() -> StdioToolProviderConfig {
        StdioToolProviderConfig::new(
            "/usr/bin/local-mcp",
            vec!["--stdio".to_owned()],
            vec![
                ToolOperationName::new("hello").unwrap(),
                ToolOperationName::new("search").unwrap(),
            ],
        ).unwrap()
    }

    #[test]
    fn configuration_is_persisted_but_does_not_dispatch_or_execute() {
        let mut store = MemoryEventStore::default();
        setup(&mut store, "my.local.server");
        let event = append_tool_transport_config_checked(
            &mut store, PROVIDER, &config()
        ).unwrap();
        assert_eq!(event.kind, EventKind::ToolProviderTransportConfigured);
        let records = replay_tool_transport_config_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].config, config());
        assert_eq!(records[0].configured_sequence, event.sequence);
        assert!(!store.events().iter().any(|event| {
            matches!(event.kind, EventKind::RouteDispatched | EventKind::ToolCallOutcomeObserved)
        }));
    }

    #[test]
    fn reserved_builtin_and_duplicate_configurations_are_rejected_before_append() {
        let mut builtin = MemoryEventStore::default();
        setup(&mut builtin, RESERVED_BUILTIN_PROVIDER);
        let before = builtin.events().len();
        assert!(append_tool_transport_config_checked(
            &mut builtin, PROVIDER, &config()
        ).unwrap_err().contains("reserved builtin"));
        assert_eq!(builtin.events().len(), before);

        let mut store = MemoryEventStore::default();
        setup(&mut store, "external");
        append_tool_transport_config_checked(&mut store, PROVIDER, &config()).unwrap();
        let before = store.events().len();
        assert!(append_tool_transport_config_checked(
            &mut store, PROVIDER, &config()
        ).unwrap_err().contains("duplicate transport configuration"));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn provider_must_be_registered_addressable_and_have_no_prior_calls() {
        let mut store = MemoryEventStore::default();
        let before = store.events().len();
        assert!(append_tool_transport_config_checked(
            &mut store, PROVIDER, &config()
        ).unwrap_err().contains("missing provider"));
        assert_eq!(store.events().len(), before);

        record_tool_provider_registered(
            &mut store, PROVIDER, &ToolProviderName::new("external").unwrap()
        ).unwrap();
        let before = store.events().len();
        assert!(append_tool_transport_config_checked(
            &mut store, PROVIDER, &config()
        ).unwrap_err().contains("earlier durable routing endpoint"));
        assert_eq!(store.events().len(), before);

        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(PROVIDER, RouteEndpointId::new(100)),
        ).unwrap();
        record_local_session_registered(&mut store, SessionId::new(1)).unwrap();
        record_tool_call(
            &mut store,
            ToolCallId::new(1),
            SessionId::new(1),
            PROVIDER,
            &ToolOperationName::new("hello").unwrap(),
            "{}",
        ).unwrap();
        let before = store.events().len();
        assert!(append_tool_transport_config_checked(
            &mut store, PROVIDER, &config()
        ).unwrap_err().contains("cannot be configured retroactively"));
        assert_eq!(store.events().len(), before);
    }

    #[test]
    fn malformed_complete_configuration_fails_replay() {
        let mut store = MemoryEventStore::default();
        setup(&mut store, "external");
        store.append_scoped(
            Some(config_scope(PROVIDER)),
            EventKind::ToolProviderTransportConfigured,
            json!({
                "schema": SCHEMA,
                "version": VERSION,
                "record": "tool_transport_configured",
                "provider_id": PROVIDER.get(),
                "transport": "stdio",
                "executable": "relative/path",
                "argv": [],
                "allowed_operations": ["hello"],
            }).to_string(),
        ).unwrap();
        assert!(replay_tool_transport_config_audit(store.events()).is_err());
    }
}
