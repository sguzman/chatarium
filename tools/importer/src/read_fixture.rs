//! Import one explicitly selected sanitized C01/C02 read fixture entry into the durable journal.
//!
//! This module never infers which captured request has semantic meaning. The caller selects the
//! read_responses index explicitly after protocol evidence has been reviewed.

use chatarium_core::RemoteReadObservationId;
use chatarium_protocol::read::{JsonTopLevelType, ReadExperiment, ReadMethod, ReadObservation};
use chatarium_store::remote_read_audit::{
    RemoteReadObservationProvenance, record_remote_read_observation_from_fixture,
    replay_remote_read_audit,
};
use chatarium_store::{EventStore, JsonlEventStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const ALLOWED_FIXTURE_FIELDS: &[&str] = &["snapshot", "experiment", "status", "read_responses"];

const ALLOWED_READ_FIELDS: &[&str] = &[
    "read",
    "method",
    "path",
    "query_keys",
    "status",
    "content_type",
    "captured_bytes",
    "truncated",
    "body_present",
    "top_level_type",
    "body",
];

#[derive(Debug)]
pub struct ReadFixtureImportSummary {
    pub source_sha256: String,
    pub archive_path: PathBuf,
    pub journal_path: PathBuf,
    pub observation_id: RemoteReadObservationId,
    pub selected_index: usize,
    pub appended: bool,
}

pub fn import_file(
    fixture_path: &Path,
    selected_index: usize,
    data_dir: &Path,
) -> Result<ReadFixtureImportSummary, String> {
    let bytes = fs::read(fixture_path)
        .map_err(|error| format!("read {}: {error}", fixture_path.display()))?;
    import_bytes(&bytes, selected_index, data_dir)
}

fn import_bytes(
    bytes: &[u8],
    selected_index: usize,
    data_dir: &Path,
) -> Result<ReadFixtureImportSummary, String> {
    let fixture: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse read fixture JSON: {error}"))?;
    let observation = observation_from_fixture(&fixture, selected_index)?;

    let source_sha256 = sha256_hex(bytes);
    let provenance = RemoteReadObservationProvenance::sanitized_read_fixture(
        source_sha256.clone(),
        u64::try_from(selected_index)
            .map_err(|_| "selected read index cannot be represented as u64".to_owned())?,
    )?;
    let observation_id = deterministic_observation_id(&source_sha256, selected_index)?;
    let archive_path = archive_fixture(data_dir, &source_sha256, bytes)?;
    let journal_path = data_dir.join("journal.jsonl");
    let mut store = JsonlEventStore::open(&journal_path)
        .map_err(|error| format!("open {}: {error}", journal_path.display()))?;

    let existing = replay_remote_read_audit(store.events())?;
    if let Some(record) = existing
        .iter()
        .find(|record| record.observation_id == observation_id)
    {
        if record.observation != observation || record.provenance.as_ref() != Some(&provenance) {
            return Err(format!(
                "deterministic remote read observation ID {} already exists with conflicting content",
                observation_id
            ));
        }

        return Ok(ReadFixtureImportSummary {
            source_sha256,
            archive_path,
            journal_path,
            observation_id,
            selected_index,
            appended: false,
        });
    }

    if let Some(record) = existing.iter().find(|record| {
        record
            .provenance
            .as_ref()
            .is_some_and(|existing| existing == &provenance)
    }) {
        return Err(format!(
            "sanitized fixture provenance already exists under unexpected observation ID {}",
            record.observation_id
        ));
    }

    record_remote_read_observation_from_fixture(
        &mut store,
        observation_id,
        &observation,
        &provenance,
    )
    .map_err(|error| {
        format!(
            "append read observation {} to {}: {error}",
            observation_id,
            journal_path.display()
        )
    })?;

    Ok(ReadFixtureImportSummary {
        source_sha256,
        archive_path,
        journal_path,
        observation_id,
        selected_index,
        appended: true,
    })
}

fn observation_from_fixture(
    fixture: &Value,
    selected_index: usize,
) -> Result<ReadObservation, String> {
    fixture
        .as_object()
        .ok_or_else(|| "sanitized read fixture must be a JSON object".to_owned())?;

    validate_fixture_publication_safety(fixture)?;

    let protocol_revision = required_string(fixture, "snapshot")?.to_owned();
    if protocol_revision.is_empty() {
        return Err("sanitized read fixture snapshot must not be empty".to_owned());
    }

    let experiment_name = required_string(fixture, "experiment")?;
    let experiment = ReadExperiment::from_stable_name(experiment_name).ok_or_else(|| {
        format!("unsupported sanitized read fixture experiment {experiment_name:?}")
    })?;

    let reads = fixture
        .get("read_responses")
        .and_then(Value::as_array)
        .ok_or_else(|| "sanitized read fixture is missing read_responses array".to_owned())?;
    if reads.is_empty() {
        return Err("sanitized read fixture contains no read responses".to_owned());
    }
    let read = reads.get(selected_index).ok_or_else(|| {
        format!(
            "selected read index {selected_index} is out of range for {} read response(s)",
            reads.len()
        )
    })?;
    read.as_object()
        .ok_or_else(|| format!("read response {selected_index} must be a JSON object"))?;

    let method_name = required_string(read, "method")?;
    let method = ReadMethod::from_stable_name(method_name).ok_or_else(|| {
        format!("read response {selected_index} has unsupported method {method_name:?}")
    })?;
    let path = required_string(read, "path")?.to_owned();
    let query_keys = optional_string_array(read, "query_keys")?;
    let status = required_u16(read, "status")?;
    let content_type = required_string(read, "content_type")?.to_owned();
    let truncated = optional_bool(read, "truncated")?.unwrap_or(false);

    let body = read.get("body");

    let derived_body_present = body.is_some_and(|value| !value.is_null());
    let body_present = optional_bool(read, "body_present")?.unwrap_or(derived_body_present);
    if body_present != derived_body_present {
        return Err(format!(
            "read response {selected_index} body_present={body_present} conflicts with sanitized body presence={derived_body_present}"
        ));
    }

    let derived_top_level_type = if body_present && !truncated {
        body.and_then(json_top_level_type)
    } else {
        None
    };
    let explicit_top_level_type = optional_top_level_type(read, selected_index)?;
    if explicit_top_level_type.is_some() && explicit_top_level_type != derived_top_level_type {
        return Err(format!(
            "read response {selected_index} top_level_type conflicts with sanitized body structure"
        ));
    }
    let top_level_type = explicit_top_level_type.or(derived_top_level_type);

    ReadObservation::new(
        protocol_revision,
        experiment,
        method,
        path,
        query_keys,
        status,
        content_type,
        truncated,
        body_present,
        top_level_type,
    )
    .map_err(|error| format!("invalid selected read response {selected_index}: {error}"))
}

fn validate_fixture_publication_safety(fixture: &Value) -> Result<(), String> {
    let fixture_object = fixture
        .as_object()
        .ok_or_else(|| "sanitized read fixture must be a JSON object".to_owned())?;
    for key in fixture_object.keys() {
        if !ALLOWED_FIXTURE_FIELDS.contains(&key.as_str()) {
            return Err(format!(
                "sanitized read fixture contains unsupported top-level field {key:?}"
            ));
        }
    }

    let reads = fixture
        .get("read_responses")
        .and_then(Value::as_array)
        .ok_or_else(|| "sanitized read fixture is missing read_responses array".to_owned())?;
    if reads.is_empty() {
        return Err("sanitized read fixture contains no read responses".to_owned());
    }

    for (index, read) in reads.iter().enumerate() {
        let object = read
            .as_object()
            .ok_or_else(|| format!("read response {index} must be a JSON object"))?;
        for key in object.keys() {
            if !ALLOWED_READ_FIELDS.contains(&key.as_str()) {
                return Err(format!(
                    "read response {index} contains unsupported field {key:?}"
                ));
            }
        }
        if let Some(body) = read.get("body") {
            validate_placeholder_body(body, &format!("/read_responses/{index}/body"))?;
        }
    }

    Ok(())
}

fn validate_placeholder_body(value: &Value, pointer: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                validate_placeholder_body(nested, &format!("{pointer}/{key}"))?;
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                validate_placeholder_body(nested, &format!("{pointer}/{index}"))?;
            }
        }
        Value::String(text) => {
            if !(text.starts_with('<') && text.ends_with('>')) {
                return Err(format!(
                    "sanitized read body at {pointer} contains non-placeholder string content"
                ));
            }
        }
        Value::Null => {}
        Value::Bool(_) | Value::Number(_) => {
            return Err(format!(
                "sanitized read body at {pointer} contains raw scalar instead of typed placeholder"
            ));
        }
    }
    Ok(())
}

