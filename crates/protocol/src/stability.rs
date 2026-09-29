//! Evidence-scoped field classification for protocol comparison.
//!
//! The registry is derived from observed snapshots. structural_candidate is
//! intentionally weaker than stable or required.

use serde_json::Value;
use std::sync::OnceLock;

/// Classification of one inventory change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A path appeared in the later observation.
    Added,
    /// A path disappeared from the later observation.
    Removed,
    /// A path remained present but its value changed.
    Changed,
}

/// Evidence-scoped annotation returned for one field/path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldAnnotation {
    /// Registry class such as structural_candidate or diagnostic.
    pub field_class: String,
    /// Evidence-scoped rationale from the registry.
    pub rationale: String,
}

const REGISTRY_TEXT: &str = include_str!("../../../protocol/schemas/field-classification.v1.json");
const ALLOWED_CLASSES: &[&str] = &[
    "structural_candidate",
    "ephemeral_instance",
    "delivery_noise",
    "observation_count",
    "diagnostic",
    "controlled_fixture",
    "unknown",
];

static REGISTRY: OnceLock<Value> = OnceLock::new();

/// Return the exact committed field-classification registry parsed as JSON.
#[must_use]
pub fn registry() -> &'static Value {
    REGISTRY.get_or_init(|| {
        serde_json::from_str(REGISTRY_TEXT)
            .expect("committed protocol field-classification registry must be valid JSON")
    })
}

/// Validate a field-classification registry value.
pub fn validate_registry(value: &Value) -> Result<(), String> {
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-field-classification") {
        return Err("field classification registry has unsupported schema".to_owned());
    }
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("field classification registry has unsupported version".to_owned());
    }

    let evidence = value
        .get("evidence_revisions")
        .and_then(Value::as_array)
        .ok_or_else(|| "field classification registry is missing evidence_revisions".to_owned())?;
    if evidence.is_empty() || evidence.iter().any(|item| item.as_str().is_none()) {
        return Err(
            "field classification evidence_revisions must be a non-empty string array".to_owned(),
        );
    }

    let caveat = value
        .get("caveat")
        .and_then(Value::as_str)
        .ok_or_else(|| "field classification registry is missing caveat".to_owned())?;
    if !caveat.contains("not a claim") {
        return Err(
            "field classification caveat must explicitly deny a stability/API claim".to_owned(),
        );
    }

    let classes = value
        .get("classes")
        .and_then(Value::as_object)
        .ok_or_else(|| "field classification registry is missing classes".to_owned())?;
    for class in ALLOWED_CLASSES {
        if classes.get(*class).and_then(Value::as_str).is_none() {
            return Err(format!(
                "field classification registry is missing class definition '{class}'"
            ));
        }
    }
    for class in classes.keys() {
        if !ALLOWED_CLASSES.contains(&class.as_str()) {
            return Err(format!(
                "field classification registry has unknown class '{class}'"
            ));
        }
    }

    let flight = value
        .get("flight_inventory")
        .and_then(Value::as_object)
        .ok_or_else(|| "field classification registry is missing flight_inventory".to_owned())?;
    for required in [
        "excluded_context",
        "sections",
        "assistant_wal_fields",
        "stream_presence",
        "stream_fields",
    ] {
        let valid = flight.get(required).and_then(Value::as_object).is_some();
        if !valid {
            return Err(format!(
                "field classification registry flight_inventory is missing '{required}'"
            ));
        }
    }

    validate_annotation_classes(value, "")?;
    Ok(())
}

