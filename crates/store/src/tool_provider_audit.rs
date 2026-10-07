//! Durable local tool/provider identity and routing addressability.
//!
//! Provider registration and endpoint binding are identity facts only. They do
//! not define an MCP transport, grant permission, dispatch a route, or execute a
//! tool.

use crate::routing_audit::replay_routing_audit;
use crate::session_audit::replay_session_audit;
use crate::{EventEnvelope, EventStore};
use chatarium_core::EventKind;
use chatarium_core::routing::RouteEndpointId;
use chatarium_core::tool::{ToolProviderEndpointBinding, ToolProviderId, ToolProviderName};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SCHEMA: &str = "chatarium-tool-provider-audit";
const VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProviderAuditRecord {
    pub provider_id: ToolProviderId,
    pub name: ToolProviderName,
    pub registered_sequence: u64,
    pub endpoint_binding: Option<ToolProviderEndpointBinding>,
    pub endpoint_bound_sequence: Option<u64>,
    pub last_sequence: u64,
}

pub fn record_tool_provider_registered(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    name: &ToolProviderName,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(provider_scope(provider_id)),
        EventKind::ToolProviderRegistered,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "tool_provider_registered",
            "provider_id": provider_id.get(),
            "name": name.as_str(),
        }),
    )
}

pub fn record_tool_provider_endpoint_bound(
    store: &mut impl EventStore,
    binding: ToolProviderEndpointBinding,
) -> std::io::Result<u64> {
    append_typed(
        store,
        Some(provider_endpoint_scope(
            binding.provider_id(),
            binding.endpoint_id(),
        )),
        EventKind::ToolProviderEndpointBound,
        json!({
            "schema": SCHEMA,
            "version": VERSION,
            "record": "tool_provider_endpoint_bound",
            "provider_id": binding.provider_id().get(),
            "endpoint_id": binding.endpoint_id().get(),
        }),
    )
}

pub fn replay_tool_provider_audit(
    events: &[EventEnvelope],
) -> Result<Vec<ToolProviderAuditRecord>, String> {
    let session_endpoint_owner = replay_session_audit(events)?
        .into_iter()
        .filter_map(|record| {
            record
                .endpoint_binding
                .map(|binding| (binding.endpoint_id(), record.session_id))
        })
        .collect::<BTreeMap<_, _>>();

    let mut providers = BTreeMap::<ToolProviderId, ReplayProvider>::new();
    let mut endpoint_owner = BTreeMap::<RouteEndpointId, ToolProviderId>::new();

    for (index, event) in events.iter().enumerate() {
        match event.kind {
            EventKind::ToolProviderRegistered => {
                let value = typed_payload(event, "tool_provider_registered")?;
                let provider_id = ToolProviderId::new(required_u64(&value, "provider_id")?);
                let name = ToolProviderName::new(required_string(&value, "name")?.to_owned())
                    .map_err(|error| {
                        format!(
                            "tool provider {} at sequence {} has invalid name: {error:?}",
                            provider_id.get(),
                            event.sequence
                        )
                    })?;
                validate_scope(event, &provider_scope(provider_id))?;

                if providers.contains_key(&provider_id) {
                    return Err(format!(
                        "duplicate tool provider {} registration at sequence {}",
                        provider_id.get(),
                        event.sequence
                    ));
                }
                providers.insert(
                    provider_id,
                    ReplayProvider {
                        provider_id,
                        name,
                        registered_sequence: event.sequence,
                        endpoint_binding: None,
                        endpoint_bound_sequence: None,
                        last_sequence: event.sequence,
                    },
                );
            }
            EventKind::ToolProviderEndpointBound => {
                let value = typed_payload(event, "tool_provider_endpoint_bound")?;
                let provider_id = ToolProviderId::new(required_u64(&value, "provider_id")?);
                let endpoint_id = RouteEndpointId::new(required_u64(&value, "endpoint_id")?);
                validate_scope(event, &provider_endpoint_scope(provider_id, endpoint_id))?;

                let provider = providers.get_mut(&provider_id).ok_or_else(|| {
                    format!(
                        "tool provider endpoint binding at sequence {} references unregistered provider {}",
                        event.sequence,
                        provider_id.get()
                    )
                })?;
                if let Some(existing) = provider.endpoint_binding {
                    return Err(format!(
                        "tool provider {} is already bound to endpoint {}; cannot also bind endpoint {} at sequence {}",
                        provider_id.get(),
                        existing.endpoint_id().get(),
                        endpoint_id.get(),
                        event.sequence
                    ));
                }
                if let Some(existing_provider) = endpoint_owner.get(&endpoint_id) {
                    return Err(format!(
                        "tool endpoint {} already belongs to provider {}; cannot also bind provider {} at sequence {}",
                        endpoint_id.get(),
                        existing_provider.get(),
                        provider_id.get(),
                        event.sequence
                    ));
                }
                if let Some(session_id) = session_endpoint_owner.get(&endpoint_id) {
                    return Err(format!(
                        "tool endpoint {} collides with session {} routing endpoint",
                        endpoint_id.get(),
                        session_id.get()
                    ));
                }

                let prior_routes = replay_routing_audit(&events[..index])?;
                if let Some(route) = prior_routes.iter().find(|route| {
                    route.request.source == endpoint_id || route.request.destination == endpoint_id
                }) {
                    return Err(format!(
                        "tool provider {} cannot retroactively claim endpoint {} already used by route {} before sequence {}",
                        provider_id.get(),
                        endpoint_id.get(),
                        route.request.id.get(),
                        event.sequence
                    ));
                }

                let binding = ToolProviderEndpointBinding::new(provider_id, endpoint_id);
                provider.endpoint_binding = Some(binding);
                provider.endpoint_bound_sequence = Some(event.sequence);
                provider.last_sequence = event.sequence;
                endpoint_owner.insert(endpoint_id, provider_id);
            }
            _ => {}
        }
    }

    let mut records = providers
        .into_values()
        .map(ReplayProvider::into_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.registered_sequence);
    Ok(records)
}

