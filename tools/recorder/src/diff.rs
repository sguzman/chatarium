//! Structural diffing for sanitized Chatarium protocol inventories.

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
    let before_format = inventory_format(&before)?;
    let report = diff_inventory_values(&before, &after)?;
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("serialize inventory diff: {error}"))?;
    fs::write(output_path, bytes)
        .map_err(|error| format!("write {}: {error}", output_path.display()))?;

    let summary = report
        .get("summary")
        .and_then(Value::as_object)
        .ok_or_else(|| "generated diff is missing summary".to_owned())?;
    if before_format == "chatarium-request-inventory" {
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
    } else {
        println!(
            "added={} removed={} changed={}  {}",
            summary
                .get("added_paths")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            summary
                .get("removed_paths")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            summary
                .get("changed_paths")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            output_path.display()
        );
    }
    Ok(())
}

fn read_inventory(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse inventory {}: {error}", path.display()))
}

fn diff_request_inventories(before: &Value, after: &Value) -> Result<Value, String> {
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

fn diff_inventory_values(before: &Value, after: &Value) -> Result<Value, String> {
    let before_format = inventory_format(before)?;
    let after_format = inventory_format(after)?;
    if before_format != after_format {
        return Err(format!(
            "cannot compare inventory formats '{before_format}' and '{after_format}'"
        ));
    }

    match before_format {
        "chatarium-request-inventory" => diff_request_inventories(before, after),
        "chatarium-flight-inventory" => diff_flight_inventories(before, after),
        other => Err(format!("unsupported inventory format '{other}'")),
    }
}

fn inventory_format(inventory: &Value) -> Result<&str, String> {
    inventory
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| "inventory is missing format".to_owned())
}

fn diff_flight_inventories(before: &Value, after: &Value) -> Result<Value, String> {
    validate_flight_inventory(before)?;
    validate_flight_inventory(after)?;

    let before_experiment = required_string(before, "experiment_id")?;
    let after_experiment = required_string(after, "experiment_id")?;
    if before_experiment != after_experiment {
        return Err(format!(
            "cannot compare Flight Recorder inventories for different experiments '{before_experiment}' and '{after_experiment}'"
        ));
    }

    let before_normalized = normalize_flight_inventory(before)?;
    let after_normalized = normalize_flight_inventory(after)?;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    diff_json_values(
        "",
        &before_normalized,
        &after_normalized,
        &mut added,
        &mut removed,
        &mut changed,
    );

    Ok(json!({
        "format": "chatarium-flight-inventory-diff",
        "version": 1,
        "experiment_id": before_experiment,
        "context": {
            "before": flight_context(before),
            "after": flight_context(after),
        },
        "summary": {
            "added_paths": added.len(),
            "removed_paths": removed.len(),
            "changed_paths": changed.len(),
            "total_changes": added.len() + removed.len() + changed.len(),
        },
        "added": added,
        "removed": removed,
        "changed": changed,
    }))
}

fn validate_flight_inventory(inventory: &Value) -> Result<(), String> {
    let format = inventory_format(inventory)?;
    if format != "chatarium-flight-inventory" {
        return Err(format!("unsupported Flight Recorder inventory format '{format}'"));
    }
    let version = inventory
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Flight Recorder inventory is missing version".to_owned())?;
    if version != 1 {
        return Err(format!(
            "unsupported Flight Recorder inventory version {version}"
        ));
    }
    required_string(inventory, "experiment_id")?;
    inventory
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| "Flight Recorder inventory is missing streams array".to_owned())?;
    Ok(())
}

