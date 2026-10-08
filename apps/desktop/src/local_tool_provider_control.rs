//! Human-directed external stdio configuration and activation controls.
//! These checks run in the persistence worker, never the egui render loop.
//! No subprocess, network request, or RouteGate dispatch is performed.

use chatarium_core::tool::{StdioToolProviderConfig, ToolOperationName, ToolProviderId};
use chatarium_store::tool_provider_activation_audit::{
    ProviderActivationDecision, append_tool_provider_activation_decision_checked,
};
use chatarium_store::tool_stdio_executable_inspection::inspect_stdio_executable;
use chatarium_store::tool_transport_config_audit::replay_tool_transport_config_audit;
use chatarium_store::{EventEnvelope, EventStore};

/// Each argv line is one exact argument. Never interpret shell syntax.
pub fn parse_stdio_config_draft(
    executable: &str,
    argv_lines: &str,
    operation_lines: &str,
) -> Result<StdioToolProviderConfig, String> {
    let argv = argv_lines
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let operations = operation_lines
        .lines()
        .map(|name| {
            ToolOperationName::new(name)
                .map_err(|error| format!("invalid allowed operation {name:?}: {error:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    StdioToolProviderConfig::new(executable, argv, operations)
        .map_err(|error| format!("invalid Linux stdio configuration: {error:?}"))
}

/// A user activation is not a trust certificate. Inspect the actual Linux path
/// on the persistence worker immediately before recording a decision. A future
/// runner still needs race-aware launch checks and one-shot dispatch authority.
pub fn append_activation_with_inspection(
    store: &mut impl EventStore,
    provider_id: ToolProviderId,
    configured_sequence: u64,
    decision: ProviderActivationDecision,
) -> Result<EventEnvelope, String> {
    if decision == ProviderActivationDecision::Activate {
        let configured = replay_tool_transport_config_audit(store.events())?
            .into_iter()
            .find(|record| record.provider_id == provider_id)
            .ok_or_else(|| "provider has no immutable stdio configuration".to_owned())?;
        if configured.configured_sequence != configured_sequence {
            return Err("activation references a stale configuration sequence".to_owned());
        }
        inspect_stdio_executable(&configured.config).map_err(|error| {
            format!("Linux executable inspection rejected activation: {error:?}")
        })?;
    }
    append_tool_provider_activation_decision_checked(
        store,
        provider_id,
        configured_sequence,
        decision,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::EventKind;
    use chatarium_core::routing::RouteEndpointId;
    use chatarium_core::tool::{ToolProviderEndpointBinding, ToolProviderName};
    use chatarium_store::MemoryEventStore;
    use chatarium_store::tool_provider_audit::{
        record_tool_provider_endpoint_bound, record_tool_provider_registered,
    };
    use chatarium_store::tool_transport_config_audit::append_tool_transport_config_checked;

    #[test]
    fn drafts_preserve_exact_argv_and_validate_operations() {
        let config = parse_stdio_config_draft(
            "/usr/bin/example-mcp",
            "--name=two words\n--dry-run",
            "read\nsearch",
        )
        .unwrap();
        assert_eq!(config.args(), &["--name=two words", "--dry-run"]);
        assert_eq!(config.allowed_operations().len(), 2);
        assert!(parse_stdio_config_draft("relative/path", "", "read").is_err());
        assert!(parse_stdio_config_draft("/usr/bin/example", "", "read\nread").is_err());
        assert!(parse_stdio_config_draft("/usr/bin/example", "", " read").is_err());
    }

    #[test]
    fn missing_executable_refuses_activation_without_journal_mutation() {
        let mut store = MemoryEventStore::default();
        let provider = ToolProviderId::new(30);
        record_tool_provider_registered(
            &mut store,
            provider,
            &ToolProviderName::new("external").unwrap(),
        )
        .unwrap();
        record_tool_provider_endpoint_bound(
            &mut store,
            ToolProviderEndpointBinding::new(provider, RouteEndpointId::new(40)),
        )
        .unwrap();
        let config =
            parse_stdio_config_draft("/chatarium-missing-executable-892342/example", "", "read")
                .unwrap();
        let recorded = append_tool_transport_config_checked(&mut store, provider, &config).unwrap();
        let count = store.events().len();
        assert!(
            append_activation_with_inspection(
                &mut store,
                provider,
                recorded.sequence,
                ProviderActivationDecision::Activate,
            )
            .is_err()
        );
        assert_eq!(store.events().len(), count);
        assert!(
            !store
                .events()
                .iter()
                .any(|event| { event.kind == EventKind::ToolProviderActivationDecisionRecorded })
        );
    }
}