struct ReplayProvider {
    provider_id: ToolProviderId,
    name: ToolProviderName,
    registered_sequence: u64,
    endpoint_binding: Option<ToolProviderEndpointBinding>,
    endpoint_bound_sequence: Option<u64>,
    last_sequence: u64,
}

impl ReplayProvider {
    fn into_record(self) -> ToolProviderAuditRecord {
        ToolProviderAuditRecord {
            provider_id: self.provider_id,
            name: self.name,
            registered_sequence: self.registered_sequence,
            endpoint_binding: self.endpoint_binding,
            endpoint_bound_sequence: self.endpoint_bound_sequence,
            last_sequence: self.last_sequence,
        }
    }
}

#[must_use]
pub fn provider_scope(provider_id: ToolProviderId) -> String {
    format!("tool-provider:{}", provider_id.get())
}

#[must_use]
pub fn provider_endpoint_scope(
    provider_id: ToolProviderId,
    endpoint_id: RouteEndpointId,
) -> String {
    format!(
        "tool-provider-endpoint:{}:{}",
        provider_id.get(),
        endpoint_id.get()
    )
}

fn append_typed(
    store: &mut impl EventStore,
    scope: Option<String>,
    kind: EventKind,
    payload: Value,
) -> std::io::Result<u64> {
    let encoded = serde_json::to_string(&payload).map_err(invalid_data)?;
    store.append_scoped(scope, kind, encoded)
}

fn typed_payload(event: &EventEnvelope, expected_record: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(&event.payload).map_err(|error| {
        format!(
            "malformed tool provider payload at sequence {}: {error}",
            event.sequence
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(format!(
            "tool provider event at sequence {} has missing/unsupported schema",
            event.sequence
        ));
    }
    if value.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "tool provider event at sequence {} has missing/unsupported version",
            event.sequence
        ));
    }
    if value.get("record").and_then(Value::as_str) != Some(expected_record) {
        return Err(format!(
            "tool provider event at sequence {} has unexpected record",
            event.sequence
        ));
    }
    Ok(value)
}

