//! read logic mechanically isolated from the fail-closed flight sanitization pipeline.
use super::*;

pub(super) fn sanitize_read_capture_metadata(selected: &SelectedRun) -> Value {
    let arm = selected.events.iter().find(|event| {
        event.get("kind").and_then(Value::as_str) == Some("protocol-read-capture-armed")
    });
    let disarm = selected.events.iter().find(|event| {
        event.get("kind").and_then(Value::as_str) == Some("protocol-read-capture-disarmed")
    });

    let response_count = selected
        .events
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("protocol-read-response-captured")
        })
        .count() as u64;
    let captured_bytes = selected
        .events
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("protocol-read-response-captured")
        })
        .filter_map(|event| {
            event
                .pointer("/payload/capturedBytes")
                .and_then(Value::as_u64)
        })
        .sum::<u64>();
    let request_count = selected
        .events
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("protocol-read-request-observed")
        })
        .count() as u64;
    let skipped_count = selected
        .events
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("protocol-read-response-skipped")
        })
        .count() as u64;
    let error_count = selected
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.get("kind").and_then(Value::as_str),
                Some("protocol-read-response-error" | "protocol-read-fetch-error")
            )
        })
        .count() as u64;

    let intervals = arm
        .map(|arm| {
            vec![json!({
                "armed_at": arm.get("at"),
                "disarmed_at": disarm.and_then(|event| event.get("at")),
                "reason": disarm.and_then(|event| event.pointer("/payload/reason")),
            })]
        })
        .unwrap_or_default();

    json!({
        "present": arm.is_some(),
        "armed": arm.is_some() && disarm.is_none(),
        "armed_at": arm.and_then(|event| event.get("at")),
        "captured_bytes": captured_bytes,
        "request_count": request_count,
        "response_count": response_count,
        "skipped_count": skipped_count,
        "error_count": error_count,
        "response_limit_bytes": arm
            .and_then(|event| event.pointer("/payload/responseLimitBytes"))
            .and_then(Value::as_u64),
        "run_limit_bytes": arm
            .and_then(|event| event.pointer("/payload/runLimitBytes"))
            .and_then(Value::as_u64),
        "intervals": intervals,
    })
}

