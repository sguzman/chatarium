//! One deliberately tiny, side-effect-free builtin tool adapter.
//!
//! This is not an arbitrary MCP host. It implements only the original shim's
//! `hello -> {"message":"hello"}` smoke operation, after the separate
//! persistence worker has validated the manually approved routing boundary.

use chatarium_protocol::tool_envelope::{
    MAX_ENVELOPE_BYTES, format_legacy_tool_success, parse_legacy_tool_call,
};
use chatarium_store::tool_call_audit::ToolCallAuditRecord;
use chatarium_store::tool_provider_audit::replay_tool_provider_audit;
use chatarium_store::tool_transport_config_audit::replay_tool_transport_config_audit;
use chatarium_store::tool_provider_activation_audit::replay_tool_provider_activation_audit;
use chatarium_store::EventEnvelope;
use serde_json::{Value, json};

pub const BUILTIN_PROVIDER_NAME: &str = "chatarium.builtin";
pub const HELLO_OPERATION: &str = "hello";
pub const TOOLS_LIST_OPERATION: &str = "tools.list";
pub const MAX_DIRECTORY_OPERATIONS: usize = 128;

/// Whether an inert registered provider/call is eligible for the restricted
/// builtin hello adapter. This is not routing permission.
#[must_use]
pub fn supports_hello(provider_name: &str, operation: &str) -> bool {
    provider_name == BUILTIN_PROVIDER_NAME && operation == HELLO_OPERATION
}

/// Side-effect-free builtins supported by the *existing* user-approved,
/// durably dispatched local adapter. This does not grant automatic permission.
#[must_use]
pub fn supports_builtin(provider_name: &str, operation: &str) -> bool {
    provider_name == BUILTIN_PROVIDER_NAME
        && matches!(operation, HELLO_OPERATION | TOOLS_LIST_OPERATION)
}

/// Create a bounded local MCP directory from replayed provider configuration.
///
/// The directory is *informational*: activation and an operation allowlist
/// do not grant execution authority. Descriptions are factual only for
/// known builtins/demo tools; other operations are marked as undescribed
/// rather than inventing provider-advertised metadata.
pub fn local_tool_directory(events: &[EventEnvelope]) -> Result<Value, String> {
    let providers = replay_tool_provider_audit(events)?;
    let configs = replay_tool_transport_config_audit(events)?;
    let activation = replay_tool_provider_activation_audit(events)?;
    let mut tools = Vec::new();
    for (name, description) in [
        (HELLO_OPERATION, "Return a deterministic greeting"),
        (TOOLS_LIST_OPERATION, "List locally registered MCP provider tools and their descriptions"),
    ] {
        tools.push(json!({
            "provider": BUILTIN_PROVIDER_NAME,
            "name": name,
            "description": description,
            "available": true,
            "execution_permission": "separate durable approval required"
        }));
    }
    for provider in providers {
        if provider.name.as_str() == BUILTIN_PROVIDER_NAME {
            continue;
        }
        let Some(config) = configs.iter().find(|c| c.provider_id == provider.provider_id) else {
            continue;
        };
        let active = activation.iter()
            .find(|state| state.provider_id == provider.provider_id)
            .is_some_and(|state| state.active);
        for op in config.config.allowed_operations() {
            if tools.len() >= MAX_DIRECTORY_OPERATIONS {
                return Err("MCP directory exceeds the maximum of 128 operations".to_owned());
            }
            let description = match (provider.name.as_str(), op.as_str()) {
                ("chatarium.demo", "hello") => "Return a deterministic greeting",
                ("chatarium.demo", "time.now") => "Read the current UTC time",
                ("chatarium.demo", "shell.command") => "Run one restricted sandbox demonstration command",
                (_, "chatarium.internal.tools-list") => "Inspect this provider's actual tools/list catalog",
                _ => "Description not yet verified; inspect this provider's tools/list catalog",
            };
            tools.push(json!({
                "provider": provider.name.as_str(),
                "name": op.as_str(),
                "description": description,
                "available": active,
                "execution_permission": "separate durable approval required"
            }));
        }
    }
    Ok(json!({"tools": tools, "authority": "discovery only, not permission to run"}))
}

/// Validate and prepare a builtin result before consuming its route permit.
pub fn prepare_builtin_response(
    call: &ToolCallAuditRecord,
    events: &[EventEnvelope],
) -> Result<String, String> {
    if call.operation.as_str() == HELLO_OPERATION {
        return prepare_hello_response(call);
    }
    if call.operation.as_str() != TOOLS_LIST_OPERATION {
        return Err("unsupported builtin tool".to_owned());
    }
    let input = call.arguments_text.trim();
    let (legacy_id, arguments): (String, Value) = if input.starts_with("<tool_call") {
        let parsed = parse_legacy_tool_call(input)
            .map_err(|error| format!("invalid legacy tool directory envelope: {error:?}"))?;
        if parsed.name != TOOLS_LIST_OPERATION {
            return Err("tool directory envelope names another operation".to_owned());
        }
        (parsed.id, parsed.arguments)
    } else {
        let args = serde_json::from_str::<Value>(input)
            .map_err(|error| format!("invalid tool directory arguments: {error}"))?;
        (format!("call_{}", call.call_id.get()), args)
    };
    if arguments != json!({}) {
        return Err("tools.list requires exactly an empty JSON object".to_owned());
    }
    let directory = local_tool_directory(events)?;
    format_legacy_tool_success(&legacy_id, TOOLS_LIST_OPERATION, directory)
        .map_err(|error| format!("cannot format builtin directory result: {error:?}"))
}

