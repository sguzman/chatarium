//! stream logic mechanically isolated from the fail-closed flight sanitization pipeline.
use super::*;

pub(super) fn ensure_stream(
    raw_stream: &str,
    stream_order: &mut Vec<String>,
    stream_by_raw_id: &mut HashMap<String, usize>,
    streams: &mut Vec<StreamBuilder>,
) -> usize {
    if let Some(index) = stream_by_raw_id.get(raw_stream) {
        return *index;
    }
    let index = streams.len();
    let public_id = format!("stream-{}", index + 1);
    stream_order.push(raw_stream.to_owned());
    stream_by_raw_id.insert(raw_stream.to_owned(), index);
    streams.push(StreamBuilder {
        public_id,
        ..StreamBuilder::default()
    });
    index
}

pub(super) fn sanitize_send_intents(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> Vec<Value> {
    export
        .get("sendIntents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|intent| {
            intent
                .get("at")
                .and_then(Value::as_str)
                .is_some_and(|at| at >= selected.started_at.as_str())
        })
        .map(|intent| {
            json!({
                "id": intent
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|raw| {
                        stats.send_identities_placeholdered =
                            stats.send_identities_placeholdered.saturating_add(1);
                        ids.map_send(raw)
                    }),
                "at": intent.get("at"),
                "reason": intent.get("reason"),
                "text": safe_text(
                    intent.get("text").and_then(Value::as_str),
                    allowed_texts,
                    stats,
                ),
                "state": intent.get("state"),
                "confirmed_at": intent.get("confirmedAt"),
                "observed_message_id": intent
                    .get("observedMessageId")
                    .and_then(Value::as_str)
                    .map(|raw| {
                        stats.message_identities_placeholdered =
                            stats.message_identities_placeholdered.saturating_add(1);
                        ids.map_message(raw)
                    }),
                "confirmed_conversation": intent
                    .get("confirmedConversation")
                    .and_then(Value::as_str)
                    .map(|raw| sanitize_conversation_scope(raw, ids, stats)),
                "confirmation_evidence": intent.get("confirmationEvidence"),
            })
        })
        .collect()
}