pub(super) fn sanitize_read_response_event(
    event: &Value,
    public_id: &str,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
    warnings: &mut Vec<String>,
) -> Result<(Value, Value), String> {
    let seq = event.get("seq").and_then(Value::as_u64).unwrap_or_default();
    let payload = event
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("seq {seq}: protocol read response is missing object payload"))?;

    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("seq {seq}: protocol read response is missing method"))?;
    if !matches!(method, "GET" | "HEAD") {
        return Err(format!(
            "seq {seq}: protocol read response used non-read method {method}"
        ));
    }

    let raw_path = payload
        .get("endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("seq {seq}: protocol read response is missing endpoint"))?;
    if !raw_path.starts_with("/backend-api/") {
        return Err(format!(
            "seq {seq}: protocol read response escaped /backend-api/: {raw_path}"
        ));
    }
    let path = normalize_read_path(raw_path, ids, stats);

    let query_keys = payload
        .get("queryKeys")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("seq {seq}: protocol read response is missing queryKeys"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("seq {seq}: protocol read query key is not a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let query_parameters =
        sanitize_read_query_evidence(payload, raw_path, method, &query_keys, seq, stats)?;

    let content_type = payload
        .get("contentType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !json_content_type(content_type) {
        return Err(format!(
            "seq {seq}: captured protocol read response has unsupported content type {content_type:?}"
        ));
    }

    let status = payload.get("status").and_then(Value::as_i64);
    let captured_bytes = payload.get("capturedBytes").and_then(Value::as_u64);
    let truncated = payload
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body_present = payload
        .get("bodyPresent")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let (body, top_level_type, shape, body_warning) = if method == "HEAD" || !body_present {
        (Value::Null, None, Vec::new(), None)
    } else if truncated {
        stats.read_truncated_bodies_omitted = stats.read_truncated_bodies_omitted.saturating_add(1);
        warnings.push(format!(
            "seq {seq}: {public_id} read body was truncated and omitted from public evidence"
        ));
        (
            Value::String(TRUNCATED_READ_BODY.to_owned()),
            None,
            Vec::new(),
            Some("truncated-private-json-omitted"),
        )
    } else {
        let raw = payload
            .get("bodyText")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("seq {seq}: GET read response body is missing bodyText"))?;
        let parsed: Value = serde_json::from_str(raw).map_err(|error| {
            stats.malformed_read_json_fail_closed =
                stats.malformed_read_json_fail_closed.saturating_add(1);
            format!("seq {seq}: declared JSON protocol read body failed closed: {error}")
        })?;
        let top = json_type_name(&parsed).to_owned();
        let mut shape = Vec::new();
        collect_read_shape(&parsed, "", &mut shape);
        let reduced = reduce_read_json(&parsed, None, allowed_texts, ids, stats);
        stats.read_responses_reduced = stats.read_responses_reduced.saturating_add(1);
        (reduced, Some(top), shape, None)
    };

    let mut sanitized = json!({
        "read": public_id,
        "method": method,
        "path": path,
        "query_keys": query_keys,
        "status": status,
        "content_type": content_type,
        "captured_bytes": captured_bytes,
        "truncated": truncated,
        "body_present": body_present,
        "body": body,
    });
    if let Some(parameters) = query_parameters.clone() {
        sanitized["query_parameters"] = parameters;
    }

    let mut inventory = json!({
        "read": public_id,
        "method": method,
        "path": sanitized.get("path"),
        "query_keys": sanitized.get("query_keys"),
        "status": status,
        "content_type": content_type,
        "truncated": truncated,
        "body_present": body_present,
        "top_level_type": top_level_type,
        "shape": shape,
        "warning": body_warning,
    });
    if let Some(parameters) = query_parameters {
        inventory["query_parameters"] = parameters;
    }

    Ok((sanitized, inventory))
}

pub(super) fn sanitize_read_query_evidence(
    payload: &Map<String, Value>,
    raw_path: &str,
    method: &str,
    query_keys: &[String],
    seq: u64,
    stats: &mut FlightSanitizationStats,
) -> Result<Option<Value>, String> {
    let Some(raw) = payload.get("queryEvidence") else {
        return Ok(None);
    };

    if !matches!(method, "GET" | "HEAD") || !is_c02_conversation_resource_path(raw_path) {
        return Err(format!(
            "seq {seq}: queryEvidence is allowed only for C02 conversation-resource reads"
        ));
    }

    let entries = raw
        .as_array()
        .ok_or_else(|| format!("seq {seq}: queryEvidence must be an array"))?;
    let mut sanitized = Vec::with_capacity(entries.len());

    for (index, entry) in entries.iter().enumerate() {
        let object = entry
            .as_object()
            .ok_or_else(|| format!("seq {seq}: queryEvidence entry {index} must be an object"))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "key" | "value" | "unsupported"))
        {
            return Err(format!(
                "seq {seq}: queryEvidence entry {index} contains unsupported fields"
            ));
        }

        let key = object
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("seq {seq}: queryEvidence entry {index} is missing key"))?;
        if !query_keys.iter().any(|candidate| candidate == key) {
            return Err(format!(
                "seq {seq}: queryEvidence entry {index} key is absent from queryKeys"
            ));
        }

        match (object.get("value"), object.get("unsupported")) {
            (Some(Value::String(literal)), None) => {
                let evidence = ReadQueryParameterEvidence::known(key, literal).map_err(|_| {
                    format!(
                        "seq {seq}: queryEvidence entry {index} violates the approved safe-literal boundary"
                    )
                })?;
                stats.read_query_literals_retained =
                    stats.read_query_literals_retained.saturating_add(1);
                sanitized.push(json!({
                    "key": evidence.key(),
                    "value": evidence.literal_evidence().expect("known literal").literal(),
                }));
            }
            (None, Some(Value::Bool(true))) => {
                let evidence =
                    ReadQueryParameterEvidence::unsupported_redacted(key).map_err(|_| {
                        format!(
                            "seq {seq}: queryEvidence entry {index} violates the approved-key boundary"
                        )
                    })?;
                stats.read_query_values_redacted =
                    stats.read_query_values_redacted.saturating_add(1);
                sanitized.push(json!({
                    "key": evidence.key(),
                    "unsupported": true,
                }));
            }
            _ => {
                return Err(format!(
                    "seq {seq}: queryEvidence entry {index} must contain exactly one safe value or unsupported=true marker"
                ));
            }
        }
    }

    Ok(Some(Value::Array(sanitized)))
}