/// Prepare the canonical legacy tool result for the side-effect-free hello.
///
/// Parsing/formatting happen *before* durable dispatch so malformed arguments
/// cannot consume a permit. The returned text does not itself assert dispatch
/// or provenance; those are independently recorded by the route/tool audits.
pub fn prepare_hello_response(call: &ToolCallAuditRecord) -> Result<String, String> {
    if call.operation.as_str() != HELLO_OPERATION {
        return Err(format!(
            "builtin hello cannot run operation {}",
            call.operation.as_str()
        ));
    }
    if call.arguments_text.len() > MAX_ENVELOPE_BYTES {
        return Err("builtin hello arguments exceed bounded envelope size".to_owned());
    }

    let input = call.arguments_text.trim();
    let (legacy_id, arguments): (String, Value) = if input.starts_with("<tool_call") {
        let parsed = parse_legacy_tool_call(input)
            .map_err(|error| format!("invalid legacy hello envelope: {error:?}"))?;
        if parsed.name != HELLO_OPERATION {
            return Err(format!(
                "legacy envelope names operation {}, not hello",
                parsed.name
            ));
        }
        (parsed.id, parsed.arguments)
    } else {
        let arguments = if input.is_empty() {
            json!({})
        } else {
            serde_json::from_str(input)
                .map_err(|error| format!("invalid builtin hello JSON arguments: {error}"))?
        };
        (format!("call_{}", call.call_id.get()), arguments)
    };
    if arguments != json!({}) {
        return Err("builtin hello requires exactly an empty JSON object".to_owned());
    }

    format_legacy_tool_success(&legacy_id, HELLO_OPERATION, json!({"message":"hello"}))
        .map_err(|error| format!("cannot format builtin hello result: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::session::SessionId;
    use chatarium_core::tool::{ToolCallId, ToolOperationName, ToolProviderId};
    use chatarium_protocol::tool_envelope::parse_legacy_tool_result;

    fn call(operation: &str, text: &str) -> ToolCallAuditRecord {
        ToolCallAuditRecord {
            call_id: ToolCallId::new(4),
            source_session_id: SessionId::new(5),
            provider_id: ToolProviderId::new(6),
            operation: ToolOperationName::new(operation).unwrap(),
            arguments_text: text.to_owned(),
            recorded_sequence: 7,
            route_id: None,
            route_bound_sequence: None,
        }
    }

    #[test]
    fn preserves_legacy_xml_correlation_and_exact_shim_hello_payload() {
        let call = call(
            "hello",
            "<tool_call id=\"legacy-42\" name=\"hello\">\n{}\n</tool_call>",
        );
        let response = prepare_hello_response(&call).unwrap();
        let parsed = parse_legacy_tool_result(&response).unwrap();
        assert_eq!(parsed.id, "legacy-42");
        assert_eq!(parsed.name, "hello");
        assert_eq!(parsed.payload["ok"], true);
        assert_eq!(parsed.payload["message"], "hello");
    }

    #[test]
    fn plain_json_uses_local_numeric_call_correlation() {
        let response = prepare_hello_response(&call("hello", "{}")).unwrap();
        let parsed = parse_legacy_tool_result(&response).unwrap();
        assert_eq!(parsed.id, "call_4");
        assert_eq!(parsed.payload["message"], "hello");
    }

    #[test]
    fn rejects_nonempty_arguments_and_wrong_operation() {
        assert!(prepare_hello_response(&call("hello", "{\"path\":\"/home\"}")).is_err());
        assert!(prepare_hello_response(&call("hello", "[1]")).is_err());
        assert!(prepare_hello_response(&call("clock", "{}")).is_err());
        assert!(
            prepare_hello_response(&call(
                "hello",
                "<tool_call name=\"clock.now\">{}</tool_call>"
            ))
            .is_err()
        );
        assert!(prepare_hello_response(&call("hello", "garbage")).is_err());
    }

    #[test]
    fn only_explicit_builtin_provider_name_is_eligible() {
        assert!(supports_hello("chatarium.builtin", "hello"));
        assert!(!supports_hello("custom.provider", "hello"));
        assert!(!supports_hello("chatarium.builtin", "clock.now"));
    }
    #[test]
    fn tools_list_is_bounded_and_does_not_infer_execution_permission() {
        use chatarium_store::MemoryEventStore;
        use chatarium_store::EventStore;
        let store = MemoryEventStore::default();
        let response = prepare_builtin_response(&call("tools.list", "{}"), store.events()).unwrap();
        let parsed = parse_legacy_tool_result(&response).unwrap();
        assert_eq!(parsed.name, "tools.list");
        assert_eq!(parsed.payload["ok"], true);
        assert_eq!(parsed.payload["tools"][0]["name"], "hello");
        assert_eq!(parsed.payload["tools"][1]["name"], "tools.list");
        assert_eq!(
            parsed.payload["tools"][1]["execution_permission"],
            "separate durable approval required"
        );
        assert_eq!(parsed.payload["authority"], "discovery only, not permission to run");
        assert!(prepare_builtin_response(&call("tools.list", r#"{"x":1}"#), store.events()).is_err());
        assert!(!supports_builtin("chatarium.demo", "tools.list"));
    }

}
