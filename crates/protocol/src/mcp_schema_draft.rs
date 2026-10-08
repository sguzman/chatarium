//! Conservative editable JSON argument drafts from untrusted MCP tools/list schemas.
//!
//! This module is intentionally NOT a JSON Schema validator or a permission
//! authority. It reads only the schema's required-property structure and
//! basic types; no default, example, enum, reference, expression or annotation
//! is ever executed or adopted as a user-supplied argument.

use crate::mcp_wire::MAX_MCP_ARGUMENT_BYTES;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const MAX_DRAFT_FIELDS: usize = 64;
const MAX_DRAFT_DEPTH: usize = 8;
const MAX_REQUIRED_PER_OBJECT: usize = 32;
const MAX_PROPERTY_NAME_BYTES: usize = 256;

/// Editable text plus a bounded review checklist, never an executable call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpArgumentDraft {
    pub arguments_json: String,
    pub review_notes: Vec<String>,
}

struct DraftBuilder {
    visited: usize,
    notes: Vec<String>,
}

impl DraftBuilder {
    fn visit(&mut self) -> Result<(), String> {
        self.visited += 1;
        if self.visited > MAX_DRAFT_FIELDS {
            return Err("provider schema has too many nested required fields to draft".to_owned());
        }
        Ok(())
    }

    fn placeholder(&mut self, schema: &Value, path: &str, depth: usize) -> Result<Value, String> {
        self.visit()?;
        if depth > MAX_DRAFT_DEPTH {
            return Err("provider schema is too deeply nested to draft safely".to_owned());
        }
        let spec = schema
            .as_object()
            .ok_or_else(|| format!("provider schema for {path} is not an object"))?;
        if [
            "$ref", "oneOf", "anyOf", "allOf", "not", "if", "then", "else",
        ]
        .iter()
        .any(|keyword| spec.contains_key(*keyword))
            || spec.get("type").is_some_and(|t| t.is_array())
        {
            self.notes
                .push(format!("{path}: complex schema; replace null manually"));
            return Ok(Value::Null);
        }
        match spec.get("type").and_then(Value::as_str) {
            Some("object") => self.object(schema, path, depth),
            Some("string") => {
                self.notes.push(format!("{path}: replace empty string"));
                Ok(Value::String(String::new()))
            }
            Some("integer" | "number") => {
                self.notes
                    .push(format!("{path}: review numeric zero placeholder"));
                Ok(Value::from(0))
            }
            Some("boolean") => {
                self.notes.push(format!("{path}: review false placeholder"));
                Ok(Value::Bool(false))
            }
            Some("array") => {
                self.notes.push(format!("{path}: populate array as needed"));
                Ok(Value::Array(Vec::new()))
            }
            Some("null") => Ok(Value::Null),
            None if spec.contains_key("properties") => self.object(schema, path, depth),
            _ => {
                self.notes.push(format!(
                    "{path}: unsupported or unspecified type; replace null"
                ));
                Ok(Value::Null)
            }
        }
    }

    fn object(&mut self, schema: &Value, path: &str, depth: usize) -> Result<Value, String> {
        let spec = schema
            .as_object()
            .ok_or_else(|| format!("{path}: expected JSON Schema object"))?;
        let props = match spec.get("properties") {
            Some(Value::Object(map)) => Some(map),
            None => None,
            _ => return Err(format!("{path}: properties must be an object")),
        };
        let required = match spec.get("required") {
            Some(Value::Array(names)) => names.as_slice(),
            None => &[],
            _ => return Err(format!("{path}: required must be an array")),
        };
        if required.len() > MAX_REQUIRED_PER_OBJECT {
            return Err(format!(
                "{path}: required field list exceeds drafting limit"
            ));
        }
        let mut seen = BTreeSet::new();
        let mut output = Map::new();
        for name in required {
            let key = name
                .as_str()
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= MAX_PROPERTY_NAME_BYTES
                        && !name.chars().any(char::is_control)
                })
                .ok_or_else(|| format!("{path}: invalid required field name"))?;
            if !seen.insert(key) {
                return Err(format!("{path}: duplicate required field name"));
            }
            let field = props
                .and_then(|map| map.get(key))
                .ok_or_else(|| format!("{path}: required field lacks a property schema"))?;
            let field_path = format!("{path}.{key}");
            let value = self.placeholder(field, &field_path, depth + 1)?;
            output.insert(key.to_owned(), value);
        }
        Ok(Value::Object(output))
    }
}

