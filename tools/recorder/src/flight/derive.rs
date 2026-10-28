//! derive logic mechanically isolated from the fail-closed flight sanitization pipeline.
use super::*;

pub(super) fn derive_sanitized_run(
    export: &Value,
    selected: &SelectedRun,
    experiment: &ExperimentDefinition,
    allowed_texts: &BTreeSet<String>,
) -> Result<Value, String> {
    let mut ids = IdentityMap::default();
    let mut stats = FlightSanitizationStats::default();
    let mut warnings = Vec::<String>::new();
    let mut event_kind_counts = BTreeMap::<String, u64>::new();
    let mut event_timeline = Vec::new();
    let mut stream_order = Vec::<String>::new();
    let mut stream_by_raw_id = HashMap::<String, usize>::new();
    let mut streams = Vec::<StreamBuilder>::new();
    let mut read_responses = Vec::<Value>::new();
    let mut read_inventory = Vec::<Value>::new();

    for event in &selected.events {
        let seq = event.get("seq").and_then(Value::as_u64).unwrap_or_default();
        let kind = event
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned();
        *event_kind_counts.entry(kind.clone()).or_default() += 1;
        event_timeline.push(json!({"seq": seq, "kind": kind}));

        match kind.as_str() {
            "network-stream-start" => {
                let raw_stream = event
                    .pointer("/payload/streamId")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing-stream-id>")
                    .to_owned();
                let index = ensure_stream(
                    &raw_stream,
                    &mut stream_order,
                    &mut stream_by_raw_id,
                    &mut streams,
                );
                let stream = &mut streams[index];
                stream.endpoint = event
                    .pointer("/payload/endpoint")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                stream.method = event
                    .pointer("/payload/method")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                stream.status = event.pointer("/payload/status").and_then(Value::as_i64);
                stream.content_type = event
                    .pointer("/payload/contentType")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
            "network-stream-chunk" => {
                let Some(raw_stream) = event.pointer("/payload/streamId").and_then(Value::as_str)
                else {
                    warnings.push(format!("seq {seq}: network-stream-chunk missing streamId"));
                    continue;
                };
                let index = ensure_stream(
                    raw_stream,
                    &mut stream_order,
                    &mut stream_by_raw_id,
                    &mut streams,
                );
                let Some(text) = event.pointer("/payload/text").and_then(Value::as_str) else {
                    warnings.push(format!("seq {seq}: network-stream-chunk missing text"));
                    continue;
                };
                for frame in streams[index].decoder.push(text) {
                    let sanitized = sanitize_sse_frame(
                        &frame,
                        allowed_texts,
                        &mut ids,
                        &mut streams[index].inventory,
                        &mut warnings,
                        &mut stats,
                    );
                    streams[index].frames.push(sanitized);
                }
            }
            "network-stream-end" => {
                let Some(raw_stream) = event.pointer("/payload/streamId").and_then(Value::as_str)
                else {
                    warnings.push(format!("seq {seq}: network-stream-end missing streamId"));
                    continue;
                };
                let index = ensure_stream(
                    raw_stream,
                    &mut stream_order,
                    &mut stream_by_raw_id,
                    &mut streams,
                );
                streams[index].terminal = event
                    .pointer("/payload/terminal")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                streams[index].captured_bytes = event
                    .pointer("/payload/capturedBytes")
                    .and_then(Value::as_u64);
            }
            "network-stream-error" => warnings.push(format!(
                "seq {seq}: source recorder reported network-stream-error"
            )),
            "protocol-read-response-captured" => {
                let public_id = format!("read-{}", read_responses.len() + 1);
                let (sanitized_read, inventory_read) = sanitize_read_response_event(
                    event,
                    &public_id,
                    allowed_texts,
                    &mut ids,
                    &mut stats,
                    &mut warnings,
                )?;
                read_responses.push(sanitized_read);
                read_inventory.push(inventory_read);
            }
            "protocol-read-response-error" | "protocol-read-fetch-error" => {
                warnings.push(format!("seq {seq}: source recorder reported {kind}"));
            }
            _ => {}
        }
    }

    for stream in &streams {
        if !stream.decoder.pending().trim().is_empty() {
            stats.unterminated_sse_tails_redacted =
                stats.unterminated_sse_tails_redacted.saturating_add(1);
            warnings.push(format!(
                "{}: unterminated SSE tail was redacted",
                stream.public_id
            ));
        }
    }

    let (send_intents, assistant_wal, messages) = if is_read_experiment(experiment) {
        (Vec::new(), Value::Null, Vec::new())
    } else {
        (
            sanitize_send_intents(export, selected, allowed_texts, &mut ids, &mut stats),
            sanitize_assistant_wal(export, selected, allowed_texts, &mut ids, &mut stats),
            sanitize_messages(export, selected, allowed_texts, &mut ids, &mut stats),
        )
    };

    let sanitized_streams = streams
        .iter()
        .map(|stream| {
            json!({
                "stream": stream.public_id,
                "endpoint": stream.endpoint,
                "method": stream.method,
                "status": stream.status,
                "content_type": stream.content_type,
                "terminal": stream.terminal,
                "captured_bytes": stream.captured_bytes,
                "frames": stream.frames,
            })
        })
        .collect::<Vec<_>>();

    let inventory_streams = streams
        .iter()
        .map(|stream| {
            json!({
                "stream": stream.public_id,
                "endpoint": stream.endpoint,
                "method": stream.method,
                "status": stream.status,
                "content_type": stream.content_type,
                "frame_count": stream.inventory.frame_count,
                "named_event_counts": stream.inventory.named_event_counts,
                "control_type_counts": stream.inventory.control_type_counts,
                "delta_operation_counts": stream.inventory.delta_operation_counts,
                "delta_path_counts": stream.inventory.delta_path_counts,
                "marker_counts": stream.inventory.marker_counts,
                "delta_encodings": stream.inventory.delta_encodings,
                "completion": {
                    "message_stream_complete": stream.inventory.message_stream_complete,
                    "done": stream.inventory.done,
                    "terminal": stream.terminal,
                },
                "parse_warning_count": stream.inventory.parse_warning_count,
            })
        })
        .collect::<Vec<_>>();

    let send_state_counts = count_field(&send_intents, "state");
    let confirmation_evidence_counts = count_field(&send_intents, "confirmation_evidence");
    let message_role_counts = count_field(&messages, "role");
    let message_source_counts = count_field(&messages, "source");

    let sanitized = json!({
        "format": "chatarium-flight-sanitized-run",
        "version": 1,
        "experiment_id": experiment.id,
        "recorder_version": selected.recorder_version,
        "selected_run": {
            "started_seq": selected.started_seq,
            "started_at": selected.started_at,
        },
        "event_timeline": event_timeline,
        "send_intents": send_intents,
        "assistant_wal": assistant_wal,
        "messages": messages,
        "streams": sanitized_streams,
        "read_capture": sanitize_read_capture_metadata(selected),
        "read_responses": read_responses,
        "warnings": warnings,
    });

    let warning_count = sanitized
        .get("warnings")
        .and_then(Value::as_array)
        .map_or(0, |values| values.len() as u64);

    let inventory = json!({
        "format": "chatarium-flight-inventory",
        "version": 1,
        "experiment_id": experiment.id,
        "recorder_version": selected.recorder_version,
        "selected_run": {
            "started_seq": selected.started_seq,
            "started_at": selected.started_at,
            "event_count": selected.events.len(),
        },
        "event_kind_counts": event_kind_counts,
        "send_state_counts": send_state_counts,
        "confirmation_evidence_counts": confirmation_evidence_counts,
        "message_role_counts": message_role_counts,
        "message_source_counts": message_source_counts,
        "assistant_wal": assistant_inventory(export, selected, allowed_texts),
        "streams": inventory_streams,
        "read_capture": sanitize_read_capture_metadata(selected),
        "read_responses": read_inventory,
        "warning_count": warning_count,
    });

    Ok(json!({
        "sanitized": sanitized,
        "inventory": inventory,
        "sanitization_report": stats.report(),
    }))
}
