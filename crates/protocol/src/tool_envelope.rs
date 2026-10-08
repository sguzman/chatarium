//! Compatibility parser/formatter for the original ChatGPT Tool Shim envelope.
//!
//! Evidence: sguzman/chatgpt-tool-shim, src/protocol/parse_tool_calls.ts,
//! format_tool_result.ts, types.ts and test/protocol.test.ts (2026-05-08).
//! This is the older XML-like pseudo-MCP envelope, NOT native MCP or a ChatGPT
//! protocol claim. Parsing creates no durable tool call and grants no authority.
//!
//! We intentionally accept a conservative, canonical subset of the shim's
//! permissive attribute regex and fenced-code stripping.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Conservative size boundary for a single legacy envelope.
pub const MAX_ENVELOPE_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyToolCall {
    /// Explicit legacy id or shim-compatible FNV-1a fallback.
    pub id: String,
    pub name: String,
    pub arguments: Value,
    /// Exact trimmed envelope for local audit/debugging.
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyToolResult {
    pub id: String,
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyEnvelopeError {
    Empty,
    TooLarge,
    UnexpectedEnvelope,
    InvalidAttributes,
    MissingName,
    MissingId,
    MalformedJson,
    InvalidResult,
}

/// Parse exactly one complete canonical legacy `<tool_call>` envelope.
///
/// Like the shim, reject surrounding prose, incomplete tags, malformed JSON,
/// and fenced examples. Unlike its regex, reject duplicate/unknown attributes
/// and do not strip arbitrary fenced blocks to discover executable calls.
pub fn parse_legacy_tool_call(text: &str) -> Result<LegacyToolCall, LegacyEnvelopeError> {
    let (attributes, body, raw) = parse_element(text, "tool_call")?;
    let name = attributes
        .get("name")
        .ok_or(LegacyEnvelopeError::MissingName)?;
    validate_attribute_value(name).map_err(|_| LegacyEnvelopeError::MissingName)?;
    if attributes.keys().any(|key| key != "name" && key != "id") {
        return Err(LegacyEnvelopeError::InvalidAttributes);
    }
    let arguments = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(body.trim()).map_err(|_| LegacyEnvelopeError::MalformedJson)?
    };
    let id = match attributes.get("id") {
        Some(id) if !id.trim().is_empty() => {
            validate_attribute_value(id).map_err(|_| LegacyEnvelopeError::InvalidAttributes)?;
            id.clone()
        }
        _ => shim_hash_id(&format!("{}:{}", name, body.trim())),
    };
    Ok(LegacyToolCall {
        id,
        name: name.clone(),
        arguments,
        raw: raw.to_owned(),
    })
}

/// Parse an adapter-provided legacy `<tool_result>` envelope.
///
/// The old shim defined the formatter rather than a strict result parser; this
/// reader is the conservative counterpart for response correlation.
pub fn parse_legacy_tool_result(text: &str) -> Result<LegacyToolResult, LegacyEnvelopeError> {
    let (attributes, body, _) = parse_element(text, "tool_result")?;
    if attributes.keys().any(|key| key != "name" && key != "id") {
        return Err(LegacyEnvelopeError::InvalidAttributes);
    }
    let name = attributes
        .get("name")
        .ok_or(LegacyEnvelopeError::MissingName)?;
    validate_attribute_value(name).map_err(|_| LegacyEnvelopeError::MissingName)?;
    let id = attributes.get("id").ok_or(LegacyEnvelopeError::MissingId)?;
    validate_attribute_value(id).map_err(|_| LegacyEnvelopeError::MissingId)?;
    let payload: Value =
        serde_json::from_str(body.trim()).map_err(|_| LegacyEnvelopeError::MalformedJson)?;
    let object = payload
        .as_object()
        .ok_or(LegacyEnvelopeError::InvalidResult)?;
    match object.get("ok").and_then(Value::as_bool) {
        Some(true) if !object.contains_key("error") => {}
        Some(false) => {
            let error = object
                .get("error")
                .and_then(Value::as_object)
                .ok_or(LegacyEnvelopeError::InvalidResult)?;
            for field in ["code", "message"] {
                if !error
                    .get(field)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    return Err(LegacyEnvelopeError::InvalidResult);
                }
            }
        }
        _ => return Err(LegacyEnvelopeError::InvalidResult),
    }
    Ok(LegacyToolResult {
        id: id.clone(),
        name: name.clone(),
        payload,
    })
}

