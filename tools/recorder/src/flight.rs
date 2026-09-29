//! Flight Recorder export ingestion and fail-closed public evidence derivation.

use chatarium_protocol::sse::{SseDecoder, SseFrame};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use crate::sha256_hex;

const REDACTED: &str = "<redacted>";
const REDACTED_CONTENT: &str = "<redacted-content>";
const REDACTED_VALUE: &str = "<redacted-value>";
const NUMBER: &str = "<number>";

#[derive(Debug, Deserialize)]
struct ExperimentDefinition {
    schema: String,
    version: u64,
    id: String,
    action: ExperimentAction,
    success: ExperimentSuccess,
}

#[derive(Debug, Deserialize)]
struct ExperimentAction {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExperimentSuccess {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Clone)]
struct SelectedRun {
    started_seq: u64,
    started_at: String,
    recorder_version: String,
    events: Vec<Value>,
}

#[derive(Debug, Default)]
struct StreamBuilder {
    public_id: String,
    endpoint: Option<String>,
    method: Option<String>,
    status: Option<i64>,
    content_type: Option<String>,
    terminal: Option<String>,
    captured_bytes: Option<u64>,
    decoder: SseDecoder,
    frames: Vec<Value>,
    inventory: StreamInventory,
}

#[derive(Debug, Default)]
struct StreamInventory {
    frame_count: u64,
    named_event_counts: BTreeMap<String, u64>,
    control_type_counts: BTreeMap<String, u64>,
    delta_operation_counts: BTreeMap<String, u64>,
    delta_path_counts: BTreeMap<String, u64>,
    marker_counts: BTreeMap<String, u64>,
    delta_encodings: BTreeSet<String>,
    message_stream_complete: bool,
    done: bool,
    parse_warning_count: u64,
}

#[derive(Debug, Default)]
struct IdentityMap {
    conversation: HashMap<String, String>,
    message: HashMap<String, String>,
    send: HashMap<String, String>,
}

impl IdentityMap {
    fn map_conversation(&mut self, raw: &str) -> String {
        stable_placeholder(&mut self.conversation, raw, "conversation")
    }

    fn map_message(&mut self, raw: &str) -> String {
        stable_placeholder(&mut self.message, raw, "message")
    }

    fn map_send(&mut self, raw: &str) -> String {
        stable_placeholder(&mut self.send, raw, "send")
    }
}

fn stable_placeholder(map: &mut HashMap<String, String>, raw: &str, kind: &str) -> String {
    if let Some(value) = map.get(raw) {
        return value.clone();
    }
    let value = format!("<{kind}:{}>", map.len() + 1);
    map.insert(raw.to_owned(), value.clone());
    value
}

