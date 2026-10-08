//! Read-only comparison of one completed MCP result against one *observed*
//! provider outputSchema. This is not a general JSON Schema 2020-12 engine.
//! Unsupported assertions are reported as inconclusive, never as success.
//! No schema, result text, URI, or annotation becomes execution authority.

use crate::mcp_wire::{
    MAX_MCP_FRAME_BYTES, McpListedTool, McpWireError, validate_tools_call_result,
};
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_INSPECTION_DEPTH: usize = 8;
const MAX_INSPECTION_NODES: usize = 256;
const MAX_INSPECTION_COLLECTION: usize = 128;
const MAX_INSPECTION_SCHEMA_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOutputVerdict {
    NoAdvertisedSchema,
    NoStructuredContent,
    ToolReportedError,
    PassedSupportedChecks,
    Mismatch,
    Inconclusive,
}

/// Explanation is a fixed literal: provider-authored strings never enter it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpOutputInspection {
    pub verdict: McpOutputVerdict,
    pub explanation: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InspectionIssue {
    Mismatch,
    Unsupported,
}

#[derive(Default)]
struct InspectionBudget {
    visited: usize,
}

impl InspectionBudget {
    fn visit(&mut self, depth: usize) -> Result<(), InspectionIssue> {
        self.visited += 1;
        if depth > MAX_INSPECTION_DEPTH || self.visited > MAX_INSPECTION_NODES {
            return Err(InspectionIssue::Unsupported);
        }
        Ok(())
    }
}

/// Compare the result to *this exact catalog snapshot*, not to any future
/// refreshed provider schema. The caller must separately correlate provider,
/// tool name, conversation, dispatch, and recorded observation.
pub fn inspect_structured_tool_output(
    tool: &McpListedTool,
    result: &Value,
) -> Result<McpOutputInspection, McpWireError> {
    let shape = validate_tools_call_result(result)?;
    if shape.is_error {
        return Ok(report(
            McpOutputVerdict::ToolReportedError,
            "Tool reported an error; output schema comparison is not applicable.",
        ));
    }
    let Some(schema) = tool.output_schema.as_ref() else {
        return Ok(report(
            McpOutputVerdict::NoAdvertisedSchema,
            "The inspected tool catalog did not declare outputSchema.",
        ));
    };
    let Some(value) = result.get("structuredContent") else {
        return Ok(report(
            McpOutputVerdict::NoStructuredContent,
            "The completed result did not supply structuredContent.",
        ));
    };
    let schema_bytes = serde_json::to_vec(schema).map_err(|_| McpWireError::InvalidResponse)?;
    let output_bytes = serde_json::to_vec(value).map_err(|_| McpWireError::InvalidResponse)?;
    if schema_bytes.len() > MAX_INSPECTION_SCHEMA_BYTES || output_bytes.len() > MAX_MCP_FRAME_BYTES
    {
        return Ok(report(
            McpOutputVerdict::Inconclusive,
            "The advertised schema or structured output exceeds the inspection budget.",
        ));
    }
    // Review the *entire* advertised schema before inspecting the value.
    // Otherwise unsupported constraints hidden behind absent properties
    // could be skipped and mistakenly reported as a supported-subset pass.
    let mut schema_budget = InspectionBudget::default();
    if inspect_schema_subset(schema, 0, &mut schema_budget).is_err() {
        return Ok(report(
            McpOutputVerdict::Inconclusive,
            "The schema contains unsupported, malformed, or over-budget assertions; no conformance claim was made.",
        ));
    }
    let mut budget = InspectionBudget::default();
    match inspect_value(schema, value, 0, &mut budget) {
        Ok(()) => Ok(report(
            McpOutputVerdict::PassedSupportedChecks,
            "The value passed every recognized constraint in this bounded schema subset; this is not full JSON Schema validation.",
        )),
        Err(InspectionIssue::Mismatch) => Ok(report(
            McpOutputVerdict::Mismatch,
            "The structured output violates a recognized schema constraint.",
        )),
        Err(InspectionIssue::Unsupported) => Ok(report(
            McpOutputVerdict::Inconclusive,
            "The schema contains unsupported, malformed, or over-budget assertions; no conformance claim was made.",
        )),
    }
}

const fn report(verdict: McpOutputVerdict, explanation: &'static str) -> McpOutputInspection {
    McpOutputInspection {
        verdict,
        explanation,
    }
}

