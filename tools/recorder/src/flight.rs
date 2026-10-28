//! Flight Recorder export ingestion and fail-closed public evidence derivation.

use chatarium_protocol::read::ReadQueryParameterEvidence;
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
const STRING: &str = "<string>";
const BOOL: &str = "<bool>";
const TRUNCATED_READ_BODY: &str = "<truncated-private-json-omitted>";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FlightSanitizationStats {
    sensitive_values_redacted: u64,
    content_values_redacted: u64,
    unknown_scalar_values_redacted: u64,
    numeric_values_generalized: u64,
    conversation_identities_placeholdered: u64,
    message_identities_placeholdered: u64,
    send_identities_placeholdered: u64,
    generic_identities_placeholdered: u64,
    malformed_sse_frames_fail_closed: u64,
    unterminated_sse_tails_redacted: u64,
    unknown_event_names_redacted: u64,
    unknown_structural_values_redacted: u64,
    unknown_paths_redacted: u64,
    unknown_encodings_redacted: u64,
    canonical_experiment_literals_retained: u64,
    read_responses_reduced: u64,
    read_string_values_generalized: u64,
    read_identity_values_placeholdered: u64,
    read_truncated_bodies_omitted: u64,
    malformed_read_json_fail_closed: u64,
    read_query_literals_retained: u64,
    read_query_values_redacted: u64,
}

impl FlightSanitizationStats {
    fn report(self) -> Value {
        json!({
            "format": "chatarium-flight-sanitization-report",
            "version": 1,
            "policy": "flight-selected-run-fail-closed-v1",
            "counts": {
                "sensitive_values_redacted": self.sensitive_values_redacted,
                "content_values_redacted": self.content_values_redacted,
                "unknown_scalar_values_redacted": self.unknown_scalar_values_redacted,
                "numeric_values_generalized": self.numeric_values_generalized,
                "conversation_identities_placeholdered": self.conversation_identities_placeholdered,
                "message_identities_placeholdered": self.message_identities_placeholdered,
                "send_identities_placeholdered": self.send_identities_placeholdered,
                "generic_identities_placeholdered": self.generic_identities_placeholdered,
                "malformed_sse_frames_fail_closed": self.malformed_sse_frames_fail_closed,
                "unterminated_sse_tails_redacted": self.unterminated_sse_tails_redacted,
                "unknown_event_names_redacted": self.unknown_event_names_redacted,
                "unknown_structural_values_redacted": self.unknown_structural_values_redacted,
                "unknown_paths_redacted": self.unknown_paths_redacted,
                "unknown_encodings_redacted": self.unknown_encodings_redacted,
                "raw_network_stream_chunks_copied": 0,
                "raw_read_response_bodies_copied": 0,
                "canonical_experiment_literals_retained": self.canonical_experiment_literals_retained,
                "read_responses_reduced": self.read_responses_reduced,
                "read_string_values_generalized": self.read_string_values_generalized,
                "read_identity_values_placeholdered": self.read_identity_values_placeholdered,
                "read_truncated_bodies_omitted": self.read_truncated_bodies_omitted,
                "malformed_read_json_fail_closed": self.malformed_read_json_fail_closed,
                "read_query_literals_retained": self.read_query_literals_retained,
                "read_query_values_redacted": self.read_query_values_redacted,
            },
            "raw_sensitive_values_retained": false,
            "raw_network_stream_chunks_copied": false,
            "raw_read_response_bodies_copied": false,
            "publication_safety_proven": false,
            "warning": "This report counts selected-run sanitizer transformations; it does not prove arbitrary source material safe to publish."
        })
    }
}

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
    generic: HashMap<String, String>,
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

    fn map_generic(&mut self, raw: &str) -> String {
        stable_placeholder(&mut self.generic, raw, "id")
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
#[cfg(test)]
#[path = "flight/tests.rs"]
mod tests;
#[path = "flight/derive.rs"]
mod derive;
#[path = "flight/policy.rs"]
mod policy;
#[path = "flight/read.rs"]
mod read;
#[path = "flight/selection.rs"]
mod selection;
#[path = "flight/stream.rs"]
mod stream;
pub(super) use derive::*;
pub(super) use policy::*;
pub(super) use read::*;
pub(super) use selection::*;
pub(super) use stream::*;

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
    let selected = select_run_for_experiment(&export, &experiment)?;
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
    let sanitization_report =
        serde_json::to_vec_pretty(derived.get("sanitization_report").ok_or_else(|| {
            "internal flight derivation omitted sanitization report output".to_owned()
        })?)
        .map_err(|error| format!("serialize Flight Recorder sanitization report: {error}"))?;

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

    let sanitization_relative = format!("derived/{capture_id}.flight.sanitization.json");
    let sanitization_path = derived_dir.join(format!("{capture_id}.flight.sanitization.json"));
    fs::write(&sanitization_path, &sanitization_report)
        .map_err(|error| format!("write {}: {error}", sanitization_path.display()))?;

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
        "sanitization_report_file": sanitization_relative,
        "sanitization_report_sha256": sha256_hex(&sanitization_report),
        "sanitization_report_bytes": sanitization_report.len(),
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
    println!("sanitization: {}", sanitization_path.display());
    println!("metadata: {}", metadata_path.display());
    println!("raw-sha256: {}", sha256_hex(&source));
    println!("sanitized-sha256: {}", sha256_hex(&sanitized));
    Ok(())
}