/// Ingest a Flight Recorder export and write deterministic sanitized evidence plus inventory.
pub fn snapshot_flight(
    input: &Path,
    experiment_path: &Path,
    snapshot_dir: &Path,
    capture_id: &str,
) -> Result<(), String> {
    crate::validate_capture_id(capture_id)?;

    let source = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let experiment_bytes = fs::read(experiment_path)
        .map_err(|error| format!("read {}: {error}", experiment_path.display()))?;
    let experiment_text = std::str::from_utf8(&experiment_bytes)
        .map_err(|error| format!("experiment TOML is not UTF-8: {error}"))?;
    let experiment: ExperimentDefinition = toml::from_str(experiment_text)
        .map_err(|error| format!("parse experiment TOML: {error}"))?;
    validate_experiment(&experiment)?;

    let export: Value = serde_json::from_slice(&source)
        .map_err(|error| format!("parse Flight Recorder JSON: {error}"))?;
    validate_export(&export)?;
    let selected = select_latest_run(&export)?;
    validate_run_matches_experiment(&export, &selected, &experiment)?;

    let mut allowed_texts = BTreeSet::new();
    if let Some(text) = experiment.action.text.as_deref() {
        allowed_texts.insert(text.to_owned());
    }
    if let Some(text) = experiment.success.text.as_deref() {
        allowed_texts.insert(text.to_owned());
    }

    let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed_texts)?;
    let sanitized = serde_json::to_vec_pretty(
        derived
            .get("sanitized")
            .ok_or_else(|| "internal flight derivation omitted sanitized output".to_owned())?,
    )
    .map_err(|error| format!("serialize sanitized Flight Recorder evidence: {error}"))?;
    let inventory = serde_json::to_vec_pretty(
        derived
            .get("inventory")
            .ok_or_else(|| "internal flight derivation omitted inventory output".to_owned())?,
    )
    .map_err(|error| format!("serialize Flight Recorder inventory: {error}"))?;

    let evidence_dir = snapshot_dir.join("evidence");
    let derived_dir = snapshot_dir.join("derived");
    fs::create_dir_all(&evidence_dir)
        .map_err(|error| format!("create {}: {error}", evidence_dir.display()))?;
    fs::create_dir_all(&derived_dir)
        .map_err(|error| format!("create {}: {error}", derived_dir.display()))?;

    let evidence_relative = format!("evidence/{capture_id}.flight.json");
    let evidence_path = evidence_dir.join(format!("{capture_id}.flight.json"));
    fs::write(&evidence_path, &sanitized)
        .map_err(|error| format!("write {}: {error}", evidence_path.display()))?;

    let inventory_relative = format!("derived/{capture_id}.flight.inventory.json");
    let inventory_path = derived_dir.join(format!("{capture_id}.flight.inventory.json"));
    fs::write(&inventory_path, &inventory)
        .map_err(|error| format!("write {}: {error}", inventory_path.display()))?;

    let warning_count = derived
        .pointer("/inventory/warning_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();

    let metadata = json!({
        "format": "chatarium-flight-capture",
        "version": 1,
        "capture_id": capture_id,
        "experiment_id": experiment.id,
        "experiment_schema": experiment.schema,
        "experiment_version": experiment.version,
        "recorder_version": selected.recorder_version,
        "selected_run": {
            "started_seq": selected.started_seq,
            "started_at": selected.started_at,
            "event_count": selected.events.len(),
        },
        "raw_source_sha256": sha256_hex(&source),
        "raw_source_bytes": source.len(),
        "raw_retained_outside_git": true,
        "sanitized_file": evidence_relative,
        "sanitized_sha256": sha256_hex(&sanitized),
        "sanitized_bytes": sanitized.len(),
        "inventory_file": inventory_relative,
        "inventory_sha256": sha256_hex(&inventory),
        "inventory_bytes": inventory.len(),
        "warning_count": warning_count,
        "recorder_tool_version": env!("CARGO_PKG_VERSION"),
    });
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("serialize Flight Recorder metadata: {error}"))?;
    let metadata_path = derived_dir.join(format!("{capture_id}.flight.meta.json"));
    fs::write(&metadata_path, &metadata_bytes)
        .map_err(|error| format!("write {}: {error}", metadata_path.display()))?;

    println!("capture: {}", evidence_path.display());
    println!("inventory: {}", inventory_path.display());
    println!("metadata: {}", metadata_path.display());
    println!("raw-sha256: {}", sha256_hex(&source));
    println!("sanitized-sha256: {}", sha256_hex(&sanitized));
    Ok(())
}

fn validate_experiment(experiment: &ExperimentDefinition) -> Result<(), String> {
    if experiment.schema != "chatarium-experiment" || experiment.version != 1 {
        return Err(format!(
            "unsupported experiment definition {} v{}",
            experiment.schema, experiment.version
        ));
    }
    if experiment.id.trim().is_empty() {
        return Err("experiment id is empty".to_owned());
    }
    if experiment.action.kind == "send_text"
        && experiment.action.text.as_deref().unwrap_or("").is_empty()
    {
        return Err("send_text experiment is missing action.text".to_owned());
    }
    if experiment.success.kind == "assistant_text_contains"
        && experiment.success.text.as_deref().unwrap_or("").is_empty()
    {
        return Err("assistant_text_contains experiment is missing success.text".to_owned());
    }
    Ok(())
}

fn validate_export(export: &Value) -> Result<(), String> {
    let format = export.get("format").and_then(Value::as_str);
    let version = export.get("version").and_then(Value::as_u64);
    if format != Some("chatarium-flight-recorder-export") {
        return Err("input is not a chatarium-flight-recorder-export".to_owned());
    }
    if version != Some(3) {
        return Err(format!(
            "unsupported Flight Recorder export version {}",
            version.map_or_else(|| "?".to_owned(), |value| value.to_string())
        ));
    }
    if export.get("events").and_then(Value::as_array).is_none() {
        return Err("Flight Recorder export is missing events".to_owned());
    }
    Ok(())
}

