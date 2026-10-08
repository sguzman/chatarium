//! One deliberately tiny, side-effect-free builtin tool adapter.
//!
//! This is not an arbitrary MCP host. It implements only the original shim's
//! `hello -> {"message":"hello"}` smoke operation, after the separate
//! persistence worker has validated the manually approved routing boundary.

use chatarium_protocol::tool_envelope::{
    MAX_ENVELOPE_BYTES, format_legacy_tool_success, parse_legacy_tool_call,
};
use chatarium_store::tool_call_audit::ToolCallAuditRecord;
use serde_json::{Value, json};

pub const BUILTIN_PROVIDER_NAME: &str = "chatarium.builtin";
pub const HELLO_OPERATION: &str = "hello";

/// Whether an inert registered provider/call is eligible for the restricted
/// builtin hello adapter. This is not routing permission.
#[must_use]
pub fn supports_hello(provider_name: &str, operation: &str) -> bool {
    provider_name == BUILTIN_PROVIDER_NAME && operation == HELLO_OPERATION
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
        let call = call("hello", "<tool_call id=\"legacy-42\" name=\"hello\">\n{}\n</tool_call>");
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
        assert!(prepare_hello_response(&call(
            "hello",
            "<tool_call name=\"clock.now\">{}</tool_call>"
        )).is_err());
        assert!(prepare_hello_response(&call("hello", "garbage")).is_err());
    }

    #[test]
    fn only_explicit_builtin_provider_name_is_eligible() {
        assert!(supports_hello("chatarium.builtin", "hello"));
        assert!(!supports_hello("custom.provider", "hello"));
        assert!(!supports_hello("chatarium.builtin", "clock.now"));
    }
}