fn flight_context(inventory: &Value) -> Value {
    json!({
        "recorder_version": inventory.get("recorder_version").cloned().unwrap_or(Value::Null),
        "selected_run": inventory.get("selected_run").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_flight_inventory(inventory: &Value) -> Result<Value, String> {
    let streams = inventory
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| "Flight Recorder inventory is missing streams array".to_owned())?;
    let mut stream_counts = BTreeMap::<String, u64>::new();
    let mut normalized_streams = serde_json::Map::new();

    for stream in streams {
        let method = required_string(stream, "method")?;
        let endpoint = required_string(stream, "endpoint")?;
        let base = format!("{method} {endpoint}");
        let ordinal = stream_counts.entry(base.clone()).or_default();
        *ordinal = ordinal.saturating_add(1);
        let key = if *ordinal == 1 {
            base
        } else {
            format!("{base} #{}", *ordinal)
        };
        normalized_streams.insert(
            key,
            json!({
                "status": stream.get("status").cloned().unwrap_or(Value::Null),
                "content_type": stream.get("content_type").cloned().unwrap_or(Value::Null),
                "frame_count": stream.get("frame_count").cloned().unwrap_or(Value::Null),
                "named_event_counts": stream.get("named_event_counts").cloned().unwrap_or_else(|| json!({})),
                "control_type_counts": stream.get("control_type_counts").cloned().unwrap_or_else(|| json!({})),
                "delta_operation_counts": stream.get("delta_operation_counts").cloned().unwrap_or_else(|| json!({})),
                "delta_path_counts": stream.get("delta_path_counts").cloned().unwrap_or_else(|| json!({})),
                "marker_counts": stream.get("marker_counts").cloned().unwrap_or_else(|| json!({})),
                "delta_encodings": stream.get("delta_encodings").cloned().unwrap_or_else(|| json!([])),
                "completion": stream.get("completion").cloned().unwrap_or_else(|| json!({})),
                "parse_warning_count": stream.get("parse_warning_count").cloned().unwrap_or(Value::Null),
            }),
        );
    }

    let mut event_kind_counts = inventory
        .get("event_kind_counts")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    event_kind_counts.remove("network-stream-chunk");

    Ok(json!({
        "experiment_id": inventory.get("experiment_id").cloned().unwrap_or(Value::Null),
        "event_kind_counts": Value::Object(event_kind_counts),
        "send_state_counts": inventory.get("send_state_counts").cloned().unwrap_or_else(|| json!({})),
        "confirmation_evidence_counts": inventory.get("confirmation_evidence_counts").cloned().unwrap_or_else(|| json!({})),
        "message_role_counts": inventory.get("message_role_counts").cloned().unwrap_or_else(|| json!({})),
        "message_source_counts": inventory.get("message_source_counts").cloned().unwrap_or_else(|| json!({})),
        "assistant_wal": inventory.get("assistant_wal").cloned().unwrap_or(Value::Null),
        "streams": Value::Object(normalized_streams),
        "warning_count": inventory.get("warning_count").cloned().unwrap_or(Value::Null),
    }))
}

fn diff_json_values(
    path: &str,
    before: &Value,
    after: &Value,
    added: &mut Vec<Value>,
    removed: &mut Vec<Value>,
    changed: &mut Vec<Value>,
) {
    match (before, after) {
        (Value::Object(before_map), Value::Object(after_map)) => {
            let keys = before_map
                .keys()
                .chain(after_map.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = format!("{}/{}", path, escape_pointer_segment(&key));
                match (before_map.get(&key), after_map.get(&key)) {
                    (None, Some(value)) => {
                        added.push(json!({"path": child_path, "after": value}));
                    }
                    (Some(value), None) => {
                        removed.push(json!({"path": child_path, "before": value}));
                    }
                    (Some(before_value), Some(after_value)) => diff_json_values(
                        &child_path,
                        before_value,
                        after_value,
                        added,
                        removed,
                        changed,
                    ),
                    (None, None) => unreachable!("key came from one of the objects"),
                }
            }
        }
        _ if before != after => changed.push(json!({
            "path": if path.is_empty() { "/" } else { path },
            "before": before,
            "after": after,
        })),
        _ => {}
    }
}

fn escape_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn index_inventory(inventory: &Value) -> Result<EndpointIndex, String> {
    validate_request_inventory(inventory)?;
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

fn validate_request_inventory(inventory: &Value) -> Result<(), String> {
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

    fn flight_inventory() -> Value {
        json!({
            "format": "chatarium-flight-inventory",
            "version": 1,
            "experiment_id": "C03-send-text",
            "recorder_version": "0.6.0",
            "selected_run": {
                "started_seq": 25,
                "started_at": "2026-09-29T11:33:42Z",
                "event_count": 37
            },
            "event_kind_counts": {
                "network-stream-start": 1,
                "network-stream-chunk": 15,
                "network-stream-end": 1
            },
            "send_state_counts": {"confirmed": 1},
            "confirmation_evidence_counts": {"protocol-input-message": 1},
            "message_role_counts": {"assistant": 1, "user": 1},
            "message_source_counts": {"protocol-sse": 2},
            "assistant_wal": {
                "present": true,
                "source": "protocol-sse",
                "protocol_status": "finished_successfully",
                "protocol_end_turn": true,
                "protocol_is_complete": true,
                "text_is_experiment_allowed": true
            },
            "streams": [{
                "stream": "stream-1",
                "endpoint": "/backend-api/f/conversation",
                "method": "POST",
                "status": 200,
                "content_type": "text/event-stream; charset=utf-8",
                "frame_count": 29,
                "named_event_counts": {"delta": 16, "delta_encoding": 1},
                "control_type_counts": {"message_stream_complete": 1},
                "delta_operation_counts": {"append": 2, "patch": 1, "replace": 4},
                "delta_path_counts": {"/message/content/parts/0": 1},
                "marker_counts": {"last_token:last": 1},
                "delta_encodings": ["v1"],
                "completion": {
                    "message_stream_complete": true,
                    "done": true,
                    "terminal": "sse-done"
                },
                "parse_warning_count": 0
            }],
            "warning_count": 0
        })
    }

    #[test]
    fn identical_flight_inventories_have_no_changes() {
        let value = flight_inventory();
        let report = diff_flight_inventories(&value, &value).expect("diff");
        assert_eq!(report.pointer("/summary/total_changes"), Some(&json!(0)));
    }

    #[test]
    fn volatile_flight_context_does_not_count_as_protocol_change() {
        let before = flight_inventory();
        let mut after = before.clone();
        after["recorder_version"] = json!("0.7.0");
        after["selected_run"]["started_seq"] = json!(900);
        after["selected_run"]["started_at"] = json!("2026-10-01T00:00:00Z");
        after["selected_run"]["event_count"] = json!(99);

        let report = diff_flight_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/total_changes"), Some(&json!(0)));
        assert_eq!(
            report.pointer("/context/after/recorder_version"),
            Some(&json!("0.7.0"))
        );
    }

    #[test]
    fn flight_control_type_addition_is_reported() {
        let before = flight_inventory();
        let mut after = before.clone();
        after["streams"][0]["control_type_counts"]["new_control"] = json!(1);

        let report = diff_flight_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/added_paths"), Some(&json!(1)));
        assert_eq!(
            report.pointer("/added/0/path"),
            Some(&json!("/streams/POST ~1backend-api~1f~1conversation/control_type_counts/new_control"))
        );
    }

    #[test]
    fn flight_operation_count_and_completion_changes_are_reported() {
        let before = flight_inventory();
        let mut after = before.clone();
        after["streams"][0]["delta_operation_counts"]["append"] = json!(3);
        after["streams"][0]["completion"]["done"] = json!(false);

        let report = diff_flight_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/changed_paths"), Some(&json!(2)));
    }

    #[test]
    fn flight_stream_addition_is_reported() {
        let before = flight_inventory();
        let mut after = before.clone();
        let second = json!({
            "stream": "stream-2",
            "endpoint": "/backend-api/f/other",
            "method": "GET",
            "status": 200,
            "content_type": "application/json",
            "frame_count": 0,
            "named_event_counts": {},
            "control_type_counts": {},
            "delta_operation_counts": {},
            "delta_path_counts": {},
            "marker_counts": {},
            "delta_encodings": [],
            "completion": {},
            "parse_warning_count": 0
        });
        after["streams"].as_array_mut().unwrap().push(second);

        let report = diff_flight_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/added_paths"), Some(&json!(1)));
    }

    #[test]
    fn different_flight_experiments_are_rejected() {
        let before = flight_inventory();
        let mut after = before.clone();
        after["experiment_id"] = json!("C04-stop-generation");
        let error = diff_flight_inventories(&before, &after).unwrap_err();
        assert!(error.contains("different experiments"));
    }

    #[test]
    fn mixed_inventory_formats_are_rejected() {
        let request = inventory(vec![entry("GET", "/backend-api/example", 200)]);
        let flight = flight_inventory();
        let error = diff_inventory_values(&request, &flight).unwrap_err();
        assert!(error.contains("cannot compare inventory formats"));
    }

    #[test]
    fn browser_chunk_count_does_not_create_flight_protocol_diff() {
        let before = flight_inventory();
        let mut after = before.clone();
        after["event_kind_counts"]["network-stream-chunk"] = json!(99);

        let report = diff_flight_inventories(&before, &after).expect("diff");
        assert_eq!(report.pointer("/summary/total_changes"), Some(&json!(0)));
    }

    #[test]
    fn identical_inventories_are_unchanged() {
        let value = inventory(vec![entry("GET", "/backend-api/example", 200)]);
        let report = diff_request_inventories(&value, &value).expect("diff");
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
        let report = diff_request_inventories(&before, &after).expect("diff");
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
        let report = diff_request_inventories(&before, &after).expect("diff");
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
        let report = diff_request_inventories(&before, &after).expect("diff");
        assert_eq!(
            report.pointer("/summary/changed_endpoints"),
            Some(&json!(1))
        );
    }
}
