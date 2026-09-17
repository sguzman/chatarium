//! Structural diffing for sanitized request inventories.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

type VariantMap = BTreeMap<String, (Value, u64)>;
type EndpointIndex = BTreeMap<String, (Value, VariantMap)>;

/// Compare two request-inventory files and write a machine-readable diff report.
pub(crate) fn diff_inventory_files(
    before_path: &Path,
    after_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    let before = read_inventory(before_path)?;
    let after = read_inventory(after_path)?;
    let report = diff_inventories(&before, &after)?;
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("serialize inventory diff: {error}"))?;
    fs::write(output_path, bytes)
        .map_err(|error| format!("write {}: {error}", output_path.display()))?;

    let summary = report
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| "generated diff is missing summary".to_owned())?;
    println!(
        "added={} removed={} changed={} unchanged={}  {}",
        summary
            .get("added_endpoints")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        summary
            .get("removed_endpoints")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        summary
            .get("changed_endpoints")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        summary
            .get("unchanged_endpoints")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_path.display()
    );
    Ok(())
}

fn read_inventory(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse inventory {}: {error}", path.display()))
}

fn diff_inventories(before: &Value, after: &Value) -> Result<Value, String> {
    let before_index = index_inventory(before)?;
    let after_index = index_inventory(after)?;
    let keys = before_index
        .keys()
        .chain(after_index.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged = 0_u64;

    for key in keys {
        match (before_index.get(&key), after_index.get(&key)) {
            (None, Some((endpoint, variants))) => added.push(json!({
                "endpoint": endpoint,
                "after": variants_json(variants),
            })),
            (Some((endpoint, variants)), None) => removed.push(json!({
                "endpoint": endpoint,
                "before": variants_json(variants),
            })),
            (Some((endpoint, before_variants)), Some((_, after_variants))) => {
                if before_variants == after_variants {
                    unchanged = unchanged.saturating_add(1);
                } else {
                    changed.push(json!({
                        "endpoint": endpoint,
                        "before": variants_json(before_variants),
                        "after": variants_json(after_variants),
                    }));
                }
            }
            (None, None) => unreachable!("key came from one of the indexes"),
        }
    }

    Ok(json!({
        "format": "chatarium-request-inventory-diff",
        "version": 1,
        "summary": {
            "added_endpoints": added.len(),
            "removed_endpoints": removed.len(),
            "changed_endpoints": changed.len(),
            "unchanged_endpoints": unchanged,
        },
        "added": added,
        "removed": removed,
        "changed": changed,
    }))
}

fn index_inventory(inventory: &Value) -> Result<EndpointIndex, String> {
    validate_inventory(inventory)?;
    let entries = inventory
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "inventory is missing entries array".to_owned())?;
    let mut index = EndpointIndex::new();

    for entry in entries {
        let endpoint = endpoint_identity(entry)?;
        let endpoint_key = serde_json::to_string(&endpoint)
            .map_err(|error| format!("serialize endpoint identity: {error}"))?;
        let shape = structural_shape(entry);
        let shape_key = serde_json::to_string(&shape)
            .map_err(|error| format!("serialize endpoint shape: {error}"))?;

        let (_, variants) = index
            .entry(endpoint_key)
            .or_insert_with(|| (endpoint, VariantMap::new()));
        let (_, count) = variants.entry(shape_key).or_insert_with(|| (shape, 0_u64));
        *count = count.saturating_add(1);
    }

    Ok(index)
}

fn validate_inventory(inventory: &Value) -> Result<(), String> {
    let format = inventory
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| "inventory is missing format".to_owned())?;
    if format != "chatarium-request-inventory" {
        return Err(format!("unsupported inventory format '{format}'"));
    }

    let version = inventory
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "inventory is missing version".to_owned())?;
    if version != 1 {
        return Err(format!("unsupported inventory version {version}"));
    }
    Ok(())
}