pub(super) fn sanitize_assistant_wal(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> Value {
    let Some(wal) = export.get("assistantWal").filter(|value| !value.is_null()) else {
        return Value::Null;
    };
    if !wal
        .get("at")
        .and_then(Value::as_str)
        .is_some_and(|at| at >= selected.started_at.as_str())
    {
        return Value::Null;
    }

    json!({
        "at": wal.get("at"),
        "conversation": wal
            .get("conversation")
            .and_then(Value::as_str)
            .map(|raw| sanitize_conversation_scope(raw, ids, stats)),
        "observed_id": wal
            .get("observedId")
            .and_then(Value::as_str)
            .map(|raw| {
                stats.message_identities_placeholdered =
                    stats.message_identities_placeholdered.saturating_add(1);
                ids.map_message(raw)
            }),
        "text": safe_text(
            wal.get("text").and_then(Value::as_str),
            allowed_texts,
            stats,
        ),
        "source": wal.get("source"),
        "evidence_sources": wal.get("evidenceSources"),
        "protocol_evidence": wal.get("protocolEvidence"),
        "protocol_status": wal.get("protocolStatus"),
        "protocol_end_turn": wal.get("protocolEndTurn"),
        "protocol_is_complete": wal.get("protocolIsComplete"),
    })
}

pub(super) fn sanitize_messages(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> Vec<Value> {
    export
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| {
            message
                .get("observedAt")
                .and_then(Value::as_str)
                .is_some_and(|at| at >= selected.started_at.as_str())
        })
        .map(|message| {
            json!({
                "observed_at": message.get("observedAt"),
                "observed_id": message
                    .get("observedId")
                    .and_then(Value::as_str)
                    .map(|raw| {
                        stats.message_identities_placeholdered =
                            stats.message_identities_placeholdered.saturating_add(1);
                        ids.map_message(raw)
                    }),
                "conversation": message
                    .get("conversation")
                    .and_then(Value::as_str)
                    .map(|raw| sanitize_conversation_scope(raw, ids, stats)),
                "role": message.get("role"),
                "text": safe_text(
                    message.get("text").and_then(Value::as_str),
                    allowed_texts,
                    stats,
                ),
                "source": message.get("source"),
                "protocol_status": message.get("protocolStatus"),
                "protocol_end_turn": message.get("protocolEndTurn"),
                "protocol_evidence": message.get("protocolEvidence"),
                "protocol_is_complete": message.get("protocolIsComplete"),
            })
        })
        .collect()
}

pub(super) fn assistant_inventory(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
) -> Value {
    let Some(wal) = export.get("assistantWal").filter(|value| !value.is_null()) else {
        return json!({"present": false});
    };
    if !wal
        .get("at")
        .and_then(Value::as_str)
        .is_some_and(|at| at >= selected.started_at.as_str())
    {
        return json!({"present": false});
    }

    let text = wal.get("text").and_then(Value::as_str);
    json!({
        "present": true,
        "source": wal.get("source"),
        "evidence_sources": wal.get("evidenceSources"),
        "protocol_evidence": wal.get("protocolEvidence"),
        "protocol_status": wal.get("protocolStatus"),
        "protocol_end_turn": wal.get("protocolEndTurn"),
        "protocol_is_complete": wal.get("protocolIsComplete"),
        "text_is_experiment_allowed": text.is_some_and(|value| allowed_texts.contains(value)),
    })
}

pub(super) fn safe_text(
    text: Option<&str>,
    allowed_texts: &BTreeSet<String>,
    stats: &mut FlightSanitizationStats,
) -> Value {
    match text {
        Some(text) if allowed_texts.contains(text) => {
            stats.canonical_experiment_literals_retained = stats
                .canonical_experiment_literals_retained
                .saturating_add(1);
            Value::String(text.to_owned())
        }
        Some(_) => {
            stats.content_values_redacted = stats.content_values_redacted.saturating_add(1);
            Value::String(REDACTED_CONTENT.to_owned())
        }
        None => Value::Null,
    }
}

pub(super) fn sanitize_conversation_scope(
    raw: &str,
    ids: &mut IdentityMap,
    stats: &mut FlightSanitizationStats,
) -> String {
    if let Some(value) = raw.strip_prefix("conversation:") {
        stats.conversation_identities_placeholdered = stats
            .conversation_identities_placeholdered
            .saturating_add(1);
        return format!("conversation:{}", ids.map_conversation(value));
    }
    if raw.starts_with("route:") {
        return raw.to_owned();
    }
    stats.conversation_identities_placeholdered = stats
        .conversation_identities_placeholdered
        .saturating_add(1);
    ids.map_conversation(raw)
}

pub(super) fn sanitize_sse_frame(
    frame: &SseFrame,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    inventory: &mut StreamInventory,
    warnings: &mut Vec<String>,
    stats: &mut FlightSanitizationStats,
) -> Value {
    inventory.frame_count += 1;
    let public_event = frame.event.as_deref().map(|event| {
        let sanitized = safe_sse_event_name(event);
        if sanitized != event {
            stats.unknown_event_names_redacted =
                stats.unknown_event_names_redacted.saturating_add(1);
        }
        sanitized
    });
    if let Some(event) = public_event.as_deref() {
        *inventory
            .named_event_counts
            .entry(event.to_owned())
            .or_default() += 1;
    }

    if frame.data == "[DONE]" {
        inventory.done = true;
        return json!({
            "event": public_event,
            "data": "[DONE]",
        });
    }

    let Ok(mut payload) = serde_json::from_str::<Value>(&frame.data) else {
        inventory.parse_warning_count += 1;
        stats.malformed_sse_frames_fail_closed =
            stats.malformed_sse_frames_fail_closed.saturating_add(1);
        warnings.push(format!(
            "SSE frame {} could not be parsed and was fail-closed",
            inventory.frame_count
        ));
        return json!({
            "event": public_event,
            "data": "<redacted-unparsed-sse>",
        });
    };

    observe_frame_shape(frame, &payload, inventory);
    if frame.event.as_deref() == Some("delta_encoding") {
        if payload.as_str() != Some("v1") {
            stats.unknown_encodings_redacted = stats.unknown_encodings_redacted.saturating_add(1);
            payload = Value::String(REDACTED_VALUE.to_owned());
        }
    } else {
        sanitize_protocol_json(&mut payload, None, allowed_texts, ids, stats);
    }

    json!({
        "event": public_event,
        "data": payload,
    })
}

pub(super) fn observe_frame_shape(frame: &SseFrame, payload: &Value, inventory: &mut StreamInventory) {
    if frame.event.as_deref() == Some("delta_encoding") {
        if let Some(value) = payload.as_str() {
            inventory.delta_encodings.insert(if value == "v1" {
                value.to_owned()
            } else {
                REDACTED_VALUE.to_owned()
            });
        }
        return;
    }

    if frame.event.as_deref() == Some("delta") {
        observe_delta_shape(payload, inventory);
        return;
    }

    if let Some(kind) = payload.get("type").and_then(Value::as_str) {
        let public_kind = safe_structural_value("type", kind);
        *inventory
            .control_type_counts
            .entry(public_kind)
            .or_default() += 1;
        if kind == "message_stream_complete" {
            inventory.message_stream_complete = true;
        }
        if kind == "message_marker" {
            let marker = payload.get("marker").and_then(Value::as_str).map_or_else(
                || "<missing>".to_owned(),
                |value| safe_structural_value("marker", value),
            );
            let event = payload.get("event").and_then(Value::as_str).map_or_else(
                || "<missing>".to_owned(),
                |value| safe_structural_value("event", value),
            );
            *inventory
                .marker_counts
                .entry(format!("{marker}:{event}"))
                .or_default() += 1;
        }
    }
}

pub(super) fn observe_delta_shape(payload: &Value, inventory: &mut StreamInventory) {
    if let Some(operation) = payload.get("o").and_then(Value::as_str) {
        *inventory
            .delta_operation_counts
            .entry(safe_structural_value("o", operation))
            .or_default() += 1;
    }
    if let Some(path) = payload.get("p").and_then(Value::as_str) {
        *inventory
            .delta_path_counts
            .entry(safe_protocol_path(path))
            .or_default() += 1;
    }
    if payload.get("o").and_then(Value::as_str) == Some("patch") {
        if let Some(operations) = payload.get("v").and_then(Value::as_array) {
            for operation in operations {
                if let Some(name) = operation.get("o").and_then(Value::as_str) {
                    *inventory
                        .delta_operation_counts
                        .entry(safe_structural_value("o", name))
                        .or_default() += 1;
                }
                if let Some(path) = operation.get("p").and_then(Value::as_str) {
                    *inventory
                        .delta_path_counts
                        .entry(safe_protocol_path(path))
                        .or_default() += 1;
                }
            }
        }
    }
}