fn validate_annotation_classes(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let child = format!("{path}/{key}");
                if matches!(key.as_str(), "class" | "key_class" | "value_class") {
                    let Some(class) = nested.as_str() else {
                        return Err(format!("{child} must be a string"));
                    };
                    if !ALLOWED_CLASSES.contains(&class) {
                        return Err(format!("{child} has unknown field class '{class}'"));
                    }
                }
                validate_annotation_classes(nested, &child)?;
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                validate_annotation_classes(nested, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Classify a Flight Recorder inventory diff path using the committed registry.
#[must_use]
pub fn classify_flight_inventory_change(path: &str, kind: ChangeKind) -> FieldAnnotation {
    let segments = path
        .split('/')
        .skip(1)
        .map(unescape_pointer_segment)
        .collect::<Vec<_>>();

    if segments.is_empty() || segments[0].is_empty() {
        return unknown("No field path was available for classification.");
    }

    if segments[0] == "streams" {
        if segments.len() == 2 {
            return classify_entry(
                registry().pointer("/flight_inventory/stream_presence"),
                kind,
                false,
            );
        }
        if segments.len() < 3 {
            return unknown(
                "The change addresses the stream collection rather than a classified stream.",
            );
        }
        return classify_entry(
            registry()
                .pointer("/flight_inventory/stream_fields")
                .and_then(Value::as_object)
                .and_then(|map| map.get(&segments[2])),
            kind,
            segments.len() > 3,
        );
    }

    if segments[0] == "assistant_wal" && segments.len() >= 2 {
        if let Some(entry) = registry()
            .pointer("/flight_inventory/assistant_wal_fields")
            .and_then(Value::as_object)
            .and_then(|map| map.get(&segments[1]))
        {
            return classify_entry(Some(entry), kind, segments.len() > 2);
        }
    }

    classify_entry(
        registry()
            .pointer("/flight_inventory/sections")
            .and_then(Value::as_object)
            .and_then(|map| map.get(&segments[0])),
        kind,
        segments.len() > 1,
    )
}

/// Return the registry annotation for context deliberately excluded from Flight diffs.
#[must_use]
pub fn classify_excluded_flight_context(name: &str) -> FieldAnnotation {
    classify_entry(
        registry()
            .pointer("/flight_inventory/excluded_context")
            .and_then(Value::as_object)
            .and_then(|map| map.get(name)),
        ChangeKind::Changed,
        false,
    )
}

fn classify_entry(entry: Option<&Value>, kind: ChangeKind, nested: bool) -> FieldAnnotation {
    let Some(entry) = entry.and_then(Value::as_object) else {
        return unknown("Current evidence has no explicit classification for this path.");
    };

    let class = if let Some(class) = entry.get("class").and_then(Value::as_str) {
        class
    } else if nested && matches!(kind, ChangeKind::Added | ChangeKind::Removed) {
        entry
            .get("key_class")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    } else {
        entry
            .get("value_class")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };

    FieldAnnotation {
        field_class: class.to_owned(),
        rationale: entry
            .get("rationale")
            .and_then(Value::as_str)
            .unwrap_or("Current evidence provides no rationale.")
            .to_owned(),
    }
}

fn unknown(rationale: &str) -> FieldAnnotation {
    FieldAnnotation {
        field_class: "unknown".to_owned(),
        rationale: rationale.to_owned(),
    }
}

fn unescape_pointer_segment(value: &str) -> String {
    value.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_registry_is_valid() {
        validate_registry(registry()).unwrap();
        assert!(
            registry()
                .get("caveat")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains("not a claim"))
        );
    }

    #[test]
    fn added_control_type_is_structural_candidate() {
        let annotation = classify_flight_inventory_change(
            "/streams/POST ~1backend-api~1f~1conversation/control_type_counts/new_control",
            ChangeKind::Added,
        );
        assert_eq!(annotation.field_class, "structural_candidate");
    }

    #[test]
    fn changed_control_type_count_is_observation_count() {
        let annotation = classify_flight_inventory_change(
            "/streams/POST ~1backend-api~1f~1conversation/control_type_counts/message_stream_complete",
            ChangeKind::Changed,
        );
        assert_eq!(annotation.field_class, "observation_count");
    }

    #[test]
    fn added_stream_is_structural_candidate() {
        let annotation = classify_flight_inventory_change(
            "/streams/GET ~1backend-api~1f~1other",
            ChangeKind::Added,
        );
        assert_eq!(annotation.field_class, "structural_candidate");
    }

    #[test]
    fn completion_is_structural_candidate() {
        let annotation = classify_flight_inventory_change(
            "/streams/POST ~1backend-api~1f~1conversation/completion/done",
            ChangeKind::Changed,
        );
        assert_eq!(annotation.field_class, "structural_candidate");
    }

    #[test]
    fn parse_warning_is_diagnostic() {
        let annotation = classify_flight_inventory_change(
            "/streams/POST ~1backend-api~1f~1conversation/parse_warning_count",
            ChangeKind::Changed,
        );
        assert_eq!(annotation.field_class, "diagnostic");
    }

    #[test]
    fn excluded_browser_chunk_count_is_delivery_noise() {
        let annotation = classify_excluded_flight_context("event_kind_counts.network-stream-chunk");
        assert_eq!(annotation.field_class, "delivery_noise");
    }

    #[test]
    fn unknown_path_remains_unknown() {
        let annotation = classify_flight_inventory_change("/future/new_field", ChangeKind::Added);
        assert_eq!(annotation.field_class, "unknown");
    }
}
