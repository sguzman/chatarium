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
pub const MAX_MCP_LIST_TOOLS_PER_PAGE: usize = 128;
pub const MAX_MCP_CALL_CONTENT_BLOCKS: usize = 128;

/// A tool catalogue is external provider testimony, not an execution allowlist.
/// Exact input schema remains untrusted JSON until separately reviewed.
#[derive(Debug, Clone, PartialEq)]
pub struct McpListedTool {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolCatalogPage {
    pub tools: Vec<McpListedTool>,
    pub next_cursor: Option<String>,
}

/// Validate the bounded, correlated MCP 2026 tools/list response body.
/// This does not launch a provider, trust its schemas or change configuration.
/// Additional pages require separate user-reviewed, approved requests.
pub fn parse_tools_list_page(result: &Value) -> Result<McpToolCatalogPage, McpWireError> {
    let object = result.as_object().ok_or(McpWireError::InvalidResponse)?;
    if object.get("resultType").and_then(Value::as_str) != Some("complete") {
        return Err(McpWireError::InvalidResponse);
    }
    let listed = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(McpWireError::InvalidResponse)?;
    if listed.len() > MAX_MCP_LIST_TOOLS_PER_PAGE {
        return Err(McpWireError::TooLarge);
    }
    let next_cursor = match object.get("nextCursor") {
        None => None,
        Some(Value::String(cursor))
            if !cursor.is_empty() && cursor.len() <= MAX_MCP_ARGUMENT_BYTES =>
        {
            Some(cursor.clone())
        }
        _ => return Err(McpWireError::InvalidResponse),
    };
    let mut names = std::collections::BTreeSet::new();
    let mut tools = Vec::with_capacity(listed.len());
    for entry in listed {
        let obj = entry.as_object().ok_or(McpWireError::InvalidResponse)?;
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .ok_or(McpWireError::InvalidResponse)?;
        if name.is_empty()
            || name.trim() != name
            || name.chars().count() > MAX_MCP_TOOL_NAME_SCALARS
            || name.chars().any(char::is_control)
            || !names.insert(name.to_owned())
        {
            return Err(McpWireError::InvalidResponse);
        }
        let input_schema = obj
            .get("inputSchema")
            .filter(|value| value.is_object())
            .ok_or(McpWireError::InvalidResponse)?;
        let title = match obj.get("title") {
            None => None,
            Some(Value::String(value)) if value.len() <= 4096 => Some(value.clone()),
            _ => return Err(McpWireError::InvalidResponse),
        };
        let description = match obj.get("description") {
            None => None,
            Some(Value::String(value)) if value.len() <= 65_536 => Some(value.clone()),
            _ => return Err(McpWireError::InvalidResponse),
        };
        tools.push(McpListedTool {
            name: name.to_owned(),
            title,
            description,
            input_schema: input_schema.clone(),
        });
    }
    Ok(McpToolCatalogPage { tools, next_cursor })
}

/// Structural summary only. It is not outputSchema validation, trusted
/// content, or authority to admit a tool response into model context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpToolCallResultShape {
    pub content_blocks: usize,
    pub has_structured_content: bool,
    pub is_error: bool,
}

/// Check a completed 2026 tools/call body before treating it as a durable
/// successful transport observation. The outer JSON-RPC request ID is checked
/// separately. Unknown content block variants fail closed; extra provider
/// metadata stays untrusted and is never interpreted as instructions.
pub fn validate_tools_call_result(
    result: &Value,
) -> Result<McpToolCallResultShape, McpWireError> {
    let object = result.as_object().ok_or(McpWireError::InvalidResponse)?;
    if object.get("resultType").and_then(Value::as_str) != Some("complete") {
        return Err(McpWireError::InvalidResponse);
    }
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or(McpWireError::InvalidResponse)?;
    if content.len() > MAX_MCP_CALL_CONTENT_BLOCKS {
        return Err(McpWireError::TooLarge);
    }
    let is_error = match object.get("isError") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(McpWireError::InvalidResponse),
    };
    for item in content {
        let block = item.as_object().ok_or(McpWireError::InvalidResponse)?;
        if block
            .get("annotations")
            .is_some_and(|annotations| !annotations.is_object())
        {
            return Err(McpWireError::InvalidResponse);
        }
        let valid = match block.get("type").and_then(Value::as_str) {
            Some("text") => has_string(block, "text"),
            Some("image" | "audio") => {
                has_string(block, "data") && has_nonempty_string(block, "mimeType")
            }
            Some("resource_link") => {
                has_nonempty_string(block, "uri") && has_nonempty_string(block, "name")
            }
            Some("resource") => block
                .get("resource")
                .and_then(Value::as_object)
                .is_some_and(|resource| {
                    has_nonempty_string(resource, "uri")
                        && (has_string(resource, "text") || has_string(resource, "blob"))
                        && resource
                            .get("mimeType")
                            .is_none_or(Value::is_string)
                }),
            _ => false,
        };
        if !valid {
            return Err(McpWireError::InvalidResponse);
        }
    }
    Ok(McpToolCallResultShape {
        content_blocks: content.len(),
        has_structured_content: object.contains_key("structuredContent"),
        is_error,
    })
}