/// Render the shim's result wrapper with JSON payload and an explicit `ok`.
///
/// Success objects are flattened as in the original `formatToolResult`.
/// A success value other than a JSON object, or one containing an `ok`
/// override, is rejected rather than coercing unsafe JS spread behavior.
pub fn format_legacy_tool_success(
    id: &str,
    name: &str,
    result: Value,
) -> Result<String, LegacyEnvelopeError> {
    let fields = result
        .as_object()
        .ok_or(LegacyEnvelopeError::InvalidResult)?;
    if fields.contains_key("ok") {
        return Err(LegacyEnvelopeError::InvalidResult);
    }
    let mut payload = Map::new();
    payload.insert("ok".to_owned(), Value::Bool(true));
    payload.extend(fields.clone());
    format_result_envelope(id, name, Value::Object(payload))
}

/// Render the shim's explicit error `{ok:false,error:{code,message}}`.
pub fn format_legacy_tool_error(
    id: &str,
    name: &str,
    code: &str,
    message: &str,
) -> Result<String, LegacyEnvelopeError> {
    if code.is_empty() || message.is_empty() {
        return Err(LegacyEnvelopeError::InvalidResult);
    }
    format_result_envelope(
        id,
        name,
        json!({"ok":false,"error":{"code":code,"message":message}}),
    )
}

fn format_result_envelope(
    id: &str,
    name: &str,
    payload: Value,
) -> Result<String, LegacyEnvelopeError> {
    validate_attribute_value(id).map_err(|_| LegacyEnvelopeError::MissingId)?;
    validate_attribute_value(name).map_err(|_| LegacyEnvelopeError::MissingName)?;
    let body =
        serde_json::to_string_pretty(&payload).map_err(|_| LegacyEnvelopeError::InvalidResult)?;
    let text = format!("<tool_result name=\"{name}\" id=\"{id}\">\n{body}\n</tool_result>");
    if text.len() > MAX_ENVELOPE_BYTES {
        return Err(LegacyEnvelopeError::TooLarge);
    }
    Ok(text)
}

fn parse_element<'a>(
    text: &'a str,
    tag: &str,
) -> Result<(BTreeMap<String, String>, &'a str, &'a str), LegacyEnvelopeError> {
    if text.trim().is_empty() {
        return Err(LegacyEnvelopeError::Empty);
    }
    if text.len() > MAX_ENVELOPE_BYTES {
        return Err(LegacyEnvelopeError::TooLarge);
    }
    let raw = text.trim();
    let prefix = format!("<{tag}");
    let suffix = format!("</{tag}>");
    let rest = raw
        .strip_prefix(prefix.as_str())
        .ok_or(LegacyEnvelopeError::UnexpectedEnvelope)?;
    let rest = rest
        .strip_suffix(suffix.as_str())
        .ok_or(LegacyEnvelopeError::UnexpectedEnvelope)?;
    let tag_end = rest
        .find('>')
        .ok_or(LegacyEnvelopeError::UnexpectedEnvelope)?;
    let attribute_text = &rest[..tag_end];
    if !attribute_text.is_empty()
        && !attribute_text
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        return Err(LegacyEnvelopeError::InvalidAttributes);
    }
    let attributes = parse_attributes(attribute_text)?;
    let body = &rest[tag_end + 1..];
    Ok((attributes, body, raw))
}

fn parse_attributes(text: &str) -> Result<BTreeMap<String, String>, LegacyEnvelopeError> {
    let mut remainder = text;
    let mut attributes = BTreeMap::new();
    loop {
        let trimmed = remainder.trim_start();
        if trimmed.is_empty() {
            return Ok(attributes);
        }
        if trimmed.len() == remainder.len() {
            return Err(LegacyEnvelopeError::InvalidAttributes);
        }
        remainder = trimmed;
        let end = remainder
            .find(|c: char| c.is_whitespace() || c == '=')
            .ok_or(LegacyEnvelopeError::InvalidAttributes)?;
        let key = &remainder[..end];
        if !matches!(key, "name" | "id") {
            return Err(LegacyEnvelopeError::InvalidAttributes);
        }
        remainder = remainder[end..].trim_start();
        remainder = remainder
            .strip_prefix("=\"")
            .ok_or(LegacyEnvelopeError::InvalidAttributes)?;
        let quote = remainder
            .find('"')
            .ok_or(LegacyEnvelopeError::InvalidAttributes)?;
        let value = &remainder[..quote];
        if attributes
            .insert(key.to_owned(), value.to_owned())
            .is_some()
        {
            return Err(LegacyEnvelopeError::InvalidAttributes);
        }
        remainder = &remainder[quote + 1..];
        if !remainder.is_empty() && !remainder.chars().next().is_some_and(char::is_whitespace) {
            return Err(LegacyEnvelopeError::InvalidAttributes);
        }
    }
}

