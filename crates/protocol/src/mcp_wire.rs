//! Bounded MCP 2026-07-28 JSON-RPC wire representation.
//!
//! This is a *pure codec*, not a provider transport or execution engine.
//! It neither launches local servers nor performs filesystem/network activity.
//! An MCP request is never dispatch authority: Chatarium's durable ToolCall
//! route, explicit user Allow, and one-shot DispatchPermit remain separate.
//!
//! Specification: https://modelcontextprotocol.io/specification/2026-07-28
//! This version has no initialize/initialized handshake or protocol sessions.
//! Every request carries version and client capabilities in params._meta.
//! The legacy ChatGPT Tool Shim XML envelope is handled by tool_envelope.rs.

use serde_json::{Value, json};

pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const MAX_MCP_FRAME_BYTES: usize = 1_048_576;
pub const MAX_MCP_ARGUMENT_BYTES: usize = 65_536;
pub const MAX_MCP_TOOL_NAME_SCALARS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpWireError {
    Empty,
    TooLarge,
    InvalidFrame,
    InvalidJson,
    InvalidRequest,
    InvalidToolName,
    InvalidArguments,
    MismatchedId,
    InvalidResponse,
    /// MCP may require another user/client interaction. Never auto-resubmit.
    InputRequiredUnsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpResponse {
    Complete(Value),
    Error {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}

/// Create a discovery request for explicit capability inspection.
///
/// This is not a replacement for Chatarium's own user permission checks.
pub fn discover_request(id: u64) -> Value {
    request(id, "server/discover", json!({}))
}

/// Create a bounded tools/list request, optionally continuing cursor pagination.
///
/// Cursor text is retained exactly, not interpreted as route permission.
pub fn tools_list_request(id: u64, cursor: Option<&str>) -> Result<Value, McpWireError> {
    if cursor.is_some_and(|value| value.is_empty() || value.len() > MAX_MCP_ARGUMENT_BYTES) {
        return Err(McpWireError::InvalidArguments);
    }
    let mut params = json!({});
    if let Some(cursor) = cursor {
        params["cursor"] = Value::String(cursor.to_owned());
    }
    Ok(request(id, "tools/list", params))
}

/// Encode one tools/call request. Arguments must be a bounded JSON object.
/// This step performs zero execution and consumes no Chatarium route permit.
pub fn tools_call_request(
    id: u64,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value, McpWireError> {
    let scalar_count = tool_name.chars().count();
    if tool_name.is_empty()
        || tool_name.trim() != tool_name
        || scalar_count > MAX_MCP_TOOL_NAME_SCALARS
        || tool_name.chars().any(char::is_control)
    {
        return Err(McpWireError::InvalidToolName);
    }
    if !arguments.is_object() {
        return Err(McpWireError::InvalidArguments);
    }
    let args = serde_json::to_vec(arguments).map_err(|_| McpWireError::InvalidArguments)?;
    if args.len() > MAX_MCP_ARGUMENT_BYTES {
        return Err(McpWireError::TooLarge);
    }
    Ok(request(
        id,
        "tools/call",
        json!({
            "name": tool_name,
            "arguments": arguments,
        }),
    ))
}

/// One line-delimited UTF-8 JSON-RPC frame for a reliable bidirectional stream.
///
/// JSON serialization escapes embedded line breaks. Exactly one frame is emitted;
/// no stdout logging, shell quoting, or subprocess behavior is involved.
pub fn encode_stdio_frame(message: &Value) -> Result<String, McpWireError> {
    let obj = message.as_object().ok_or(McpWireError::InvalidRequest)?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || obj.get("id").and_then(Value::as_u64).is_none()
        || obj.get("method").and_then(Value::as_str).is_none()
        || obj.get("params").and_then(Value::as_object).is_none()
    {
        return Err(McpWireError::InvalidRequest);
    }
    let json = serde_json::to_string(message).map_err(|_| McpWireError::InvalidJson)?;
    if json.len() + 1 > MAX_MCP_FRAME_BYTES {
        return Err(McpWireError::TooLarge);
    }
    Ok(format!("{json}\n"))
}

/// Decode one response to the exact numeric request ID.
///
/// Batch responses, unsolicited requests, notifications, mismatched IDs,
/// multi-frame input, malformed JSON, and unknown result types fail closed.
/// In-band multi-round-trip `inputRequired` is explicit unsupported state: it
/// must never trigger an automatic additional tool invocation.
pub fn decode_stdio_response(frame: &str, expected_id: u64) -> Result<McpResponse, McpWireError> {
    if frame.is_empty() {
        return Err(McpWireError::Empty);
    }
    if frame.len() > MAX_MCP_FRAME_BYTES {
        return Err(McpWireError::TooLarge);
    }
    let raw = frame.strip_suffix('\n').unwrap_or(frame);
    if raw.is_empty() || raw.contains('\n') || raw.contains('\r') {
        return Err(McpWireError::InvalidFrame);
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| McpWireError::InvalidJson)?;
    let obj = value.as_object().ok_or(McpWireError::InvalidResponse)?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpWireError::InvalidResponse);
    }
    if obj.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Err(McpWireError::MismatchedId);
    }

    match (obj.get("result"), obj.get("error")) {
        (Some(Value::Object(result)), None) => {
            match result.get("resultType").and_then(Value::as_str) {
                Some("complete") => Ok(McpResponse::Complete(Value::Object(result.clone()))),
                Some("inputRequired") => Err(McpWireError::InputRequiredUnsupported),
                _ => Err(McpWireError::InvalidResponse),
            }
        }
        (None, Some(Value::Object(error))) => {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or(McpWireError::InvalidResponse)?;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or(McpWireError::InvalidResponse)?;
            Ok(McpResponse::Error {
                code,
                message: message.to_owned(),
                data: error.get("data").cloned(),
            })
        }
        _ => Err(McpWireError::InvalidResponse),
    }
}

