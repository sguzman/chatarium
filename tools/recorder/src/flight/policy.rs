//! policy logic mechanically isolated from the fail-closed flight sanitization pipeline.
use super::*;

pub(super) fn sanitize_protocol_json(
    value: &mut Value,
    key: Option<&str>,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) {
    match value {
        Value::Null | Value::Bool(_) => {}
        Value::Number(_) => {
            stats.numeric_values_generalized = stats.numeric_values_generalized.saturating_add(1);
            *value = Value::String(NUMBER.to_owned());
        }
        Value::String(text) => {
            let field = key.unwrap_or_default();
            if sensitive_field(field) {
                stats.sensitive_values_redacted = stats.sensitive_values_redacted.saturating_add(1);
                *text = REDACTED.to_owned();
            } else if conversation_identity_field(field) {
                stats.conversation_identities_placeholdered = stats
                    .conversation_identities_placeholdered
                    .saturating_add(1);
                *text = ids.map_conversation(text);
            } else if message_identity_field(field) {
                stats.message_identities_placeholdered =
                    stats.message_identities_placeholdered.saturating_add(1);
                *text = ids.map_message(text);
            } else if generic_identity_field(field) {
                stats.generic_identities_placeholdered =
                    stats.generic_identities_placeholdered.saturating_add(1);
                *text = "<id>".to_owned();
            } else if content_field(field) {
                if allowed_texts.contains(text) {
                    stats.canonical_experiment_literals_retained = stats
                        .canonical_experiment_literals_retained
                        .saturating_add(1);
                } else {
                    stats.content_values_redacted = stats.content_values_redacted.saturating_add(1);
                    *text = REDACTED_CONTENT.to_owned();
                }
            } else if field == "p" {
                let sanitized = safe_protocol_path(text);
                if sanitized != *text {
                    stats.unknown_paths_redacted = stats.unknown_paths_redacted.saturating_add(1);
                    *text = sanitized;
                }
            } else if structural_string_field(field) {
                let sanitized = safe_structural_value(field, text);
                if sanitized != *text {
                    stats.unknown_structural_values_redacted =
                        stats.unknown_structural_values_redacted.saturating_add(1);
                    *text = sanitized;
                }
            } else if allowed_texts.contains(text) {
                stats.canonical_experiment_literals_retained = stats
                    .canonical_experiment_literals_retained
                    .saturating_add(1);
            } else {
                stats.unknown_scalar_values_redacted =
                    stats.unknown_scalar_values_redacted.saturating_add(1);
                *text = REDACTED_VALUE.to_owned();
            }
        }
        Value::Array(items) => {
            let content_array = matches!(key, Some("parts"));
            for item in items {
                if content_array {
                    match item {
                        Value::String(text) if allowed_texts.contains(text) => {
                            stats.canonical_experiment_literals_retained = stats
                                .canonical_experiment_literals_retained
                                .saturating_add(1);
                        }
                        Value::String(text) => {
                            stats.content_values_redacted =
                                stats.content_values_redacted.saturating_add(1);
                            *text = REDACTED_CONTENT.to_owned();
                        }
                        nested => {
                            sanitize_protocol_json(nested, key, allowed_texts, ids, stats);
                        }
                    }
                } else {
                    sanitize_protocol_json(item, key, allowed_texts, ids, stats);
                }
            }
        }
        Value::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for child_key in keys {
                if let Some(nested) = map.get_mut(&child_key) {
                    sanitize_protocol_json(nested, Some(&child_key), allowed_texts, ids, stats);
                }
            }
        }
    }
}

pub(super) fn sensitive_field(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    normalized == "token"
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.contains("authorization")
        || normalized.contains("cookie")
        || normalized.contains("sentinel")
}

pub(super) fn conversation_identity_field(key: &str) -> bool {
    key.eq_ignore_ascii_case("conversation_id")
}

pub(super) fn message_identity_field(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "id" | "message_id" | "parent_id"
    )
}

pub(super) fn generic_identity_field(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.ends_with("_id")
}

pub(super) fn content_field(key: &str) -> bool {
    matches!(key, "text" | "content" | "title")
}

pub(super) fn structural_string_field(key: &str) -> bool {
    matches!(
        key,
        "type"
            | "kind"
            | "role"
            | "content_type"
            | "status"
            | "channel"
            | "recipient"
            | "o"
            | "marker"
            | "event"
            | "message_type"
            | "reasoning_status"
            | "reasoning_recap_type"
    )
}

pub(super) fn safe_sse_event_name(value: &str) -> String {
    match value {
        "delta" | "delta_encoding" => value.to_owned(),
        _ => "<redacted-event-name>".to_owned(),
    }
}

pub(super) fn safe_structural_value(field: &str, value: &str) -> String {
    let allowed = match field {
        "type" => matches!(
            value,
            "resume_conversation_token"
                | "input_message"
                | "title_generation"
                | "message_marker"
                | "server_ste_metadata"
                | "message_stream_complete"
                | "conversation_detail_metadata"
                | "text"
                | "reasoning_recap"
                | "model_editable_context"
                | "stop"
        ),
        "kind" => matches!(value, "topic"),
        "role" => matches!(
            value,
            "user" | "assistant" | "system" | "developer" | "tool"
        ),
        "content_type" => matches!(value, "text" | "reasoning_recap" | "model_editable_context"),
        "status" => matches!(value, "finished_successfully" | "in_progress"),
        "channel" => matches!(value, "final"),
        "recipient" => matches!(value, "all"),
        "o" => matches!(value, "add" | "append" | "patch" | "replace"),
        "marker" => matches!(
            value,
            "cot_token" | "user_visible_token" | "final_channel_token" | "last_token"
        ),
        "event" => matches!(value, "first" | "last"),
        "message_type" => matches!(value, "next"),
        "reasoning_status" => matches!(value, "reasoning_ended"),
        "reasoning_recap_type" => matches!(value, "hide_all"),
        _ => false,
    };
    if allowed {
        value.to_owned()
    } else {
        REDACTED_VALUE.to_owned()
    }
}

pub(super) fn safe_protocol_path(value: &str) -> String {
    match value {
        ""
        | "/message/content/parts/0"
        | "/message/status"
        | "/message/end_turn"
        | "/message/metadata"
        | "/message/metadata/conversation_followup_suggestions_eligible" => value.to_owned(),
        _ => "<redacted-path>".to_owned(),
    }
}

pub(super) fn count_field(values: &[Value], field: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for value in values {
        if let Some(text) = value.get(field).and_then(Value::as_str) {
            *counts.entry(text.to_owned()).or_default() += 1;
        }
    }
    counts
}