fn validate_attribute_value(value: &str) -> Result<(), ()> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 128
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '&'))
    {
        Err(())
    } else {
        Ok(())
    }
}

/// The shim's original fallback id: FNV-1a 32-bit over JavaScript UTF-16 units.
#[must_use]
pub fn shim_hash_id(value: &str) -> String {
    let mut hash = 2_166_136_261_u32;
    for code_unit in value.encode_utf16() {
        hash ^= u32::from(code_unit);
        hash = hash.wrapping_mul(16_777_619);
    }
    format!("call_{hash:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_original_clock_now_call_and_generates_legacy_id() {
        let call =
            parse_legacy_tool_call("<tool_call name=\"clock.now\">\n{}\n</tool_call>").unwrap();
        assert_eq!(call.name, "clock.now");
        assert_eq!(call.arguments, json!({}));
        assert_eq!(call.id, shim_hash_id("clock.now:{}"));
        assert!(call.id.starts_with("call_"));
    }

    #[test]
    fn preserves_explicit_id_and_json_arguments() {
        let call = parse_legacy_tool_call(
            "  <tool_call id=\"abc-123\" name=\"browser.tab.links\">\n{\"tabId\":123,\"limit\":200}\n</tool_call>  ",
        )
        .unwrap();
        assert_eq!(call.id, "abc-123");
        assert_eq!(call.arguments["tabId"], 123);
        assert_eq!(call.arguments["limit"], 200);
        assert!(call.raw.starts_with("<tool_call"));
        assert!(!call.raw.starts_with(' '));
    }

    #[test]
    fn rejects_extra_prose_fences_and_incomplete_or_duplicate_calls() {
        for input in [
            "Explanation <tool_call name=\"clock.now\">{}</tool_call>",
            "```xml\n<tool_call name=\"clock.now\">{}</tool_call>\n```",
            "<tool_call name=\"clock.now\">{}",
            "<tool_call name=\"clock.now\">{}</tool_call><tool_call name=\"clock.now\">{}</tool_call>",
            "<tool_call name=\"clock.now\" name=\"hello\">{}</tool_call>",
            "<tool_call name=\"clock.now\" other=\"x\">{}</tool_call>",
            "<tool_call name=\"clock.now\">not json</tool_call>",
        ] {
            assert!(
                parse_legacy_tool_call(input).is_err(),
                "unexpected accept: {input}"
            );
        }
    }

    #[test]
    fn empty_body_matches_shim_empty_object_default() {
        assert_eq!(
            parse_legacy_tool_call("<tool_call name=\"hello\"></tool_call>")
                .unwrap()
                .arguments,
            json!({})
        );
    }

    #[test]
    fn success_and_error_round_trip_with_original_tag_names() {
        let success =
            format_legacy_tool_success("call_abc", "clock.now", json!({"now":"2026-05-08"}))
                .unwrap();
        assert!(success.starts_with("<tool_result name=\"clock.now\" id=\"call_abc\">"));
        let parsed = parse_legacy_tool_result(&success).unwrap();
        assert_eq!(parsed.id, "call_abc");
        assert_eq!(parsed.payload["ok"], true);
        assert_eq!(parsed.payload["now"], "2026-05-08");

        let error =
            format_legacy_tool_error("call_abc", "local.mcp.call", "denied", "No access").unwrap();
        let parsed = parse_legacy_tool_result(&error).unwrap();
        assert_eq!(parsed.payload["ok"], false);
        assert_eq!(parsed.payload["error"]["code"], "denied");
    }

    #[test]
    fn never_accepts_result_attribute_injection_or_ok_override() {
        assert!(format_legacy_tool_success("id", "hello", json!({"ok":false})).is_err());
        assert!(format_legacy_tool_success("x\" y=", "hello", json!({})).is_err());
        assert!(
            parse_legacy_tool_result(
                "<tool_result name=\"hello\" id=\"id\">{\"ok\":false}</tool_result>"
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_hash_uses_utf16_code_units() {
        assert_eq!(shim_hash_id("hi"), shim_hash_id("hi"));
        assert_ne!(shim_hash_id("🙂"), shim_hash_id("🙂🙂"));
    }
}