fn endpoint_identity(entry: &Value) -> Result<Value, String> {
    let method = required_string(entry, "method")?;
    let host = required_string(entry, "host")?;
    let path = required_string(entry, "path")?;
    Ok(json!({
        "method": method,
        "host": host,
        "path": path,
    }))
}

fn structural_shape(entry: &Value) -> Value {
    json!({
        "status": entry.get("status").cloned().unwrap_or(Value::Null),
        "response_mime": entry.get("response_mime").cloned().unwrap_or(Value::Null),
        "request_mime": entry.get("request_mime").cloned().unwrap_or(Value::Null),
        "has_request_body": entry.get("has_request_body").cloned().unwrap_or(Value::Null),
        "resource_type": entry.get("resource_type").cloned().unwrap_or(Value::Null),
        "query_names": entry.get("query_names").cloned().unwrap_or_else(|| json!([])),
        "request_header_names": entry
            .get("request_header_names")
            .cloned()
            .unwrap_or_else(|| json!([])),
        "response_header_names": entry
            .get("response_header_names")
            .cloned()
            .unwrap_or_else(|| json!([])),
    })
}

fn required_string<'a>(entry: &'a Value, field: &str) -> Result<&'a str, String> {
    entry
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("inventory entry is missing string field '{field}'"))
}

fn variants_json(variants: &VariantMap) -> Vec<Value> {
    variants
        .values()
        .map(|(shape, count)| {
            json!({
                "count": count,
                "shape": shape,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(entries: Vec<Value>) -> Value {
        json!({
            "format": "chatarium-request-inventory",
            "version": 1,
            "entry_count": entries.len(),
            "entries": entries,
        })
    }

    fn entry(method: &str, path: &str, status: u64) -> Value {
        json!({
            "index": 0,
            "method": method,
            "host": "chatgpt.com",
            "path": path,
            "status": status,
            "response_mime": "application/json",
            "request_mime": null,
            "has_request_body": false,
            "resource_type": "fetch",
            "query_names": [],
            "request_header_names": ["accept"],
            "response_header_names": ["content-type"],
        })
    }

    #[test]
    fn identical_inventories_are_unchanged() {
        let value = inventory(vec![entry("GET", "/backend-api/example", 200)]);
        let report = diff_inventories(&value, &value).expect("diff");
        assert_eq!(
            report.pointer("/summary/unchanged_endpoints"),
            Some(&json!(1))
        );
        assert_eq!(
            report.pointer("/summary/changed_endpoints"),
            Some(&json!(0))
        );
    }

    #[test]
    fn status_change_is_structural_change_not_add_remove() {
        let before = inventory(vec![entry("GET", "/backend-api/example", 200)]);
        let after = inventory(vec![entry("GET", "/backend-api/example", 429)]);
        let report = diff_inventories(&before, &after).expect("diff");
        assert_eq!(
            report.pointer("/summary/changed_endpoints"),
            Some(&json!(1))
        );
        assert_eq!(report.pointer("/summary/added_endpoints"), Some(&json!(0)));
        assert_eq!(
            report.pointer("/summary/removed_endpoints"),
            Some(&json!(0))
        );
    }

    #[test]
    fn endpoint_addition_is_reported() {
        let before = inventory(vec![entry("GET", "/backend-api/one", 200)]);
        let after = inventory(vec![
            entry("GET", "/backend-api/one", 200),
            entry("POST", "/backend-api/two", 200),
        ]);
        let report = diff_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/added_endpoints"), Some(&json!(1)));
        assert_eq!(
            report.pointer("/summary/unchanged_endpoints"),
            Some(&json!(1))
        );
    }

    #[test]
    fn repeated_shape_count_change_is_detected() {
        let single = entry("GET", "/backend-api/repeated", 200);
        let before = inventory(vec![single.clone()]);
        let after = inventory(vec![single.clone(), single]);
        let report = diff_inventories(&before, &after).expect("diff");
        assert_eq!(
            report.pointer("/summary/changed_endpoints"),
            Some(&json!(1))
        );
    }
}