fn validate_run_matches_experiment(
    export: &Value,
    selected: &SelectedRun,
    experiment: &ExperimentDefinition,
) -> Result<(), String> {
    if experiment.action.kind != "send_text" {
        return Ok(());
    }

    let expected = experiment
        .action
        .text
        .as_deref()
        .ok_or_else(|| "send_text experiment is missing action.text".to_owned())?;
    let matched = export
        .get("sendIntents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|intent| {
            intent
                .get("at")
                .and_then(Value::as_str)
                .is_some_and(|at| at >= selected.started_at.as_str())
                && intent.get("text").and_then(Value::as_str) == Some(expected)
        });

    if !matched {
        return Err(format!(
            "latest recorder run does not contain the exact action text for experiment {}",
            experiment.id
        ));
    }
    Ok(())
}

fn select_latest_run(export: &Value) -> Result<SelectedRun, String> {
    let events = export
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "Flight Recorder export is missing events".to_owned())?;

    let start = events
        .iter()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("recorder-started"))
        .max_by_key(|event| event.get("seq").and_then(Value::as_u64).unwrap_or_default())
        .ok_or_else(|| "Flight Recorder export contains no recorder-started event".to_owned())?;

    let started_seq = start
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "latest recorder-started event has no numeric seq".to_owned())?;
    let started_at = start
        .get("at")
        .and_then(Value::as_str)
        .ok_or_else(|| "latest recorder-started event has no timestamp".to_owned())?
        .to_owned();
    let recorder_version = start
        .pointer("/payload/version")
        .and_then(Value::as_str)
        .or_else(|| export.get("recorderVersion").and_then(Value::as_str))
        .unwrap_or("?")
        .to_owned();

    let selected_events = events
        .iter()
        .filter(|event| {
            event
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|seq| seq >= started_seq)
        })
        .cloned()
        .collect::<Vec<_>>();

    Ok(SelectedRun {
        started_seq,
        started_at,
        recorder_version,
        events: selected_events,
    })
}

fn derive_sanitized_run(
    export: &Value,
    selected: &SelectedRun,
    experiment: &ExperimentDefinition,
    allowed_texts: &BTreeSet<String>,
) -> Result<Value, String> {
    let mut ids = IdentityMap::default();
    let mut warnings = Vec::<String>::new();
    let mut event_kind_counts = BTreeMap::<String, u64>::new();
    let mut event_timeline = Vec::new();
    let mut stream_order = Vec::<String>::new();
    let mut stream_by_raw_id = HashMap::<String, usize>::new();
    let mut streams = Vec::<StreamBuilder>::new();

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
            _ => {}
        }
    }

    for stream in &streams {
        if !stream.decoder.pending().trim().is_empty() {
            warnings.push(format!(
                "{}: unterminated SSE tail was redacted",
                stream.public_id
            ));
        }
    }

    let send_intents = sanitize_send_intents(export, selected, allowed_texts, &mut ids);
    let assistant_wal = sanitize_assistant_wal(export, selected, allowed_texts, &mut ids);
    let messages = sanitize_messages(export, selected, allowed_texts, &mut ids);

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
        "warning_count": warning_count,
    });

    Ok(json!({
        "sanitized": sanitized,
        "inventory": inventory,
    }))
}

fn ensure_stream(
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

fn sanitize_send_intents(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
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
                    .map(|raw| ids.map_send(raw)),
                "at": intent.get("at"),
                "reason": intent.get("reason"),
                "text": safe_text(intent.get("text").and_then(Value::as_str), allowed_texts),
                "state": intent.get("state"),
                "confirmed_at": intent.get("confirmedAt"),
                "observed_message_id": intent
                    .get("observedMessageId")
                    .and_then(Value::as_str)
                    .map(|raw| ids.map_message(raw)),
                "confirmed_conversation": intent
                    .get("confirmedConversation")
                    .and_then(Value::as_str)
                    .map(|raw| sanitize_conversation_scope(raw, ids)),
                "confirmation_evidence": intent.get("confirmationEvidence"),
            })
        })
        .collect()
}

