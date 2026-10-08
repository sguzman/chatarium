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
    if schema_bytes.len() > MAX_INSPECTION_SCHEMA_BYTES
        || output_bytes.len() > MAX_MCP_FRAME_BYTES
    {
        return Ok(report(
            McpOutputVerdict::Inconclusive,
            "The advertised schema or structured output exceeds the inspection budget.",
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
            // Fail closed on *any* constraint we do not interpret. Annotation
            // keywords are not assertions. $ref, composition, format, pattern,
            // conditionals, unevaluated*, and arbitrary extension keywords
            // cannot be silently treated as validated.
            for name in spec.keys() {
                if !matches!(
                    name.as_str(),
                    "$schema"
                        | "$id"
                        | "$comment"
                        | "title"
                        | "description"
                        | "default"
                        | "examples"
                        | "deprecated"
                        | "readOnly"
                        | "writeOnly"
                        | "type"
                        | "properties"
                        | "required"
                        | "items"
                        | "additionalProperties"
                ) {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            if let Some(dialect) = spec.get("$schema") {
                if !matches!(
                    dialect.as_str(),
                    Some("https://json-schema.org/draft/2020-12/schema")
                        | Some("https://json-schema.org/draft/2020-12/schema#")
                ) {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            if let Some(kind) = spec.get("type") {
                let kind = kind.as_str().ok_or(InspectionIssue::Unsupported)?;
                if !matches!(
                    kind,
                    "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
                ) {
                    return Err(InspectionIssue::Unsupported);
                }
                let matches = match kind {
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
                    _ => return Err(InspectionIssue::Unsupported),
                };
                if !matches {
                    return Err(InspectionIssue::Mismatch);
                }
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
                json!([1,2,3]),
            ),
            (json!({"type":"string"}), json!("ok")),
            (json!({"type":"null"}), Value::Null),
            (json!(true), json!({"anything":"allowed"})),
        ];
        for (schema, value) in cases {
            let inspection = inspect_structured_tool_output(
                &inspected_tool(Some(schema)), &complete(value)
            )
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
        for bad in [json!([{"id":4}]), json!([{"wrong":"field"}]), json!({"id":"x"})] {
            let inspection = inspect_structured_tool_output(&tool, &complete(bad)).unwrap();
            assert_eq!(inspection.verdict, McpOutputVerdict::Mismatch);
        }
        let never = inspected_tool(Some(json!(false)));
        assert_eq!(
            inspect_structured_tool_output(&never, &complete(json!(1)))
                .unwrap().verdict,
            McpOutputVerdict::Mismatch
        );
    }

    #[test]
    fn missing_schema_missing_content_and_tool_error_are_distinct() {
        let none = inspected_tool(None);
        assert_eq!(
            inspect_structured_tool_output(&none, &complete(json!(1))).unwrap().verdict,
            McpOutputVerdict::NoAdvertisedSchema
        );
        let declared = inspected_tool(Some(json!({"type":"object"})));
        assert_eq!(
            inspect_structured_tool_output(&declared, &json!({
                "resultType":"complete","content":[]
            })).unwrap().verdict,
            McpOutputVerdict::NoStructuredContent
        );
        assert_eq!(
            inspect_structured_tool_output(&declared, &json!({
                "resultType":"complete","content":[],
                "isError":true,
                "structuredContent":{"error":"bad"}
            })).unwrap().verdict,
            McpOutputVerdict::ToolReportedError
        );
        assert!(inspect_structured_tool_output(&declared, &json!({
            "resultType":"complete","content":[{"type":"text"}]
        })).is_err());
    }

    #[test]
    fn unknown_assertions_and_reference_schemas_are_never_declared_valid() {
        for schema in [
            json!({"type":"string","pattern":"^hello$"}),
            json!({"type":"string","enum":["safe"]}),
            json!({"$ref":"https://untrusted.example/schema.json"}),
            json!({"oneOf":[{"type":"number"},{"type":"string"}]}),
            json!({"type":["string","number"]}),
            json!({"type":"array","minItems":1}),
            json!({"type":"number","minimum":0}),
            json!({"type":"object","unevaluatedProperties":false}),
            json!({"type":"object","$schema":"http://json-schema.org/draft-07/schema#"}),
        ] {
            let check = inspect_structured_tool_output(
                &inspected_tool(Some(schema)), &complete(json!("hello"))
            ).unwrap();
            assert_eq!(check.verdict, McpOutputVerdict::Inconclusive);
        }
    }

    #[test]
    fn inspection_limits_decline_large_or_deep_values() {
        let tool = inspected_tool(Some(json!({"type":"array","items":{"type":"object"}})));
        let values = vec![json!({}); MAX_INSPECTION_COLLECTION + 1];
        assert_eq!(
            inspect_structured_tool_output(&tool, &complete(json!(values)))
                .unwrap().verdict,
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
                .unwrap().verdict,
            McpOutputVerdict::Inconclusive
        );
    }
}
