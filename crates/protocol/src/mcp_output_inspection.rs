//! Read-only comparison of one completed MCP result against one *observed*
//! provider outputSchema. This is not a general JSON Schema 2020-12 engine.
//! Unsupported assertions are reported as inconclusive, never as success.
//! No schema, result text, URI, or annotation becomes execution authority.

use crate::mcp_wire::{
    MAX_MCP_FRAME_BYTES, McpListedTool, McpWireError, validate_tools_call_result,
};
use serde_json::{Number, Value};
use std::cmp::Ordering;
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
    // Only code-owned constraint labels can be emitted in diagnostics.
    // Never copy provider-authored schema keys, values, or output paths.
    Mismatch(&'static str),
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
        Err(InspectionIssue::Mismatch(keyword)) => Ok(report(
            McpOutputVerdict::Mismatch,
            mismatch_explanation(keyword),
        )),
        Err(InspectionIssue::Unsupported) => Ok(report(
            McpOutputVerdict::Inconclusive,
            "The schema contains unsupported, malformed, or over-budget assertions; no conformance claim was made.",
        )),
    }
}

// Fixed diagnostic strings are intentional: provider keys, instance paths and
// values may contain private data or terminal-control characters. Report the
// first recognized failing keyword, not untrusted provider-authored content.
const fn mismatch_explanation(keyword: &str) -> &'static str {
    match keyword {
        "false schema" => "A boolean false schema rejects the structured output.",
        "type" => "Constraint type failed: the structured output has an unexpected JSON type.",
        "required" => "Constraint required failed: a required object property is missing.",
        "const" => "Constraint const failed: the structured output differs from the declared constant.",
        "enum" => "Constraint enum failed: the structured output matches no declared option.",
        "minLength" => "Constraint minLength failed: a string is too short.",
        "maxLength" => "Constraint maxLength failed: a string is too long.",
        "minItems" => "Constraint minItems failed: an array has too few entries.",
        "maxItems" => "Constraint maxItems failed: an array has too many entries.",
        "minProperties" => "Constraint minProperties failed: an object has too few properties.",
        "maxProperties" => "Constraint maxProperties failed: an object has too many properties.",
        "minimum" => "Constraint minimum failed: a number is below its inclusive lower bound.",
        "maximum" => "Constraint maximum failed: a number exceeds its inclusive upper bound.",
        "exclusiveMinimum" => "Constraint exclusiveMinimum failed: a number is not above its exclusive lower bound.",
        "exclusiveMaximum" => "Constraint exclusiveMaximum failed: a number is not below its exclusive upper bound.",
        "multipleOf" => "Constraint multipleOf failed: a number is not an exact multiple.",
        "uniqueItems" => "Constraint uniqueItems failed: an array contains equivalent entries.",
        _ => "The structured output violates a recognized schema constraint.",
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
            "items" | "additionalProperties" | "propertyNames" => {
                inspect_schema_subset(value, depth + 1, budget)?;
            }
            "prefixItems" => {
                let entries = value.as_array().ok_or(InspectionIssue::Unsupported)?;
                if entries.is_empty() || entries.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                for entry in entries {
                    inspect_schema_subset(entry, depth + 1, budget)?;
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
            | "maxProperties" => {
                if value.as_u64().is_none() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if !value.is_number() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "multipleOf" => {
                let divisor = value.as_number().ok_or(InspectionIssue::Unsupported)?;
                if exact_decimal(divisor)?.0 <= 0 {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "uniqueItems" => {
                if !value.is_boolean() {
                    return Err(InspectionIssue::Unsupported);
                }
            }
            "const" => {
                inspect_json_literal(value, depth + 1, budget)?;
            }
            "enum" => {
                let members = value.as_array().ok_or(InspectionIssue::Unsupported)?;
                if members.is_empty() || members.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                for member in members {
                    inspect_json_literal(member, depth + 1, budget)?;
                }
            }
            _ => return Err(InspectionIssue::Unsupported),
        }
    }
    Ok(())
}

/// Literal values inside enum/const are data, not schemas. Bound their
/// nesting and total nodes without interpreting embedded keys as directives.
fn inspect_json_literal(
    value: &Value,
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<(), InspectionIssue> {
    budget.visit(depth)?;
    match value {
        Value::Array(members) => {
            if members.len() > MAX_INSPECTION_COLLECTION {
                return Err(InspectionIssue::Unsupported);
            }
            for member in members {
                inspect_json_literal(member, depth + 1, budget)?;
            }
        }
        Value::Object(members) => {
            if members.len() > MAX_INSPECTION_COLLECTION {
                return Err(InspectionIssue::Unsupported);
            }
            for member in members.values() {
                inspect_json_literal(member, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Exact integer comparisons avoid f64 rounding above 2^53. Mixed float /
/// integer and float / float comparisons are only attempted within that
/// range; beyond it the result is inconclusive instead of spuriously equal.
fn compare_numbers(left: &Number, right: &Number) -> Result<Ordering, InspectionIssue> {
    fn integer(number: &Number) -> Option<i128> {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    }
    if let (Some(a), Some(b)) = (integer(left), integer(right)) {
        return Ok(a.cmp(&b));
    }
    const MAX_EXACT_FLOAT_INT: f64 = 9_007_199_254_740_992.0;
    const MAX_EXACT_INTEGER: i128 = 9_007_199_254_740_992;
    if integer(left).is_some_and(|value| !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value))
        || integer(right)
            .is_some_and(|value| !(-MAX_EXACT_INTEGER..=MAX_EXACT_INTEGER).contains(&value))
    {
        // A subsequent f64 conversion would silently round these exact
        // integers and could make two *different* values compare equal.
        return Err(InspectionIssue::Unsupported);
    }
    let a = left.as_f64().ok_or(InspectionIssue::Unsupported)?;
    let b = right.as_f64().ok_or(InspectionIssue::Unsupported)?;
    if !a.is_finite()
        || !b.is_finite()
        || a.abs() > MAX_EXACT_FLOAT_INT
        || b.abs() > MAX_EXACT_FLOAT_INT
    {
        return Err(InspectionIssue::Unsupported);
    }
    a.partial_cmp(&b).ok_or(InspectionIssue::Unsupported)
}

/// Recursive JSON-semantic equality, including numeric 1 == 1.0.
/// Uncertain numeric representations cause an inconclusive verdict.
fn json_equal(
    left: &Value,
    right: &Value,
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<bool, InspectionIssue> {
    budget.visit(depth)?;
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(compare_numbers(a, b)? == Ordering::Equal),
        (Value::Null, Value::Null) => Ok(true),
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        (Value::String(a), Value::String(b)) => Ok(a == b),
        (Value::Array(a), Value::Array(b)) => {
            if a.len() > MAX_INSPECTION_COLLECTION || b.len() > MAX_INSPECTION_COLLECTION {
                return Err(InspectionIssue::Unsupported);
            }
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !json_equal(a, b, depth + 1, budget)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(a), Value::Object(b)) => {
            if a.len() > MAX_INSPECTION_COLLECTION || b.len() > MAX_INSPECTION_COLLECTION {
                return Err(InspectionIssue::Unsupported);
            }
            if a.len() != b.len() {
                return Ok(false);
            }
            for (key, a) in a {
                let Some(b) = b.get(key) else {
                    return Ok(false);
                };
                if !json_equal(a, b, depth + 1, budget)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Enum search never concludes mismatch while an unresolved candidate might
/// match. Every attempted comparison consumes the same bounded budget.
fn check_enum(
    allowed: &[Value],
    value: &Value,
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<(), InspectionIssue> {
    let mut uncertain = false;
    for candidate in allowed {
        match json_equal(value, candidate, depth + 1, budget) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(InspectionIssue::Unsupported) => uncertain = true,
            Err(InspectionIssue::Mismatch(_)) => unreachable!("equality never yields mismatch"),
        }
    }
    if uncertain {
        Err(InspectionIssue::Unsupported)
    } else {
        Err(InspectionIssue::Mismatch("enum"))
    }
}

fn check_numeric_bounds(
    spec: &serde_json::Map<String, Value>,
    actual: &Number,
) -> Result<(), InspectionIssue> {
    for (keyword, exclusive, lower) in [
        ("minimum", false, true),
        ("maximum", false, false),
        ("exclusiveMinimum", true, true),
        ("exclusiveMaximum", true, false),
    ] {
        let Some(bound) = spec.get(keyword) else {
            continue;
        };
        let number = bound.as_number().ok_or(InspectionIssue::Unsupported)?;
        let order = compare_numbers(actual, number)?;
        if (lower && (order == Ordering::Less || (exclusive && order == Ordering::Equal)))
            || (!lower && (order == Ordering::Greater || (exclusive && order == Ordering::Equal)))
        {
            return Err(InspectionIssue::Mismatch(keyword));
        }
    }
    Ok(())
}

// Preserve the exact decimal meaning of bounded, serializable JSON numbers.
// Never compute a floating-point remainder. Integers remain exact across the
// full signed/unsigned 64-bit range; decimals use a checked i128 mantissa and
// at most 18 base-ten fractional places. Unrepresentable cases are inconclusive.
fn exact_decimal(number: &Number) -> Result<(i128, u32), InspectionIssue> {
    if let Some(value) = number.as_i64() {
        return Ok((i128::from(value), 0));
    }
    if let Some(value) = number.as_u64() {
        return Ok((i128::from(value), 0));
    }
    const FLOAT_SAFE_LIMIT: f64 = 9_007_199_254_740_992.0;
    let approximate = number.as_f64().ok_or(InspectionIssue::Unsupported)?;
    if !approximate.is_finite() || approximate.abs() >= FLOAT_SAFE_LIMIT {
        return Err(InspectionIssue::Unsupported);
    }

    let text = number.to_string();
    let exponent_at = text.find('e').or_else(|| text.find('E'));
    let (digits, exponent) = if let Some(at) = exponent_at {
        let power = text[at + 1..]
            .parse::<i32>()
            .map_err(|_| InspectionIssue::Unsupported)?;
        (&text[..at], power)
    } else {
        (text.as_str(), 0)
    };
    if !(-18..=18).contains(&exponent) {
        return Err(InspectionIssue::Unsupported);
    }

    let mut mantissa = 0_i128;
    let mut fractional = 0_i32;
    let mut period = false;
    let mut saw_digit = false;
    for (index, symbol) in digits.bytes().enumerate() {
        if index == 0 && symbol == b'-' {
            continue;
        }
        if symbol == b'.' && !period {
            period = true;
            continue;
        }
        if !symbol.is_ascii_digit() {
            return Err(InspectionIssue::Unsupported);
        }
        saw_digit = true;
        mantissa = mantissa
            .checked_mul(10)
            .and_then(|value| value.checked_add(i128::from(symbol - b'0')))
            .ok_or(InspectionIssue::Unsupported)?;
        if period {
            fractional += 1;
        }
    }
    if !saw_digit {
        return Err(InspectionIssue::Unsupported);
    }
    if digits.starts_with('-') {
        mantissa = -mantissa;
    }
    let mut places = fractional - exponent;
    if !(-18..=18).contains(&places) {
        return Err(InspectionIssue::Unsupported);
    }
    if places < 0 {
        for _ in 0..(-places) {
            mantissa = mantissa
                .checked_mul(10)
                .ok_or(InspectionIssue::Unsupported)?;
        }
        places = 0;
    }
    while places > 0 && mantissa % 10 == 0 {
        mantissa /= 10;
        places -= 1;
    }
    Ok((mantissa, places as u32))
}

fn decimal_multiple(actual: &Number, divisor: &Number) -> Result<bool, InspectionIssue> {
    let (mut amount, amount_places) = exact_decimal(actual)?;
    let (mut step, step_places) = exact_decimal(divisor)?;
    if step <= 0 {
        return Err(InspectionIssue::Unsupported);
    }
    let common_places = amount_places.max(step_places);
    for _ in amount_places..common_places {
        amount = amount.checked_mul(10).ok_or(InspectionIssue::Unsupported)?;
    }
    for _ in step_places..common_places {
        step = step.checked_mul(10).ok_or(InspectionIssue::Unsupported)?;
    }
    Ok(amount % step == 0)
}

const MAX_UNIQUE_COMPARISON_ITEMS: usize = 16;

/// Deep JSON-semantic uniqueness with no hash coercion and a shared bounded
/// comparison budget. If a pair is numerically uncertain, do not claim pass.
fn check_unique_items(
    items: &[Value],
    depth: usize,
    budget: &mut InspectionBudget,
) -> Result<(), InspectionIssue> {
    if items.len() > MAX_UNIQUE_COMPARISON_ITEMS {
        return Err(InspectionIssue::Unsupported);
    }
    for (index, item) in items.iter().enumerate() {
        for other in &items[index + 1..] {
            if json_equal(item, other, depth + 1, budget)? {
                return Err(InspectionIssue::Mismatch("uniqueItems"));
            }
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
        Value::Bool(false) => return Err(InspectionIssue::Mismatch("false schema"),
        Value::Object(spec) => {
            if let Some(kind) = spec.get("type") {
                let kinds = declared_types(kind)?;
                if !kinds.iter().any(|kind| matches_json_type(kind, value)) {
                    return Err(InspectionIssue::Mismatch("type"));
                }
            }
            if let Some(text) = value.as_str() {
                let length = text.chars().count() as u64;
                check_count(spec, "minLength", "maxLength", length)?;
            }
            if let Some(actual) = value.as_number() {
                check_numeric_bounds(spec, actual)?;
                if let Some(divisor) = spec.get("multipleOf").and_then(Value::as_number) {
                    if !decimal_multiple(actual, divisor)? {
                        return Err(InspectionIssue::Mismatch("multipleOf"));
                    }
                }
            }
            if let Some(expected) = spec.get("const") {
                if !json_equal(value, expected, depth + 1, budget)? {
                    return Err(InspectionIssue::Mismatch("const"));
                }
            }
            if let Some(allowed) = spec.get("enum").and_then(Value::as_array) {
                check_enum(allowed, value, depth + 1, budget)?;
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
            let names = spec.get("propertyNames");
            if names.is_some_and(|value| !value.is_boolean() && !value.is_object()) {
                return Err(InspectionIssue::Unsupported);
            }
            let items = spec.get("items");
            if items.is_some_and(|value| !value.is_boolean() && !value.is_object()) {
                return Err(InspectionIssue::Unsupported);
            }
            let prefix_items = match spec.get("prefixItems") {
                None => None,
                Some(Value::Array(entries))
                    if !entries.is_empty() && entries.len() <= MAX_INSPECTION_COLLECTION =>
                {
                    Some(entries)
                }
                _ => return Err(InspectionIssue::Unsupported),
            };
            if let Some(object) = value.as_object() {
                if object.len() > MAX_INSPECTION_COLLECTION {
                    return Err(InspectionIssue::Unsupported);
                }
                check_count(spec, "minProperties", "maxProperties", object.len() as u64)?;
                if let Some(required) = required {
                    for name in required {
                        if !object.contains_key(name) {
                            return Err(InspectionIssue::Mismatch("required"));
                        }
                    }
                }
                for (name, member) in object {
                    // propertyNames independently validates every key,
                    // including names also listed in properties.
                    if let Some(names) = names {
                        inspect_value(names, &Value::String(name.to_owned()), depth + 1, budget)?;
                    }
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
                if spec.get("uniqueItems").is_some_and(|flag| flag == true) {
                    check_unique_items(array, depth + 1, budget)?;
                }
                if let Some(prefix_items) = prefix_items {
                    for (member, schema) in array.iter().zip(prefix_items) {
                        inspect_value(schema, member, depth + 1, budget)?;
                    }
                }
                if let Some(items) = items {
                    // Draft 2020-12: items only applies *after* prefixItems.
                    // Without prefixItems it applies to the entire array.
                    let start = prefix_items.map_or(0, Vec::len);
                    for member in array.iter().skip(start) {
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
    min_key: &'static str,
    max_key: &'static str,
    actual: u64,
) -> Result<(), InspectionIssue> {
    if schema
        .get(min_key)
        .is_some_and(|limit| limit.as_u64().is_some_and(|min| actual < min))
    {
        return Err(InspectionIssue::Mismatch(min_key));
    }
    if schema
        .get(max_key)
        .is_some_and(|limit| limit.as_u64().is_some_and(|max| actual > max))
    {
        return Err(InspectionIssue::Mismatch(max_key));
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
            json!({"type":"string","enum":[]}),
            json!({"$ref":"https://untrusted.example/schema.json"}),
            json!({"oneOf":[{"type":"number"},{"type":"string"}]}),
            json!({"type":["string","string"]}),
            json!({"type":"array","minItems":-1}),
            json!({"type":"number","minimum":"zero"}),
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
            json!({"type":"array","items":{"oneOf":[{"type":"integer"}]}}),
            json!({"type":"string","minLength":"two"}),
            json!({"type":"object","properties":{"unused":{"type":["string","string"]}}}),
            json!({"type":"object","required":["x","x"]}),
            json!({"type":"object","properties":{"a":{"multipleOf":0}}}),
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
    fn const_and_enum_compare_nested_json_semantically() {
        for (schema, value) in [
            (
                json!({"const": {"kind":"sample","sizes":[1,2.0]}}),
                json!({"sizes":[1.0,2],"kind":"sample"}),
            ),
            (
                json!({"enum":[null,true,{"count":1}]}),
                json!({"count":1.0}),
            ),
            (json!({"type":"number","const":1}), json!(1.0)),
            (json!({"type":"string","enum":["x","y"]}), json!("y")),
        ] {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::PassedSupportedChecks);
        }
        for (schema, value) in [
            (json!({"const":{"x":1}}), json!({"x":2})),
            (json!({"enum":["x","y"]}), json!("z")),
            (json!({"enum":[1,2]}), json!(3)),
            (json!({"const":true}), json!(false)),
        ] {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::Mismatch);
        }
    }

    #[test]
    fn numeric_bounds_handle_inclusive_exclusive_and_fractional_values() {
        let cases = [
            (
                json!({"minimum":1}),
                json!(1),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"exclusiveMinimum":1}),
                json!(1),
                McpOutputVerdict::Mismatch,
            ),
            (
                json!({"maximum":2}),
                json!(2),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"exclusiveMaximum":2}),
                json!(2),
                McpOutputVerdict::Mismatch,
            ),
            (
                json!({"minimum":-3,"maximum":3}),
                json!(-2),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"exclusiveMinimum":0.25}),
                json!(0.5),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"maximum":0.25}),
                json!(0.5),
                McpOutputVerdict::Mismatch,
            ),
            (json!({"minimum":-1}), json!(-2), McpOutputVerdict::Mismatch),
            (
                json!({"exclusiveMaximum":10}),
                json!("not a number"),
                McpOutputVerdict::PassedSupportedChecks,
            ),
        ];
        for (schema, value, expected) in cases {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, expected);
        }
    }

    #[test]
    fn numeric_precision_uncertainty_never_becomes_mismatch_or_pass() {
        let huge_integer = json!(9_007_199_254_740_993_u64);
        let cases = [
            (json!({"const":9007199254740992.0}), huge_integer.clone()),
            (
                json!({"const":9_007_199_254_740_993_u64}),
                json!(9007199254740992.0),
            ),
            (
                json!({"minimum":-9007199254740992.0}),
                json!(-9_007_199_254_740_993_i64),
            ),
            (json!({"enum":[9007199254740992.0]}), huge_integer.clone()),
            (json!({"minimum":9007199254740992.0}), huge_integer),
            (json!({"maximum":1e100}), json!(1e100)),
        ];
        for (schema, value) in cases {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::Inconclusive);
        }
        // Two large integral JSON tokens can still be compared exactly.
        let large = json!(u64::MAX);
        assert_eq!(
            inspect_structured_tool_output(
                &inspected_tool(Some(json!({"minimum":9_007_199_254_740_994_u64}))),
                &complete(large),
            )
            .unwrap()
            .verdict,
            McpOutputVerdict::PassedSupportedChecks,
        );
    }

    #[test]
    fn malformed_enum_const_and_bounds_hidden_in_unvisited_branches_are_inconclusive() {
        for schema in [
            json!({"properties":{"absent":{"enum":[]}}}),
            json!({"properties":{"absent":{"exclusiveMinimum":"bad"}}}),
            json!({"properties":{"absent":{"enum":"not an array"}}}),
            json!({"properties":{"absent":{"multipleOf":0}}}),
        ] {
            let verdict =
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(json!({})))
                    .unwrap()
                    .verdict;
            assert_eq!(verdict, McpOutputVerdict::Inconclusive);
        }
    }

    #[test]
    fn multiple_of_accepts_bounded_exact_decimals_and_large_integers() {
        for (schema, value) in [
            (json!({"multipleOf":0.01}), json!(4.02)),
            (json!({"multipleOf":0.1}), json!(0.3)),
            (json!({"multipleOf":0.1}), json!(-0.3)),
            (json!({"multipleOf":0.25}), json!(1.25)),
            (json!({"multipleOf":0.25}), json!(0)),
            (json!({"multipleOf":5}), json!(u64::MAX)),
            (json!({"multipleOf":0.5}), json!(7)),
            (json!({"multipleOf":3}), json!("not a number")),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict,
                McpOutputVerdict::PassedSupportedChecks,
            );
        }
        for (schema, value) in [
            (json!({"multipleOf":0.01}), json!(4.021)),
            (json!({"multipleOf":0.1}), json!(0.31)),
            (json!({"multipleOf":0.25}), json!(-0.3)),
            (json!({"multipleOf":3}), json!(10)),
            (json!({"multipleOf":5}), json!(-12)),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict,
                McpOutputVerdict::Mismatch,
            );
        }
    }

    #[test]
    fn valid_multiple_of_under_absent_property_is_not_mistaken_for_unsupported() {
        let schema = json!({"type":"object","properties":{
            "optional":{"type":"number","multipleOf":0.25}
        }});
        assert_eq!(
            inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(json!({})),)
                .unwrap()
                .verdict,
            McpOutputVerdict::PassedSupportedChecks,
        );
    }

    #[test]
    fn multiple_of_nonpositive_unrepresentable_and_absent_constraints_are_inconclusive() {
        for schema in [
            json!({"multipleOf":0}),
            json!({"multipleOf":-1}),
            json!({"multipleOf":"0.1"}),
            json!({"multipleOf":1e-25}),
            json!({"properties":{"absent":{"multipleOf":0}}}),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(json!({})))
                    .unwrap()
                    .verdict,
                McpOutputVerdict::Inconclusive,
            );
        }
        for value in [json!(1e100), json!(9007199254740992.0)] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(json!({"multipleOf":0.5}))),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::Inconclusive,
            );
        }
    }

    #[test]
    fn unique_items_uses_deep_equality_with_order_independent_objects() {
        for value in [
            json!([]),
            json!([1, 2, 3]),
            json!([{"id":1},{"id":2}]),
            json!([true, false, null]),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(json!({"type":"array","uniqueItems":true}))),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::PassedSupportedChecks,
            );
        }
        for value in [
            json!([1, 1.0]),
            json!([{"a":1,"b":2},{"b":2.0,"a":1.0}]),
            json!([[1, 2], [1.0, 2.0]]),
            json!([null, null]),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(json!({"uniqueItems":true}))),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::Mismatch,
            );
        }
        assert_eq!(
            inspect_structured_tool_output(
                &inspected_tool(Some(json!({"uniqueItems":false}))),
                &complete(json!([1, 1])),
            )
            .unwrap()
            .verdict,
            McpOutputVerdict::PassedSupportedChecks,
        );
    }

    #[test]
    fn unique_items_limits_and_uncertain_numbers_never_claim_conformance() {
        for value in [
            json!((0..17).collect::<Vec<_>>()),
            json!([9007199254740992.0, 9_007_199_254_740_993_u64]),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(json!({"uniqueItems":true}))),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::Inconclusive,
            );
        }
        for schema in [
            json!({"uniqueItems":1}),
            json!({"properties":{"unused":{"uniqueItems":"true"}}}),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(schema)),
                    &complete(json!({})),
                )
                .unwrap().verdict,
                McpOutputVerdict::Inconclusive,
            );
        }
    }

    #[test]
    fn property_names_validates_every_key_independently_of_properties() {
        for (schema, value) in [
            (
                json!({"propertyNames":{"minLength":2}}),
                json!({"ab":1,"cd":2}),
            ),
            (
                json!({"propertyNames":{"enum":["first","second"]}}),
                json!({"first":true}),
            ),
            (json!({"propertyNames":false}), json!({})),
            (json!({"type":"array","propertyNames":false}), json!([1, 2])),
            (
                json!({"properties":{"x":{"type":"integer"}},"propertyNames":{"const":"x"}}),
                json!({"x":1}),
            ),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict,
                McpOutputVerdict::PassedSupportedChecks,
            );
        }
        for (schema, value) in [
            (json!({"propertyNames":{"minLength":2}}), json!({"a":1})),
            (json!({"propertyNames":false}), json!({"x":1})),
            (
                json!({"properties":{"x":true},"propertyNames":{"const":"y"}}),
                json!({"x":1}),
            ),
            (
                json!({"propertyNames":{"type":"integer"}}),
                json!({"123":1}),
            ),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict,
                McpOutputVerdict::Mismatch,
            );
        }
    }

    #[test]
    fn prefix_items_apply_positionally_and_items_only_to_the_tail() {
        let prefix = json!({
            "type":"array",
            "prefixItems":[{"type":"string"},{"type":"integer"}],
            "items":{"type":"boolean"}
        });
        for value in [
            json!([]),
            json!(["start"]),
            json!(["start", 2]),
            json!(["start", 2, true, false]),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(prefix.clone())),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::PassedSupportedChecks,
            );
        }
        for value in [
            json!([2]),
            json!(["start", "wrong"]),
            json!(["start", 2, "bad tail"]),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(prefix.clone())),
                    &complete(value),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::Mismatch,
            );
        }
        // The tuple prefix alone does not constrain the remainder.
        for (schema, value, expected) in [
            (
                json!({"prefixItems":[{"type":"integer"}]}),
                json!([1, "unconstrained", {}]),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"prefixItems":[{"type":"integer"}],"items":false}),
                json!([1, 2]),
                McpOutputVerdict::Mismatch,
            ),
            (
                json!({"prefixItems":[{"type":"integer"}],"items":false}),
                json!([1]),
                McpOutputVerdict::PassedSupportedChecks,
            ),
            (
                json!({"items":false}),
                json!([1]),
                McpOutputVerdict::Mismatch,
            ),
            (
                json!({"prefixItems":[true,true],"minItems":2}),
                json!([1]),
                McpOutputVerdict::Mismatch,
            ),
            (
                json!({"prefixItems":[true],"items":{"type":"boolean"},"uniqueItems":true}),
                json!([1, 1]),
                McpOutputVerdict::Mismatch,
            ),
        ] {
            assert_eq!(
                inspect_structured_tool_output(&inspected_tool(Some(schema)), &complete(value))
                    .unwrap()
                    .verdict,
                expected,
            );
        }
    }

    #[test]
    fn invalid_or_unsupported_unvisited_tuple_and_property_names_fail_closed() {
        for schema in [
            json!({"prefixItems":[]}),
            json!({"prefixItems":{}}),
            json!({"prefixItems":[{"type":"number","pattern":"unsupported"}]}),
            json!({"prefixItems":[true,{"$ref":"#/definitions/no"}]}),
            json!({"prefixItems": vec![true;129]}),
            json!({"propertyNames":1}),
            json!({"propertyNames":{"pattern":"^x$"}}),
            json!({"properties":{"absent":{"propertyNames":{"$ref":"#/$defs/x"}}}}),
            json!({"properties":{"absent":{"prefixItems":[{"oneOf":[true]}]}}}),
        ] {
            assert_eq!(
                inspect_structured_tool_output(
                    &inspected_tool(Some(schema)),
                    &complete(json!({})),
                )
                .unwrap()
                .verdict,
                McpOutputVerdict::Inconclusive,
            );
        }
    }

    #[test]
    fn mismatch_diagnostics_name_exact_constraint_without_echoing_provider_data() {
        let cases = [
            (json!(false), json!(1), "boolean false schema"),
            (json!({"type":"string"}), json!(1), "type"),
            (json!({"required":["PRIVATE-PROVIDER-KEY"]}), json!({}), "required"),
            (json!({"const":{"PRIVATE-PROVIDER-KEY":1}}), json!({}), "const"),
            (json!({"enum":["PRIVATE-PROVIDER-KEY"]}), json!("other"), "enum"),
            (json!({"minLength":3}), json!("a"), "minLength"),
            (json!({"maxLength":1}), json!("ab"), "maxLength"),
            (json!({"minItems":2}), json!([1]), "minItems"),
            (json!({"maxItems":0}), json!([1]), "maxItems"),
            (json!({"minProperties":2}), json!({}), "minProperties"),
            (json!({"maxProperties":0}), json!({"PRIVATE-PROVIDER-KEY":true}), "maxProperties"),
            (json!({"minimum":2}), json!(1), "minimum"),
            (json!({"maximum":2}), json!(3), "maximum"),
            (json!({"exclusiveMinimum":2}), json!(2), "exclusiveMinimum"),
            (json!({"exclusiveMaximum":2}), json!(2), "exclusiveMaximum"),
            (json!({"multipleOf":2}), json!(3), "multipleOf"),
            (json!({"uniqueItems":true}), json!([1,1.0]), "uniqueItems"),
            (
                json!({"properties":{"PRIVATE-PROVIDER-KEY":{"minLength":4}}}),
                json!({"PRIVATE-PROVIDER-KEY":"a"}),
                "minLength",
            ),
            (
                json!({"propertyNames":{"minLength":3}}),
                json!({"PRIVATE-PROVIDER-KEY":true,"a":false}),
                "minLength",
            ),
            (
                json!({"prefixItems":[{"type":"string"}]}),
                json!([false]),
                "type",
            ),
        ];
        for (schema, value, keyword) in cases {
            let result = inspect_structured_tool_output(
                &inspected_tool(Some(schema)),
                &complete(value),
            ).unwrap();
            assert_eq!(result.verdict, McpOutputVerdict::Mismatch, "{keyword}");
            assert!(result.explanation.contains(keyword), "{keyword}");
            assert!(!result.explanation.contains("PRIVATE-PROVIDER-KEY"));
        }
    }

    #[test]
    fn unsupported_nested_constraints_remain_inconclusive_without_keyword_diagnostics() {
        let schema = json!({
            "type":"object",
            "required":["PRIVATE-PROVIDER-KEY"],
            "properties":{"unused":{"pattern":"PRIVATE-PROVIDER-KEY"}}
        });
        let result = inspect_structured_tool_output(
            &inspected_tool(Some(schema)),
            &complete(json!({})),
        ).unwrap();
        assert_eq!(result.verdict, McpOutputVerdict::Inconclusive);
        assert!(!result.explanation.contains("Constraint required failed"));
        assert!(!result.explanation.contains("PRIVATE-PROVIDER-KEY"));
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