fn validate_scope(event: &EventEnvelope, expected: &str) -> Result<(), String> {
    if event.scope.as_deref() != Some(expected) {
        return Err(format!(
            "tool provider event at sequence {} has scope {:?}, expected {:?}",
            event.sequence, event.scope, expected
        ));
    }
    Ok(())
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("typed tool provider payload is missing integer field '{field}'"))
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("typed tool provider payload is missing string field '{field}'"))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::routing_audit::record_route_proposed;
    use crate::session_audit::{
        record_local_session_registered, record_session_endpoint_bound,
    };
    use chatarium_core::routing::{RouteClass, RouteId, RoutePolicy, RouteRequest};
    use chatarium_core::session::{SessionEndpointBinding, SessionId};

    #[test]
    fn registration_and_endpoint_binding_replay() {
        let mut store = MemoryEventStore::default();
        let provider = ToolProviderId::new(1);
        let endpoint = RouteEndpointId::new(10);
        record_tool_provider_registered(
            &mut store,
            provider,
            &ToolProviderName::new("local-files").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(provider, endpoint),
        )
        .unwrap();

        let records = replay_tool_provider_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].provider_id, provider);
        assert_eq!(records[0].name.as_str(), "local-files");
        assert_eq!(
            records[0].endpoint_binding,
            Some(ToolProviderEndpointBinding::new(provider, endpoint))
        );
    }

    #[test]
    fn provider_endpoint_cannot_collide_with_session_endpoint() {
        let mut store = MemoryEventStore::default();
        let endpoint = RouteEndpointId::new(10);
        record_local_session_registered(&mut store, SessionId::new(1)).unwrap();
        record_session_endpoint_bound(
            &mut store,
            SessionEndpointBinding::new(SessionId::new(1), endpoint),
        )
        .unwrap();
        record_tool_provider_registered(
            &mut store,
            ToolProviderId::new(1),
            &ToolProviderName::new("collision").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(ToolProviderId::new(1), endpoint),
        )
        .unwrap();

        assert!(
            replay_tool_provider_audit(store.events())
                .unwrap_err()
                .contains("collides with session")
        );
    }

    #[test]
    fn provider_endpoint_cannot_retroactively_claim_route_history() {
        let mut store = MemoryEventStore::default();
        let endpoint = RouteEndpointId::new(30);
        let request = RouteRequest {
            id: RouteId::new(1),
            source: RouteEndpointId::new(10),
            destination: endpoint,
            class: RouteClass::ToolCall,
        };
        record_route_proposed(&mut store, request, RoutePolicy::RequireApproval).unwrap();
        record_tool_provider_registered(
            &mut store,
            ToolProviderId::new(1),
            &ToolProviderName::new("late").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(ToolProviderId::new(1), endpoint),
        )
        .unwrap();

        assert!(
            replay_tool_provider_audit(store.events())
                .unwrap_err()
                .contains("retroactively claim")
        );
    }

    #[test]
    fn provider_and_endpoint_are_one_to_one() {
        let mut duplicate_provider = MemoryEventStore::default();
        let provider = ToolProviderId::new(1);
        record_tool_provider_registered(
            &mut duplicate_provider,
            provider,
            &ToolProviderName::new("one").unwrap(),
        )
        .unwrap();
        record_tool_provider_registered(
            &mut duplicate_provider,
            provider,
            &ToolProviderName::new("two").unwrap(),
        )
        .unwrap();
        assert!(
            replay_tool_provider_audit(duplicate_provider.events())
                .unwrap_err()
                .contains("duplicate tool provider")
        );

        let mut duplicate_endpoint = MemoryEventStore::default();
        for (id, name) in [(1, "one"), (2, "two")] {
            record_tool_provider_registered(
                &mut duplicate_endpoint,
                ToolProviderId::new(id),
                &ToolProviderName::new(name).unwrap(),
            )
            .unwrap();
        }
        let endpoint = RouteEndpointId::new(40);
        record_tool_provider_endpoint_bound(
            &mut duplicate_endpoint,
            ToolProviderEndpointBinding::new(ToolProviderId::new(1), endpoint),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut duplicate_endpoint,
            ToolProviderEndpointBinding::new(ToolProviderId::new(2), endpoint),
        )
        .unwrap();
        assert!(
            replay_tool_provider_audit(duplicate_endpoint.events())
                .unwrap_err()
                .contains("already belongs to provider")
        );
    }
}