fn json_top_level_type(value: &Value) -> Option<JsonTopLevelType> {
    Some(match value {
        Value::Object(_) => JsonTopLevelType::Object,
        Value::Array(_) => JsonTopLevelType::Array,
        Value::String(_) => JsonTopLevelType::String,
        Value::Number(_) => JsonTopLevelType::Number,
        Value::Bool(_) => JsonTopLevelType::Bool,
        Value::Null => JsonTopLevelType::Null,
    })
}

fn optional_top_level_type(
    value: &Value,
    selected_index: usize,
) -> Result<Option<JsonTopLevelType>, String> {
    match value.get("top_level_type") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => JsonTopLevelType::from_stable_name(raw)
            .map(Some)
            .ok_or_else(|| {
                format!("read response {selected_index} has unsupported top_level_type {raw:?}")
            }),
        Some(_) => Err(format!(
            "read response {selected_index} top_level_type must be string or null"
        )),
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("sanitized read fixture is missing string field {field:?}"))
}

fn required_u16(value: &Value, field: &str) -> Result<u16, String> {
    let raw = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("selected read response is missing integer field {field:?}"))?;
    u16::try_from(raw).map_err(|_| format!("selected read response field {field:?} exceeds u16"))
}

fn optional_bool(value: &Value, field: &str) -> Result<Option<bool>, String> {
    match value.get(field) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!(
            "selected read response field {field:?} must be boolean when present"
        )),
    }
}