fn request(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MCP_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {
            "name": "chatarium",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stateless_2026_metadata_is_present_per_request() {
        for message in [
            discover_request(1),
            tools_list_request(2, None).unwrap(),
            tools_call_request(3, "hello", &json!({})).unwrap(),
        ] {
            assert_eq!(message["jsonrpc"], "2.0");
            assert_eq!(
                message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                MCP_PROTOCOL_VERSION,
            );
            assert!(
                message["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                    .is_object()
            );
            assert_eq!(
                message["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
                "chatarium"
            );
            assert!(message["params"].get("protocolVersion").is_none());
        }
    }

    #[test]
    fn tool_call_preserves_exact_json_and_newline_framing() {
        let args = json!({"path": "a\nb", "number": 42});
        let request = tools_call_request(17, "local.read", &args).unwrap();
        assert_eq!(request["params"]["arguments"], args);
        assert_eq!(request["params"]["name"], "local.read");
        let frame = encode_stdio_frame(&request).unwrap();
        assert!(frame.ends_with('\n'));
        assert_eq!(frame.matches('\n').count(), 1);
        assert!(frame.contains(r#"a\nb"#));
    }

    #[test]
    fn successful_and_failed_responses_preserve_exact_correlated_data() {
        let success = r#"{"jsonrpc":"2.0","id":8,"result":{"resultType":"complete","content":[{"type":"text","text":"hello"}]}}"#;
        let McpResponse::Complete(value) = decode_stdio_response(success, 8).unwrap() else {
            panic!("expected complete")
        };
        assert_eq!(value["content"][0]["text"], "hello");

        let error = r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32602,"message":"invalid args","data":{"field":"path"}}}"#;
        assert_eq!(
            decode_stdio_response(error, 9).unwrap(),
            McpResponse::Error {
                code: -32602,
                message: "invalid args".to_owned(),
                data: Some(json!({"field":"path"})),
            }
        );
    }

    #[test]
    fn cannot_confuse_completion_with_multiple_round_trips() {
        let input_required = r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"inputRequired","inputRequests":{}}}"#;
        assert_eq!(
            decode_stdio_response(input_required, 1),
            Err(McpWireError::InputRequiredUnsupported),
        );
    }

    #[test]
    fn rejects_ambiguous_or_malicious_frames() {
        let complete = r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete"}}"#;
        assert_eq!(
            decode_stdio_response(complete, 2),
            Err(McpWireError::MismatchedId),
        );
        assert_eq!(
            decode_stdio_response(&format!("{complete}\n{complete}\n"), 1),
            Err(McpWireError::InvalidFrame),
        );
        assert_eq!(
            decode_stdio_response("[]", 1),
            Err(McpWireError::InvalidResponse),
        );
        assert_eq!(
            decode_stdio_response(
                r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"unknown"}}"#,
                1
            ),
            Err(McpWireError::InvalidResponse),
        );
        assert_eq!(
            decode_stdio_response(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#, 1),
            Err(McpWireError::InvalidResponse),
        );
        assert_eq!(decode_stdio_response("", 1), Err(McpWireError::Empty),);
        assert!(tools_call_request(1, "bad\nname", &json!({})).is_err());
        assert!(tools_call_request(1, "good", &json!([1])).is_err());
    }

    #[test]
    fn request_and_response_boundaries_reject_oversized_input() {
        let oversized = json!({"text": "x".repeat(MAX_MCP_ARGUMENT_BYTES)});
        assert_eq!(
            tools_call_request(1, "tool", &oversized),
            Err(McpWireError::TooLarge),
        );
        let oversized_frame = "x".repeat(MAX_MCP_FRAME_BYTES + 1);
        assert_eq!(
            decode_stdio_response(&oversized_frame, 1),
            Err(McpWireError::TooLarge),
        );
    }
}