fn sanitize_assistant_wal(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
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
            .map(|raw| sanitize_conversation_scope(raw, ids)),
        "observed_id": wal
            .get("observedId")
            .and_then(Value::as_str)
            .map(|raw| ids.map_message(raw)),
        "text": safe_text(wal.get("text").and_then(Value::as_str), allowed_texts),
        "source": wal.get("source"),
        "evidence_sources": wal.get("evidenceSources"),
        "protocol_evidence": wal.get("protocolEvidence"),
        "protocol_status": wal.get("protocolStatus"),
        "protocol_end_turn": wal.get("protocolEndTurn"),
        "protocol_is_complete": wal.get("protocolIsComplete"),
    })
}

fn sanitize_messages(
    export: &Value,
    selected: &SelectedRun,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
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
                    .map(|raw| ids.map_message(raw)),
                "conversation": message
                    .get("conversation")
                    .and_then(Value::as_str)
                    .map(|raw| sanitize_conversation_scope(raw, ids)),
                "role": message.get("role"),
                "text": safe_text(message.get("text").and_then(Value::as_str), allowed_texts),
                "source": message.get("source"),
                "protocol_status": message.get("protocolStatus"),
                "protocol_end_turn": message.get("protocolEndTurn"),
                "protocol_evidence": message.get("protocolEvidence"),
                "protocol_is_complete": message.get("protocolIsComplete"),
            })
        })
        .collect()
}

fn assistant_inventory(
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

fn safe_text(text: Option<&str>, allowed_texts: &BTreeSet<String>) -> Value {
    match text {
        Some(text) if allowed_texts.contains(text) => Value::String(text.to_owned()),
        Some(_) => Value::String(REDACTED_CONTENT.to_owned()),
        None => Value::Null,
    }
}

fn sanitize_conversation_scope(raw: &str, ids: &mut IdentityMap) -> String {
    if let Some(value) = raw.strip_prefix("conversation:") {
        return format!("conversation:{}", ids.map_conversation(value));
    }
    if raw.starts_with("route:") {
        return raw.to_owned();
    }
    ids.map_conversation(raw)
}

fn sanitize_sse_frame(
    frame: &SseFrame,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
    inventory: &mut StreamInventory,
    warnings: &mut Vec<String>,
) -> Value {
    inventory.frame_count += 1;
    let public_event = frame.event.as_deref().map(safe_sse_event_name);
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
            payload = Value::String(REDACTED_VALUE.to_owned());
        }
    } else {
        sanitize_protocol_json(&mut payload, None, allowed_texts, ids);
    }

    json!({
        "event": public_event,
        "data": payload,
    })
}