fn optional_string_array(value: &Value, field: &str) -> Result<Vec<String>, String> {
    match value.get(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                    format!("selected read response field {field:?} contains non-string")
                })
            })
            .collect(),
        Some(_) => Err(format!(
            "selected read response field {field:?} must be an array when present"
        )),
    }
}

fn deterministic_observation_id(
    source_sha256: &str,
    selected_index: usize,
) -> Result<RemoteReadObservationId, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"chatarium-sanitized-read-fixture-v1\0");
    hasher.update(source_sha256.as_bytes());
    hasher.update(b"\0");
    hasher.update(selected_index.to_string().as_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // UUID version 5 + RFC 4122 variant. The value is content-derived and used only as a local
    // deterministic observation identity; it is not claimed to be an RFC namespace UUID.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let formatted = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    RemoteReadObservationId::from_str(&formatted)
        .map_err(|error| format!("construct deterministic remote read observation ID: {error}"))
}

fn archive_fixture(data_dir: &Path, sha256: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let archive_dir = data_dir.join("imports").join("read-fixture");
    fs::create_dir_all(&archive_dir)
        .map_err(|error| format!("create {}: {error}", archive_dir.display()))?;
    let archive_path = archive_dir.join(format!("{sha256}.json"));

    if archive_path.exists() {
        let existing = fs::read(&archive_path).map_err(|error| {
            format!("read existing archive {}: {error}", archive_path.display())
        })?;
        if sha256_hex(&existing) != sha256 {
            return Err(format!(
                "existing sanitized fixture archive {} does not match its content-addressed filename",
                archive_path.display()
            ));
        }
        return Ok(archive_path);
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive_path)
        .map_err(|error| format!("create archive {}: {error}", archive_path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write archive {}: {error}", archive_path.display()))?;
    file.flush()
        .map_err(|error| format!("flush archive {}: {error}", archive_path.display()))?;
    file.sync_data()
        .map_err(|error| format!("sync archive {}: {error}", archive_path.display()))?;
    Ok(archive_path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_protocol::Compatibility;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-read-fixture-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn fixture_bytes() -> Vec<u8> {
        serde_json::to_vec_pretty(&serde_json::json!({
            "snapshot": "2026-09-30.001",
            "experiment": "C01-conversation-list",
            "status": "partial_observation",
            "read_responses": [{
                "method": "GET",
                "path": "/backend-api/gizmos/snorlax/sidebar",
                "query_keys": ["conversations_per_gizmo", "limit", "owned_only"],
                "status": 200,
                "content_type": "application/json",
                "body": {
                    "items": [{
                        "gizmo": {"gizmo": {"id": "<id>"}},
                        "conversations": {
                            "items": [{
                                "id": "<id>",
                                "title": "<string>",
                                "create_time": "<number>"
                            }],
                            "cursor": "<string>"
                        }
                    }],
                    "cursor": "<string>"
                }
            }]
        }))
        .unwrap()
    }

    #[test]
    fn committed_c01_fixture_is_importable_without_semantic_promotion() {
        let dir = temp_dir("committed-c01");
        let bytes =
            include_bytes!("../../../protocol/fixtures/2026-09-30.001/c01-sidebar-read.json");

        let summary = import_bytes(bytes, 0, &dir).unwrap();
        let store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        let records = replay_remote_read_audit(store.events()).unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].observation_id, summary.observation_id);
        assert_eq!(
            records[0].observation.experiment(),
            ReadExperiment::ConversationList
        );
        assert_eq!(records[0].observation.protocol_revision(), "2026-09-30.001");
        assert_eq!(records[0].compatibility, Compatibility::NoBaseline);
        assert_eq!(
            records[0].observation.path(),
            "/backend-api/gizmos/snorlax/sidebar"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn sanitized_fixture_import_is_idempotent_and_typed() {
        let dir = temp_dir("idempotent");
        let bytes = fixture_bytes();

        let first = import_bytes(&bytes, 0, &dir).unwrap();
        assert!(first.appended);
        let second = import_bytes(&bytes, 0, &dir).unwrap();
        assert!(!second.appended);
        assert_eq!(first.observation_id, second.observation_id);

        let store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].observation_id, first.observation_id);
        assert_eq!(
            records[0].observation.experiment(),
            ReadExperiment::ConversationList
        );
        assert_eq!(records[0].observation.protocol_revision(), "2026-09-30.001");
        assert_eq!(records[0].compatibility, Compatibility::NoBaseline);
        assert_eq!(
            records[0]
                .provenance
                .as_ref()
                .map(|value| value.source_read_index),
            Some(0)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn explicit_index_selects_only_requested_read() {
        let dir = temp_dir("index");
        let mut fixture: Value = serde_json::from_slice(&fixture_bytes()).unwrap();
        let second = serde_json::json!({
            "method": "GET",
            "path": "/backend-api/second",
            "query_keys": [],
            "status": 200,
            "content_type": "application/json",
            "body": {"ok": "<bool>"}
        });
        fixture["read_responses"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let bytes = serde_json::to_vec(&fixture).unwrap();

        let summary = import_bytes(&bytes, 1, &dir).unwrap();
        let store = JsonlEventStore::open(dir.join("journal.jsonl")).unwrap();
        let records = replay_remote_read_audit(store.events()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].observation_id, summary.observation_id);
        assert_eq!(records[0].observation.path(), "/backend-api/second");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn private_content_in_unselected_read_also_fails_before_archive() {
        let dir = temp_dir("unselected-private");
        let mut fixture: Value = serde_json::from_slice(&fixture_bytes()).unwrap();
        fixture["read_responses"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "method": "GET",
                "path": "/backend-api/unselected",
                "query_keys": [],
                "status": 200,
                "content_type": "application/json",
                "body": {"title": "PRIVATE UNSELECTED TITLE"}
            }));
        let bytes = serde_json::to_vec(&fixture).unwrap();

        let error = import_bytes(&bytes, 0, &dir).unwrap_err();
        assert!(error.contains("non-placeholder string content"));
        assert!(!error.contains("PRIVATE UNSELECTED TITLE"));
        assert!(!dir.join("imports").exists());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn raw_private_fields_fail_closed_without_echoing_values() {
        let dir = temp_dir("private");
        let mut fixture: Value = serde_json::from_slice(&fixture_bytes()).unwrap();
        fixture["read_responses"][0]["bodyText"] =
            Value::String("PRIVATE CONVERSATION CONTENT".to_owned());
        let bytes = serde_json::to_vec(&fixture).unwrap();

        let error = import_bytes(&bytes, 0, &dir).unwrap_err();
        assert!(error.contains("unsupported field"));
        assert!(!error.contains("PRIVATE CONVERSATION CONTENT"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn arbitrary_string_or_scalar_body_content_fails_closed() {
        for unsafe_body in [
            serde_json::json!({"title": "PRIVATE TITLE"}),
            serde_json::json!({"count": 42}),
            serde_json::json!({"enabled": true}),
        ] {
            let dir = temp_dir("unsafe-body");
            let mut fixture: Value = serde_json::from_slice(&fixture_bytes()).unwrap();
            fixture["read_responses"][0]["body"] = unsafe_body;
            let bytes = serde_json::to_vec(&fixture).unwrap();

            assert!(import_bytes(&bytes, 0, &dir).is_err());
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn out_of_range_index_fails_closed() {
        let dir = temp_dir("range");
        let error = import_bytes(&fixture_bytes(), 9, &dir).unwrap_err();
        assert!(error.contains("out of range"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unsupported_experiment_fails_closed() {
        let dir = temp_dir("experiment");
        let mut fixture: Value = serde_json::from_slice(&fixture_bytes()).unwrap();
        fixture["experiment"] = Value::String("C99-invented".to_owned());
        let bytes = serde_json::to_vec(&fixture).unwrap();

        assert!(import_bytes(&bytes, 0, &dir).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