pub(super) fn is_c02_conversation_resource_path(path: &str) -> bool {
    path.strip_prefix("/backend-api/conversations/")
        .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
}

pub(super) fn json_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    mime == "application/json" || mime == "text/json" || mime.ends_with("+json")
}

pub(super) fn normalize_read_path(
    raw: &str,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> String {
    raw.split('/')
        .map(|segment| {
            if looks_like_identity_segment(segment) {
                stats.read_identity_values_placeholdered =
                    stats.read_identity_values_placeholdered.saturating_add(1);
                ids.map_generic(segment)
            } else {
                segment.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn looks_like_identity_segment(segment: &str) -> bool {
    if segment.len() < 16 {
        return false;
    }
    let compact = segment.replace('-', "");
    compact.len() >= 16
        && compact
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(super) fn reduce_read_json(
    value: &Value,
    key: Option<&str>,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut output = Map::new();
            for child_key in keys {
                output.insert(
                    child_key.clone(),
                    reduce_read_json(&map[child_key], Some(child_key), allowed_texts, ids, stats),
                );
            }
            Value::Object(output)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| reduce_read_json(item, key, allowed_texts, ids, stats))
                .collect(),
        ),
        Value::String(text) => {
            if key.is_some_and(sensitive_field) {
                stats.sensitive_values_redacted = stats.sensitive_values_redacted.saturating_add(1);
                return Value::String(REDACTED.to_owned());
            }
            if let Some(key) = key {
                if conversation_identity_field(key) {
                    stats.read_identity_values_placeholdered =
                        stats.read_identity_values_placeholdered.saturating_add(1);
                    return Value::String(ids.map_conversation(text));
                }
                if key.eq_ignore_ascii_case("id") {
                    stats.read_identity_values_placeholdered =
                        stats.read_identity_values_placeholdered.saturating_add(1);
                    return Value::String(ids.map_generic(text));
                }
                if message_identity_field(key) {
                    stats.read_identity_values_placeholdered =
                        stats.read_identity_values_placeholdered.saturating_add(1);
                    return Value::String(ids.map_message(text));
                }
                if generic_identity_field(key) {
                    stats.read_identity_values_placeholdered =
                        stats.read_identity_values_placeholdered.saturating_add(1);
                    return Value::String(ids.map_generic(text));
                }
            }
            if allowed_texts.contains(text) {
                stats.canonical_experiment_literals_retained = stats
                    .canonical_experiment_literals_retained
                    .saturating_add(1);
                return Value::String(text.clone());
            }
            stats.read_string_values_generalized =
                stats.read_string_values_generalized.saturating_add(1);
            Value::String(STRING.to_owned())
        }
        Value::Number(_) => Value::String(NUMBER.to_owned()),
        Value::Bool(_) => Value::String(BOOL.to_owned()),
        Value::Null => Value::Null,
    }
}

pub(super) fn collect_read_shape(value: &Value, pointer: &str, rows: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            rows.push(json!({
                "pointer": pointer,
                "type": "object",
                "member_count": map.len(),
            }));
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let child = format!("{pointer}/{}", escape_pointer_segment(key));
                collect_read_shape(&map[key], &child, rows);
            }
        }
        Value::Array(items) => {
            rows.push(json!({
                "pointer": pointer,
                "type": "array",
                "length": items.len(),
            }));
            for (index, item) in items.iter().enumerate() {
                collect_read_shape(item, &format!("{pointer}/{index}"), rows);
            }
        }
        _ => rows.push(json!({
            "pointer": pointer,
            "type": json_type_name(value),
        })),
    }
}

pub(super) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn escape_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