fn has_string(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object.get(key).is_some_and(Value::is_string)
}

fn has_nonempty_string(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

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
    fn tools_call_accepts_all_specified_content_variants_and_structured_scalars() {
        let result = json!({
            "resultType": "complete",
            "content": [
                {"type": "text", "text": ""},
                {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="},
                {"type": "audio", "mimeType": "audio/wav", "data": "aGVsbG8="},
                {"type": "resource_link", "uri": "file:///tmp/example", "name": "example"},
                {"type": "resource", "resource": {"uri": "file:///tmp/a", "text": "text"}},
                {"type": "resource", "resource": {"uri": "file:///tmp/b", "blob": "YWJj"}}
            ],
            "isError": false,
            "structuredContent": [1, true, null]
        });
        assert_eq!(
            validate_tools_call_result(&result).unwrap(),
            McpToolCallResultShape {
                content_blocks: 6,
                has_structured_content: true,
                is_error: false
            }
        );
        let tool_error = json!({
            "resultType": "complete",
            "content": [{"type":"text","text":"invalid argument"}],
            "isError": true
        });
        let shape = validate_tools_call_result(&tool_error).unwrap();
        assert!(shape.is_error);
        assert!(!shape.has_structured_content);
        assert_eq!(
            validate_tools_call_result(&json!({"resultType":"complete","content":[]})).unwrap(),
            McpToolCallResultShape {
                content_blocks: 0,
                has_structured_content: false,
                is_error: false
            }
        );
    }

    #[test]
    fn malformed_completed_tool_results_fail_closed() {
        for result in [
            json!({"resultType":"complete"}),
            json!({"resultType":"complete","content":{}}),
            json!({"resultType":"complete","content":[null]}),
            json!({"resultType":"complete","content":[{"type":"text"}]}),
            json!({"resultType":"complete","content":[{"type":"image","data":"AA=="}]}),
            json!({"resultType":"complete","content":[{"type":"audio","data":"AA==","mimeType":3}]}),
            json!({"resultType":"complete","content":[{"type":"resource_link","uri":"x"}]}),
            json!({"resultType":"complete","content":[{"type":"resource","resource":{"uri":"x"}}]}),
            json!({"resultType":"complete","content":[{"type":"unknown","text":"x"}]}),
            json!({"resultType":"complete","content":[{"type":"text","text":"x","annotations":false}]}),
            json!({"resultType":"complete","content":[],"isError":"false"}),
            json!({"resultType":"input_required","content":[]}),
        ] {
            assert_eq!(
                validate_tools_call_result(&result),
                Err(McpWireError::InvalidResponse)
            );
        }
        let too_many = json!({
            "resultType": "complete",
            "content": vec![json!({"type":"text","text":"x"}); MAX_MCP_CALL_CONTENT_BLOCKS + 1]
        });
        assert_eq!(
            validate_tools_call_result(&too_many),
            Err(McpWireError::TooLarge)
        );
    }

    #[test]
    fn tools_list_catalog_requires_explicit_valid_page_and_cursor() {
        let result = json!({
            "resultType": "complete",
            "tools": [
                {"name":"weather.read","title":"Weather","description":"Read weather",
                 "inputSchema":{"type":"object","properties":{"city":{"type":"string"}}}},
                {"name":"hello","inputSchema":{"type":"object"}}
            ],
            "nextCursor":"second-page"
        });
        let page = parse_tools_list_page(&result).unwrap();
        assert_eq!(page.tools.len(), 2);
        assert_eq!(page.tools[0].name, "weather.read");
        assert_eq!(page.tools[0].input_schema["type"], "object");
        assert_eq!(page.next_cursor.as_deref(), Some("second-page"));
        assert!(
            parse_tools_list_page(&json!({"resultType":"complete","tools":[
                {"name":"same","inputSchema":{}},{"name":"same","inputSchema":{}}
            ]}))
            .is_err()
        );
        assert!(
            parse_tools_list_page(&json!({"resultType":"complete","tools":[
                {"name":"unsafe","inputSchema":null}
            ]}))
            .is_err()
        );
        assert!(
            parse_tools_list_page(&json!({"resultType":"complete","tools":[],
            "nextCursor":""}))
            .is_err()
        );
        assert!(parse_tools_list_page(&json!({"resultType":"input_required","tools":[]})).is_err());
    }

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