fn observe_frame_shape(frame: &SseFrame, payload: &Value, inventory: &mut StreamInventory) {
    if frame.event.as_deref() == Some("delta_encoding") {
        if let Some(value) = payload.as_str() {
            inventory.delta_encodings.insert(value.to_owned());
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

fn observe_delta_shape(payload: &Value, inventory: &mut StreamInventory) {
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

fn sanitize_protocol_json(
    value: &mut Value,
    key: Option<&str>,
    allowed_texts: &BTreeSet<String>,
    ids: &mut IdentityMap,
) {
    match value {
        Value::Null | Value::Bool(_) => {}
        Value::Number(_) => {
            *value = Value::String(NUMBER.to_owned());
        }
        Value::String(text) => {
            let field = key.unwrap_or_default();
            if sensitive_field(field) {
                *text = REDACTED.to_owned();
            } else if conversation_identity_field(field) {
                *text = ids.map_conversation(text);
            } else if message_identity_field(field) {
                *text = ids.map_message(text);
            } else if generic_identity_field(field) {
                *text = "<id>".to_owned();
            } else if content_field(field) {
                if !allowed_texts.contains(text) {
                    *text = REDACTED_CONTENT.to_owned();
                }
            } else if field == "p" {
                *text = safe_protocol_path(text);
            } else if structural_string_field(field) {
                *text = safe_structural_value(field, text);
            } else if !allowed_texts.contains(text) {
                *text = REDACTED_VALUE.to_owned();
            }
        }
        Value::Array(items) => {
            let content_array = matches!(key, Some("parts"));
            for item in items {
                if content_array {
                    match item {
                        Value::String(text) if allowed_texts.contains(text) => {}
                        Value::String(text) => *text = REDACTED_CONTENT.to_owned(),
                        nested => sanitize_protocol_json(nested, key, allowed_texts, ids),
                    }
                } else {
                    sanitize_protocol_json(item, key, allowed_texts, ids);
                }
            }
        }
        Value::Object(map) => {
            let keys = map.keys().cloned().collect::<Vec<_>>();
            for child_key in keys {
                if let Some(nested) = map.get_mut(&child_key) {
                    sanitize_protocol_json(nested, Some(&child_key), allowed_texts, ids);
                }
            }
        }
    }
}

fn sensitive_field(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    normalized == "token"
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.contains("authorization")
        || normalized.contains("cookie")
        || normalized.contains("sentinel")
}

fn conversation_identity_field(key: &str) -> bool {
    key.eq_ignore_ascii_case("conversation_id")
}

fn message_identity_field(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "id" | "message_id" | "parent_id"
    )
}

fn generic_identity_field(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.ends_with("_id")
}

fn content_field(key: &str) -> bool {
    matches!(key, "text" | "content" | "title")
}

fn structural_string_field(key: &str) -> bool {
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

fn safe_sse_event_name(value: &str) -> String {
    match value {
        "delta" | "delta_encoding" => value.to_owned(),
        _ => "<redacted-event-name>".to_owned(),
    }
}

fn safe_structural_value(field: &str, value: &str) -> String {
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

fn safe_protocol_path(value: &str) -> String {
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

fn count_field(values: &[Value], field: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for value in values {
        if let Some(text) = value.get(field).and_then(Value::as_str) {
            *counts.entry(text.to_owned()).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_experiment() -> ExperimentDefinition {
        toml::from_str(
            r#"
schema = "chatarium-experiment"
version = 1
id = "C03-send-text"

[action]
type = "send_text"
text = "respond with exactly CHATARIUM_PROTOCOL_TEST_001"

[success]
type = "assistant_text_contains"
text = "CHATARIUM_PROTOCOL_TEST_001"
"#,
        )
        .unwrap()
    }

    fn cumulative_export() -> Value {
        let first_chunk = r#"event: delta_encoding
data: "v1"

data: {"type":"resume_conversation_token","token":"signed-secret","conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: {"type":"input_message","input_message":{"id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","author":{"role":"user"},"content":{"content_type":"text","parts":["respond with exactly CHATARIUM_PROTOCOL_TEST_001"]},"status":"finished_successfully","metadata":{"request_id":"dddddddd-dddd-dddd-dddd-dddddddddddd","private_note":"do not leak me"}},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

event: delta
data: {"v":{"message":{"id":"cccccccc-cccc-cccc-cccc-cccccccccccc","author":{"role":"assistant"},"content":{"content_type":"text","parts":[""]},"status":"in_progress","channel":"final"},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}}

event: delta
data: {"p":"/message/content/parts/0","o":"append","v":"CHATARIUM_PROTOCOL_"#;

        let second_chunk = r#"TEST_001"}

data: {"type":"server_ste_metadata","metadata":{"plan_type":"plus","cluster_region":"secret-region","request_id":"eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"},"conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: {"type":"message_stream_complete","conversation_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"}

data: [DONE]

"#;

        json!({
            "format": "chatarium-flight-recorder-export",
            "version": 3,
            "recorderVersion": "0.6.0",
            "sendIntents": [
                {
                    "id": "send-old",
                    "at": "2026-09-29T10:00:01Z",
                    "text": "old private prompt",
                    "state": "pending"
                },
                {
                    "id": "send-new",
                    "at": "2026-09-29T11:00:01Z",
                    "text": "respond with exactly CHATARIUM_PROTOCOL_TEST_001",
                    "state": "confirmed",
                    "observedMessageId": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "confirmedConversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "confirmationEvidence": "protocol-input-message"
                }
            ],
            "assistantWal": {
                "at": "2026-09-29T11:00:02Z",
                "conversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                "observedId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                "text": "CHATARIUM_PROTOCOL_TEST_001",
                "source": "protocol-sse",
                "evidenceSources": ["protocol-sse"],
                "protocolStatus": "finished_successfully",
                "protocolEndTurn": true,
                "protocolIsComplete": true
            },
            "messages": [
                {
                    "observedAt": "2026-09-29T11:00:02Z",
                    "conversation": "conversation:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "observedId": "cccccccc-cccc-cccc-cccc-cccccccccccc",
                    "role": "assistant",
                    "text": "CHATARIUM_PROTOCOL_TEST_001",
                    "source": "protocol-sse",
                    "protocolStatus": "finished_successfully",
                    "protocolEndTurn": true,
                    "protocolEvidence": "delta-completion-patch",
                    "protocolIsComplete": true
                }
            ],
            "events": [
                {
                    "seq": 1,
                    "at": "2026-09-29T10:00:00Z",
                    "kind": "recorder-started",
                    "payload": {"version": "0.5.0"}
                },
                {
                    "seq": 2,
                    "at": "2026-09-29T10:00:02Z",
                    "kind": "network-stream-error",
                    "payload": {"message": "old failure"}
                },
                {
                    "seq": 10,
                    "at": "2026-09-29T11:00:00Z",
                    "kind": "recorder-started",
                    "payload": {"version": "0.6.0"}
                },
                {
                    "seq": 11,
                    "at": "2026-09-29T11:00:01Z",
                    "kind": "network-stream-start",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "endpoint": "/backend-api/f/conversation",
                        "method": "POST",
                        "status": 200,
                        "contentType": "text/event-stream; charset=utf-8"
                    }
                },
                {
                    "seq": 12,
                    "at": "2026-09-29T11:00:01Z",
                    "kind": "network-stream-chunk",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "text": first_chunk
                    }
                },
                {
                    "seq": 13,
                    "at": "2026-09-29T11:00:02Z",
                    "kind": "network-stream-chunk",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "text": second_chunk
                    }
                },
                {
                    "seq": 14,
                    "at": "2026-09-29T11:00:02Z",
                    "kind": "network-stream-end",
                    "payload": {
                        "streamId": "secret-stream-id",
                        "capturedBytes": 1234,
                        "terminal": "sse-done"
                    }
                }
            ]
        })
    }

    #[test]
    fn mismatched_experiment_action_is_rejected() {
        let mut export = cumulative_export();
        export["sendIntents"][1]["text"] = json!("different prompt");
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();

        let error = validate_run_matches_experiment(&export, &selected, &experiment).unwrap_err();
        assert!(error.contains("exact action text"));
    }

    #[test]
    fn latest_run_excludes_historical_events_and_records() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        assert_eq!(selected.started_seq, 10);
        assert_eq!(selected.recorder_version, "0.6.0");
        assert_eq!(selected.events.len(), 5);

        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string(&derived).unwrap();
        assert!(!text.contains("old private prompt"));
        assert!(!text.contains("old failure"));
    }

    #[test]
    fn stream_sanitizer_preserves_canonical_text_and_removes_secrets_and_ids() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(text.contains("respond with exactly CHATARIUM_PROTOCOL_TEST_001"));
        assert!(text.contains("CHATARIUM_PROTOCOL_TEST_001"));
        assert!(!text.contains("signed-secret"));
        assert!(!text.contains("do not leak me"));
        assert!(!text.contains("secret-region"));
        for raw in [
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "dddddddd-dddd-dddd-dddd-dddddddddddd",
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
        ] {
            assert!(!text.contains(raw), "raw identifier survived: {raw}");
        }
        assert!(text.contains("<redacted>"));
        assert!(text.contains("<redacted-value>"));
        assert!(text.contains("<conversation:1>"));
        assert!(text.contains("<message:"));
        assert!(text.contains("\"data\": \"v1\""));
        assert!(text.contains("\"request_id\": \"<id>\""));
    }

    #[test]
    fn unknown_server_structural_strings_are_fail_closed() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let chunk = events
                .iter_mut()
                .find(|event| {
                    event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
                        && event
                            .pointer("/payload/text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("server_ste_metadata"))
                })
                .expect("server metadata chunk");
            chunk["payload"]["text"] = json!(
                "data: {\"type\":\"PRIVATE_CONTROL_SECRET\",\"conversation_id\":\"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb\"}\n\nevent: PRIVATE_EVENT_SECRET\ndata: {\"p\":\"/message/PRIVATE_PATH_SECRET\",\"o\":\"PRIVATE_OP_SECRET\",\"v\":\"PRIVATE_VALUE_SECRET\"}\n\n"
            );
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        for secret in [
            "PRIVATE_CONTROL_SECRET",
            "PRIVATE_EVENT_SECRET",
            "PRIVATE_PATH_SECRET",
            "PRIVATE_OP_SECRET",
            "PRIVATE_VALUE_SECRET",
        ] {
            assert!(
                !text.contains(secret),
                "unknown structural value survived: {secret}"
            );
        }
        assert!(text.contains("<redacted-event-name>"));
        assert!(text.contains("<redacted-path>"));
        assert!(text.contains("<redacted-value>"));
    }

    #[test]
    fn unknown_delta_encoding_is_redacted() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let chunk = events
                .iter_mut()
                .find(|event| {
                    event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
                        && event
                            .pointer("/payload/text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains("delta_encoding"))
                })
                .expect("delta encoding chunk");
            let text = chunk["payload"]["text"].as_str().unwrap().replace(
                "event: delta_encoding\ndata: \"v1\"",
                "event: delta_encoding\ndata: \"PRIVATE_ENCODING_SECRET\"",
            );
            chunk["payload"]["text"] = json!(text);
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("PRIVATE_ENCODING_SECRET"));
        assert!(text.contains("<redacted-value>"));
    }

    #[test]
    fn split_sse_chunks_are_reassembled_and_inventory_is_stable() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();

        assert_eq!(
            derived.pointer("/inventory/streams/0/frame_count"),
            Some(&json!(8))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_encodings/0"),
            Some(&json!("v1"))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/control_type_counts/message_stream_complete"),
            Some(&json!(1))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/completion/done"),
            Some(&json!(true))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_operation_counts/append"),
            Some(&json!(1))
        );
        assert_eq!(
            derived.pointer("/inventory/streams/0/delta_path_counts/~1message~1content~1parts~10"),
            Some(&json!(1))
        );
        assert_eq!(derived.pointer("/inventory/warning_count"), Some(&json!(0)));
    }

    #[test]
    fn malformed_sse_is_fail_closed() {
        let mut export = cumulative_export();
        {
            let events = export["events"].as_array_mut().expect("events array");
            let mut chunks = events.iter_mut().filter(|event| {
                event.get("kind").and_then(Value::as_str) == Some("network-stream-chunk")
            });
            chunks.next().expect("first stream chunk")["payload"]["text"] =
                json!("event: delta\ndata: {PRIVATE RAW BROKEN FRAME}\n\n");
            chunks.next().expect("second stream chunk")["payload"]["text"] = json!("");
        }

        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let derived = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let text = serde_json::to_string_pretty(&derived).unwrap();

        assert!(!text.contains("PRIVATE RAW BROKEN FRAME"));
        assert!(text.contains("<redacted-unparsed-sse>"));
        assert!(
            derived
                .pointer("/inventory/warning_count")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 1
        );
    }

    #[test]
    fn output_derivation_is_deterministic_for_same_input() {
        let export = cumulative_export();
        let selected = select_latest_run(&export).unwrap();
        let experiment = sample_experiment();
        let allowed = BTreeSet::from([
            experiment.action.text.clone().unwrap(),
            experiment.success.text.clone().unwrap(),
        ]);
        let first = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        let second = derive_sanitized_run(&export, &selected, &experiment, &allowed).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn snapshot_does_not_copy_raw_source_or_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("chatarium-flight-test-{nonce}"));
        let input = root.join("PRIVATE-user-export.json");
        let experiment = root.join("C03-send-text.toml");
        let output = root.join("snapshot");
        fs::create_dir_all(&root).unwrap();
        let raw = serde_json::to_vec_pretty(&cumulative_export()).unwrap();
        fs::write(&input, &raw).unwrap();
        fs::write(
            &experiment,
            r#"
schema = "chatarium-experiment"
version = 1
id = "C03-send-text"

[action]
type = "send_text"
text = "respond with exactly CHATARIUM_PROTOCOL_TEST_001"

[success]
type = "assistant_text_contains"
text = "CHATARIUM_PROTOCOL_TEST_001"
"#,
        )
        .unwrap();

        snapshot_flight(&input, &experiment, &output, "C03").unwrap();

        let metadata = fs::read_to_string(output.join("derived/C03.flight.meta.json")).unwrap();
        assert!(metadata.contains(&sha256_hex(&raw)));
        assert!(!metadata.contains("PRIVATE-user-export.json"));
        assert!(!output.join("PRIVATE-user-export.json").exists());

        let public = fs::read_to_string(output.join("evidence/C03.flight.json")).unwrap();
        assert!(!public.contains("signed-secret"));
        assert!(!public.contains("PRIVATE-user-export.json"));

        let _ = fs::remove_dir_all(root);
    }
}