/// Validate every schema node, even those not reached by the particular
/// output. Unsupported assertion keywords anywhere yield Inconclusive.
fn inspect_schema_subset(
    schema: &Value,
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<(), InspectionIssue> {
    budget.visit(depth)?;
    let Value::Object(spec) = schema else {
        return if schema.is_boolean() {
            Ok(())
        } else {
            Err(InspectionIssue::Unsupported)
        };
    };
    for (key, value) in spec {
        match key.as_str() {
            "$schema" => {
                if !matches!(
                    value.as_str(),
                    Some("https://json-schema.org/draft/2020-12/schema")
                        | Some("https://json-schema.org/draft/2020-12/schema#")
                ) {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "$id" | "$comment" | "title" | "description" => {
                if !value.is_string() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "deprecated" | "readOnly" | "writeOnly" => {
                if !value.is_boolean() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "examples" => {
                if !value.is_array() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "default" => {}
            "type" => {
                let types = declared_types(value)?;
                if types.is_empty() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "properties" => {
                let properties = value.as_object().ok_or(InspectionIssue::Unsupported)?;
                if properties.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                for nested in properties.values() {
                    inspect_schema_subset(nested, depth + 1, budget)?;
                }
            }
            "required" => {
                let names = value.as_array().ok_or(InspectionIssue::Unsupported)?;
                if names.len() > MAX_INSPECTION_COLLECTION
                    || names.iter().any(|name| name.as_str().is_none())
                {
                    return Err(InspectionIssue::Unsupported);
                }
                let unique = names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>();
                if unique.len() != names.len() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "items" | "additionalProperties" => {
                inspect_schema_subset(value, depth + 1, budget)?;
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
            | "maxProperties" => {
                if value.as_u64().is_none() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            _ => return Err(InspectionIssue::Unsupported),
        }
    }
    Ok(())
}

fn declared_types(value: &Value) -> Result<Vec<&str>, InspectionIssue> {
    let names = match value {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) if !names.is_empty() && names.len() <= 7 => names
            .iter()
            .map(|item| item.as_str().ok_or(InspectionIssue::Unsupported))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(InspectionIssue::Unsupported),
    };
    if names.iter().any(|name| {
        !matches!(
            *name,
            "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
        )
    }) || names.iter().collect::<BTreeSet<_>>().len() != names.len()
    {
        return Err(InspectionIssue::Unsupported);
    }
    Ok(names)
}

fn matches_json_type(kind: &str, value: &Value) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "number" => value.is_number(),
        "integer" => value.as_number().is_some_and(|number| {
            number.is_i64()
                || number.is_u64()
                || number.as_f64().is_some_and(|number| number.fract() == 0.0)
        }),
        "string" => value.is_string(),
        _ => false,
    }
}

fn inspect_value(
    schema: &Value,
    value: &Value,
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<(), InspectionIssue> {
    budget.visit(depth)?;
    match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return Err(InspectionIssue::Mismatch),
        Value::Object(spec) => {
            if let Some(kind) = spec.get("type") {
                let kinds = declared_types(kind)?;
                if !kinds.iter().any(|kind| matches_json_type(kind, value)) {
                    return Err(InspectionIssue::Mismatch);
                }
            }
            if let Some(text) = value.as_str() {
                let length = text.chars().count() as u64;
                check_count(spec, "minLength", "maxLength", length)?;
            }
            let props = match spec.get("properties") {
                None => None,
                Some(Value::Object(props)) => Some(props),
                _ => return Err(InspectionIssue::Unsupported),
            };
            let required = match spec.get("required") {
                None => None,
                Some(Value::Array(names)) => {
                    if names.len() > MAX_INSPECTION_COLLECTION
                        || names.iter().any(|name| name.as_str().is_none())
                    {
                        return Err(InspectionIssue::Unsupported);
                    }
                    let names = names.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                    if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
                        return Err(InspectionIssue::Unsupported);
                    }
                    Some(names)
                }
                _ => return Err(InspectionIssue::Unsupported),
            };
            let extra = spec.get("additionalProperties");
            if extra.is_some_and(|value| !value.is_boolean() && !value.is_object()) {
                return Err(InspectionIssue::Unsupported);
            }
            let items = spec.get("items");
            if items.is_some_and(|value| !value.is_boolean() && !value.is_object()) {
                return Err(InspectionIssue::Unsupported);
            }
            if let Some(object) = value.as_object() {
                if object.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                check_count(spec, "minProperties", "maxProperties", object.len() as u64)?;
                if let Some(required) = required {
                    for name in required {
                        if !object.contains_key(name) {
                            return Err(InspectionIssue::Mismatch);
                        }
                    }
                }
                for (name, member) in object {
                    if let Some(child) = props.and_then(|props| props.get(name)) {
                        inspect_value(child, member, depth + 1, budget)?;
                    } else if let Some(extra) = extra {
                        inspect_value(extra, member, depth + 1, budget)?;
                    }
                }
            }
            if let Some(array) = value.as_array() {
                if array.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                check_count(spec, "minItems", "maxItems", array.len() as u64)?;
                if let Some(items) = items {
                    for member in array {
                        inspect_value(items, member, depth + 1, budget)?;
                    }
                }
            }
            Ok(())
        }
        _ => Err(InspectionIssue::Unsupported),
    }
}

fn check_count(
    schema: &serde_json::Map<String, Value>,
    min_key: &str,
    max_key: &str,
    actual: u64,
) -> Result<(), InspectionIssue> {
    if schema
        .get(min_key)
        .is_some_and(|limit| limit.as_u64().is_some_and(|min| actual < min))
        || schema
            .get(max_key)
            .is_some_and(|limit| limit.as_u64().is_some_and(|max| actual > max))
    {
        return Err(InspectionIssue::Mismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_wire::parse_tools_list_page;
    use serde_json::json;

    fn inspected_tool(schema: Option<Value>) -> McpListedTool {
        let mut listed = json!({
            "resultType": "complete",
            "tools": [{"name": "weather.read", "inputSchema": {"type": "object"}}]
        });
        if let Some(schema) = schema {
            listed["tools"][0]["outputSchema"] = schema;
        }
        parse_tools_list_page(&listed).unwrap().tools.remove(0)
    }

    fn complete(value: Value) -> Value {
        json!({
            "resultType": "complete",
            "content": [{"type":"text","text":"untrusted output"}],
            "structuredContent": value
        })
    }

    #[test]
    fn compares_object_array_scalar_and_null_snapshots_without_executing() {
        let cases = [
            (
                json!({"type":"object","required":["ok"],"properties":{"ok":{"type":"boolean"}},"additionalProperties":false}),
                json!({"ok":true}),
            ),
            (
                json!({"type":"array","items":{"type":"integer"}}),
                json!([1, 2, 3]),
            ),
            (json!({"type":"string"}), json!("ok")),
            (json!({"type":"null"}), Value::Null),
            (json!(true), json!({"anything":"allowed"})),
        ];
        for (schema, value) in cases {
            let inspection =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap();
            assert_eq!(inspection.verdict, McpOutputVerdict::PassedSupportedChecks);
        }
    }

    #[test]
    fn detects_definite_schema_mismatch() {
        let tool = inspected_tool(Some(json!({
            "type":"array",
            "items":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}
        })));
        for bad in [
            json!([{"id":4}]),
            json!([{"wrong":"field"}]),
            json!({"id":"x"}),
        ] {
            let inspection = inspect_structured_tool_output(&tool, &complete(bad)).unwrap();
            assert_eq!(inspection.verdict, McpOutputVerdict::Mismatch);
        }
        let never = inspected_tool(Some(json!(false)));
        assert_eq!(
            inspect_structured_tool_output(&never, &complete(json!(1)))
                .unwrap()
                .verdict,
            McpOutputVerdict::Mismatch
        );
    }

    #[test]
    fn missing_schema_missing_content_and_tool_error_are_distinct() {
        let none = inspected_tool(None);
        assert_eq!(
            inspect_structured_tool_output(&none, &complete(json!(1)))
                .unwrap()
                .verdict,
            McpOutputVerdict::NoAdvertisedSchema
        );
        let declared = inspected_tool(Some(json!({"type":"object"})));
        assert_eq!(
            inspect_structured_tool_output(
                &declared,
                &json!({
                    "resultType":"complete","content":[]
                })
            )
            .unwrap()
            .verdict,
            McpOutputVerdict::NoStructuredContent
        );
        assert_eq!(
            inspect_structured_tool_output(
                &declared,
                &json!({
                    "resultType":"complete","content":[],
                    "isError":true,
                    "structuredContent":{"error":"bad"}
                })
            )
            .unwrap()
            .verdict,
            McpOutputVerdict::ToolReportedError
        );
        assert!(
            inspect_structured_tool_output(
                &declared,
                &json!({
                    "resultType":"complete","content":[{"type":"text"}]
                })
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_assertions_and_reference_schemas_are_never_declared_valid() {
        for schema in [
            json!({"type":"string","pattern":"^hello$"}),
            json!({"type":"string","enum":["safe"]}),
            json!({"$ref":"https://untrusted.example/schema.json"}),
            json!({"oneOf":[{"type":"number"},{"type":"string"}]}),
            json!({"type":["string","string"]}),
            json!({"type":"array","minItems":-1}),
            json!({"type":"number","minimum":0}),
            json!({"type":"object","unevaluatedProperties":false}),
            json!({"type":"object","$schema":"http://json-schema.org/draft-07/schema#"}),
        ] {
            let check = inspect_structured_tool_output(
                &inspected_tool(Some(schema)),
                &complete(json!("hello")),
            )
            .unwrap();
            assert_eq!(check.verdict, McpOutputVerdict::Inconclusive);
        }
    }

    #[test]
    fn supported_length_and_cardinality_keywords_use_json_schema_semantics() {
        let cases = [
            (
                json!({"type":"string","minLength":2,"maxLength":3}),
                json!("é🙂"),
            ),
            (json!({"type":["string","null"],"minLength":2}), Value::Null),
            (
                json!({"type":"array","minItems":1,"maxItems":2,"items":{"type":"integer"}}),
                json!([1, 2]),
            ),
            (
                json!({"type":"object","minProperties":1,"maxProperties":2}),
                json!({"k":true}),
            ),
        ];
        for (schema, value) in cases {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::PassedSupportedChecks);
        }
        let failures = [
            (json!({"minLength":2}), json!("é")),
            (json!({"maxLength":1}), json!("😊x")),
            (json!({"minItems":2}), json!([1])),
            (json!({"maxItems":0}), json!([1])),
            (json!({"minProperties":2}), json!({"only":1})),
            (json!({"maxProperties":0}), json!({"extra":1})),
            (json!({"type":["object","array"]}), json!("wrong")),
        ];
        for (schema, value) in failures {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::Mismatch);
        }
    }

    #[test]
    fn unsupported_keywords_in_unvisited_properties_must_never_pass() {
        for schema in [
            json!({"type":"object","properties":{"absent":{"type":"string","pattern":"^x$"}}}),
            json!({"type":"object","properties":{"absent":{"$ref":"https://example.org/unsafe"}}}),
            json!({"type":"array","items":{"enum":[1,2]}}),
            json!({"type":"string","minLength":"two"}),
            json!({"type":"object","properties":{"unused":{"type":["string","string"]}}}),
            json!({"type":"object","required":["x","x"]}),
            json!({"type":"object","properties":{"a":{"minimum":1}}}),
        ] {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(json!({})))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::Inconclusive);
        }
    }

    #[test]
    fn annotation_types_are_checked_even_without_applicable_assertions() {
        let tool = inspected_tool(Some(json!({"title":42})));
        assert_eq!(
            inspect_structured_tool_output(&tool, &complete(json!({})))
                .unwrap()
                .verdict,
            McpOutputVerdict::Inconclusive,
        );
    }

    #[test]
    fn inspection_limits_decline_large_or_deep_values() {
        let tool = inspected_tool(Some(json!({"type":"array","items":{"type":"object"}})));
        let values = vec![json!({}); MAX_INSPECTION_COLLECTION + 1];
        assert_eq!(
            inspect_structured_tool_output(&tool, &complete(json!(values)))
                .unwrap()
                .verdict,
            McpOutputVerdict::Inconclusive
        );
        let mut schema = json!({"type":"string"});
        let mut value = json!("bottom");
        for _ in 0..MAX_INSPECTION_DEPTH + 2 {
            schema = json!({"type":"array","items":schema});
            value = json!([value]);
        }
        assert_eq!(
            inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                .unwrap()
                .verdict,
            McpOutputVerdict::Inconclusive
        );
    }
}