/// Derive only required-field placeholders from one already-inspected tool.
///
/// Optional properties are intentionally not invented; defaults/enums/examples
/// supplied by a provider are intentionally never copied into the draft.
/// A returned draft may NOT satisfy the schema until a person edits it.
/// Saving a ToolCall, approval and Run remain independent user actions.
pub fn draft_required_arguments(input_schema: &Value) -> Result<McpArgumentDraft, String> {
    let schema = input_schema
        .as_object()
        .ok_or_else(|| "tool inputSchema must be a JSON object".to_owned())?;
    if schema.contains_key("$ref")
        || ["allOf", "anyOf", "oneOf", "not", "if"]
            .iter()
            .any(|keyword| schema.contains_key(*keyword))
    {
        return Err("root schema composition/reference is not supported for drafting".to_owned());
    }
    if schema
        .get("type")
        .is_some_and(|t| t.as_str() != Some("object"))
    {
        return Err("tool arguments must have an object inputSchema".to_owned());
    }
    let mut builder = DraftBuilder {
        visited: 0,
        notes: Vec::new(),
    };
    let arguments = builder.object(input_schema, "arguments", 0)?;
    let arguments_json = serde_json::to_string_pretty(&arguments)
        .map_err(|_| "could not serialize provider argument draft".to_owned())?;
    if arguments_json.len() > MAX_MCP_ARGUMENT_BYTES {
        return Err("required argument template exceeds MCP argument budget".to_owned());
    }
    Ok(McpArgumentDraft {
        arguments_json,
        review_notes: builder.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drafts_only_required_fields_and_never_adopts_server_defaults() {
        let schema = json!({
            "type": "object",
            "required": ["city", "options", "count"],
            "properties": {
                "city": {"type":"string", "default":"server-controlled", "enum":["bad"]},
                "options": {"type":"object", "required":["enabled"], "properties":{
                    "enabled":{"type":"boolean","default":true},
                    "optionalDanger":{"type":"string","default":"delete everything"}
                }},
                "count": {"type":"integer", "default":99},
                "optionalAction": {"type":"string", "default":"unsafe"}
            }
        });
        let draft = draft_required_arguments(&schema).unwrap();
        let parsed: Value = serde_json::from_str(&draft.arguments_json).unwrap();
        assert_eq!(
            parsed,
            json!({"city":"","options":{"enabled":false},"count":0})
        );
        assert!(!draft.arguments_json.contains("server-controlled"));
        assert!(!draft.arguments_json.contains("optionalDanger"));
        assert_eq!(draft.review_notes.len(), 3);
    }

    #[test]
    fn handles_unsupported_nested_reference_as_explicit_null_placeholder() {
        let draft = draft_required_arguments(&json!({
            "type":"object",
            "required":["selector","extras"],
            "properties":{
                "selector":{"$ref":"#/$defs/anything"},
                "extras":{"oneOf":[{"type":"string"},{"type":"number"}]}
            }
        }))
        .unwrap();
        let parsed: Value = serde_json::from_str(&draft.arguments_json).unwrap();
        assert_eq!(parsed, json!({"selector":null,"extras":null}));
        assert_eq!(draft.review_notes.len(), 2);
    }

    #[test]
    fn refuses_malformed_schema_unbounded_fields_and_composition_at_root() {
        for schema in [
            json!({"type":"string"}),
            json!({"type":"object","required":["missing"]}),
            json!({"type":"object","properties":{},"required":["x","x"]}),
            json!({"type":"object","required":"x"}),
            json!({"$ref":"https://untrusted.example/schema"}),
            json!({"type":"object","required":[42],"properties":{}}),
        ] {
            assert!(draft_required_arguments(&schema).is_err());
        }
        let mut nested = json!({"type":"string"});
        for _ in 0..12 {
            nested = json!({"type":"object","required":["x"],"properties":{"x": nested}});
        }
        assert!(draft_required_arguments(&nested).is_err());
        let names: Vec<String> = (0..40).map(|index| format!("f{index}")).collect();
        let properties: Map<String, Value> = names
            .iter()
            .map(|name| (name.clone(), json!({"type":"boolean"})))
            .collect();
        let schema = json!({"type":"object","required":names,"properties":properties});
        assert!(draft_required_arguments(&schema).is_err());
    }

    #[test]
    fn empty_or_optional_only_schema_produces_empty_editable_object() {
        let draft = draft_required_arguments(&json!({
            "type":"object","properties":{"extra":{"type":"string","default":"injected"}}
        }))
        .unwrap();
        assert_eq!(draft.arguments_json, "{}");
        assert!(draft.review_notes.is_empty());
    }
}
