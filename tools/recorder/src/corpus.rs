//! Validation for committed protocol snapshots and sanitized fixtures.

use chatarium_protocol::sse::{SseFrame, TextTurnProjection, interpret_v1_frame};
use chatarium_protocol::stability::validate_registry;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const SNAPSHOT_SCHEMA: &str = "chatarium-protocol-snapshot";
const SNAPSHOT_VERSION: u64 = 1;
const C03_EXPERIMENT: &str = "C03-send-text";
const C03_USER_TEXT: &str = "respond with exactly CHATARIUM_PROTOCOL_TEST_001";
const C03_ASSISTANT_TEXT: &str = "CHATARIUM_PROTOCOL_TEST_001";

/// Summary of one successful corpus validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusValidationReport {
    /// Number of validated snapshot directories.
    pub snapshots: usize,
    /// Number of validated JSON fixture files.
    pub fixtures: usize,
    /// Number of canonical C03 SSE fixtures replayed through the typed parser.
    pub c03_sse_replays: usize,
}

/// Validate committed snapshot metadata, fixture provenance, and executable C03 evidence.
pub fn validate_corpus(protocol_dir: &Path) -> Result<CorpusValidationReport, String> {
    let snapshots_dir = protocol_dir.join("snapshots");
    let fixtures_dir = protocol_dir.join("fixtures");

    let snapshot_dirs = child_dirs(&snapshots_dir)?;
    let mut revisions = BTreeSet::new();
    let mut errors = Vec::new();

    for snapshot_dir in &snapshot_dirs {
        let revision = file_name(snapshot_dir)?;
        if !valid_revision_id(&revision) {
            errors.push(format!(
                "{}: snapshot directory name is not YYYY-MM-DD.NNN",
                snapshot_dir.display()
            ));
            continue;
        }
        revisions.insert(revision.clone());
        validate_snapshot(snapshot_dir, &revision, &mut errors);
    }

    validate_field_classification_registry(protocol_dir, &revisions, &mut errors);

    let fixture_dirs = child_dirs(&fixtures_dir)?;
    let mut fixture_count = 0_usize;
    let mut c03_sse_replays = 0_usize;

    for fixture_dir in fixture_dirs {
        let revision = file_name(&fixture_dir)?;
        if !valid_revision_id(&revision) {
            errors.push(format!(
                "{}: fixture directory name is not YYYY-MM-DD.NNN",
                fixture_dir.display()
            ));
            continue;
        }
        if !revisions.contains(&revision) {
            errors.push(format!(
                "{}: fixture directory references missing snapshot {revision}",
                fixture_dir.display()
            ));
        }

        for fixture_path in json_files(&fixture_dir)? {
            fixture_count = fixture_count.saturating_add(1);
            match validate_fixture(&fixture_path, &revision) {
                Ok(replayed_c03) => {
                    if replayed_c03 {
                        c03_sse_replays = c03_sse_replays.saturating_add(1);
                    }
                }
                Err(error) => errors.push(format!("{}: {error}", fixture_path.display())),
            }
        }
    }

    if !errors.is_empty() {
        return Err(format!(
            "protocol corpus validation failed ({} error{}):\n- {}",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" },
            errors.join("\n- ")
        ));
    }

    Ok(CorpusValidationReport {
        snapshots: snapshot_dirs.len(),
        fixtures: fixture_count,
        c03_sse_replays,
    })
}

fn validate_field_classification_registry(
    protocol_dir: &Path,
    revisions: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let path = protocol_dir
        .join("schemas")
        .join("field-classification.v1.json");
    let registry = match read_json(&path) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            return;
        }
    };

    if let Err(error) = validate_registry(&registry) {
        errors.push(format!("{}: {error}", path.display()));
        return;
    }

    if let Some(evidence) = registry.get("evidence_revisions").and_then(Value::as_array) {
        for revision in evidence.iter().filter_map(Value::as_str) {
            if !revisions.contains(revision) {
                errors.push(format!(
                    "{}: classification evidence revision {revision} has no committed snapshot",
                    path.display()
                ));
            }
        }
    }
}

fn validate_snapshot(snapshot_dir: &Path, revision: &str, errors: &mut Vec<String>) {
    let manifest_path = snapshot_dir.join("manifest.json");
    match read_json(&manifest_path) {
        Ok(manifest) => {
            if manifest.get("schema").and_then(Value::as_str) != Some(SNAPSHOT_SCHEMA) {
                errors.push(format!(
                    "{}: manifest schema must be {SNAPSHOT_SCHEMA}",
                    manifest_path.display()
                ));
            }
            if manifest.get("version").and_then(Value::as_u64) != Some(SNAPSHOT_VERSION) {
                errors.push(format!(
                    "{}: manifest version must be {SNAPSHOT_VERSION}",
                    manifest_path.display()
                ));
            }
            if manifest.get("revision").and_then(Value::as_str) != Some(revision) {
                errors.push(format!(
                    "{}: manifest revision must equal directory {revision}",
                    manifest_path.display()
                ));
            }
        }
        Err(error) => errors.push(error),
    }

    for name in ["observations.md", "sanitization.md"] {
        let path = snapshot_dir.join(name);
        match fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => {}
            Ok(_) => errors.push(format!("{}: required note is empty", path.display())),
            Err(error) => errors.push(format!("read {}: {error}", path.display())),
        }
    }
}

fn validate_fixture(path: &Path, revision: &str) -> Result<bool, String> {
    let fixture = read_json(path)?;
    let snapshot = fixture
        .get("snapshot")
        .and_then(Value::as_str)
        .ok_or_else(|| "fixture is missing string field 'snapshot'".to_owned())?;
    if snapshot != revision {
        return Err(format!(
            "fixture snapshot '{snapshot}' does not match directory '{revision}'"
        ));
    }

    scan_sensitive_object_values(&fixture, "")?;

    let is_c03_sse = fixture.get("experiment").and_then(Value::as_str) == Some(C03_EXPERIMENT)
        && fixture.get("representative_sequence").is_some();
    if is_c03_sse {
        validate_c03_sse_fixture(&fixture)?;
    }
    Ok(is_c03_sse)
}

fn validate_c03_sse_fixture(fixture: &Value) -> Result<(), String> {
    let sequence = fixture
        .get("representative_sequence")
        .and_then(Value::as_array)
        .ok_or_else(|| "C03 SSE fixture is missing representative_sequence array".to_owned())?;

    let mut projection = TextTurnProjection::default();
    for (index, item) in sequence.iter().enumerate() {
        let event = match item.get("event") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return Err(format!("C03 sequence item {index} has invalid event field")),
        };
        let data = item
            .get("data")
            .ok_or_else(|| format!("C03 sequence item {index} is missing data"))?;
        let encoded_data = if data.as_str() == Some("[DONE]") {
            "[DONE]".to_owned()
        } else {
            serde_json::to_string(data)
                .map_err(|error| format!("serialize C03 sequence item {index}: {error}"))?
        };

        let frame = SseFrame {
            event,
            data: encoded_data,
        };
        let interpreted = interpret_v1_frame(&frame)
            .map_err(|error| format!("interpret C03 sequence item {index}: {}", error.detail))?;
        projection.apply(&interpreted);
    }

    if projection.user_text.as_deref() != Some(C03_USER_TEXT) {
        return Err("C03 replay did not reconstruct the canonical exact user text".to_owned());
    }
    if projection.assistant_text != C03_ASSISTANT_TEXT {
        return Err(format!(
            "C03 replay reconstructed assistant text {:?}, expected {:?}",
            projection.assistant_text, C03_ASSISTANT_TEXT
        ));
    }
    if !projection.assistant_is_complete {
        return Err("C03 replay did not observe assistant is_complete=true".to_owned());
    }
    if !projection.message_stream_complete {
        return Err("C03 replay did not observe message_stream_complete".to_owned());
    }
    if !projection.done {
        return Err("C03 replay did not observe terminal [DONE]".to_owned());
    }

    Ok(())
}

fn scan_sensitive_object_values(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let child_path = format!("{path}/{}", escape_pointer_segment(key));
                if sensitive_value_key(key) && !safe_sensitive_placeholder(nested) {
                    return Err(format!(
                        "credential-bearing object field {child_path} contains a publishable value instead of null/redaction placeholder"
                    ));
                }
                scan_sensitive_object_values(nested, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                scan_sensitive_object_values(nested, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn sensitive_value_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy_authorization"
            | "cookie"
            | "set_cookie"
            | "csrf"
            | "xsrf"
            | "password"
            | "passwd"
            | "api_key"
            | "apikey"
            | "access_key"
            | "access_token"
            | "refresh_token"
            | "session_id"
            | "sessionid"
            | "session_token"
            | "device_id"
            | "deviceid"
            | "token"
    ) || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.contains("csrf_token")
        || normalized.contains("xsrf_token")
        || normalized.contains("auth_token")
        || normalized.contains("sentinel")
}

fn safe_sensitive_placeholder(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => {
            let trimmed = text.trim();
            (trimmed.starts_with('<') && trimmed.ends_with('>'))
                || trimmed.eq_ignore_ascii_case("redacted")
        }
        _ => false,
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn child_dirs(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(path)
        .map_err(|error| format!("read directory {}: {error}", path.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn json_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(path)
        .map_err(|error| format!("read directory {}: {error}", path.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
        {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))
}

fn valid_revision_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 14
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'.'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10) || byte.is_ascii_digit())
}

fn escape_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_protocol() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("chatarium-corpus-test-{nonce}"));
        write_test_snapshot(&root, "2026-09-29.001");
        write_test_snapshot(&root, "2026-09-29.002");
        fs::create_dir_all(root.join("fixtures/2026-09-29.002")).unwrap();
        write_test_classification_registry(&root);
        root
    }

    fn write_test_snapshot(root: &Path, revision: &str) {
        let dir = root.join("snapshots").join(revision);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "schema": SNAPSHOT_SCHEMA,
                "version": SNAPSHOT_VERSION,
                "revision": revision
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(dir.join("observations.md"), "# Observations\nObserved.").unwrap();
        fs::write(dir.join("sanitization.md"), "# Sanitization\nControlled.").unwrap();
    }

    fn c03_fixture() -> Value {
        json!({
            "snapshot": "2026-09-29.002",
            "experiment": C03_EXPERIMENT,
            "representative_sequence": [
                {"event": "delta_encoding", "data": "v1"},
                {
                    "event": null,
                    "data": {
                        "type": "input_message",
                        "input_message": {
                            "id": "<USER_MESSAGE_ID>",
                            "author": {"role": "user"},
                            "content": {"content_type": "text", "parts": [C03_USER_TEXT]},
                            "status": "finished_successfully"
                        },
                        "conversation_id": "<CONVERSATION_ID>"
                    }
                },
                {
                    "event": "delta",
                    "data": {
                        "v": {
                            "message": {
                                "id": "<ASSISTANT_MESSAGE_ID>",
                                "author": {"role": "assistant"},
                                "content": {"content_type": "text", "parts": [""]},
                                "status": "in_progress",
                                "end_turn": null,
                                "channel": "final"
                            },
                            "conversation_id": "<CONVERSATION_ID>"
                        }
                    }
                },
                {
                    "event": "delta",
                    "data": {
                        "p": "/message/content/parts/0",
                        "o": "append",
                        "v": C03_ASSISTANT_TEXT
                    }
                },
                {
                    "event": "delta",
                    "data": {
                        "p": "",
                        "o": "patch",
                        "v": [
                            {"p": "/message/status", "o": "replace", "v": "finished_successfully"},
                            {"p": "/message/end_turn", "o": "replace", "v": true},
                            {"p": "/message/metadata", "o": "append", "v": {"is_complete": true}}
                        ]
                    }
                },
                {"event": null, "data": {"type": "message_stream_complete", "conversation_id": "<CONVERSATION_ID>"}},
                {"event": null, "data": "[DONE]"}
            ]
        })
    }

    fn write_test_classification_registry(root: &Path) {
        fs::create_dir_all(root.join("schemas")).unwrap();
        fs::write(
            root.join("schemas/field-classification.v1.json"),
            serde_json::to_vec_pretty(chatarium_protocol::stability::registry()).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn valid_minimal_corpus_passes_and_replays_c03() {
        let root = temp_protocol();
        fs::write(
            root.join("fixtures/2026-09-29.002/c03.json"),
            serde_json::to_vec_pretty(&c03_fixture()).unwrap(),
        )
        .unwrap();

        let report = validate_corpus(&root).unwrap();
        assert_eq!(
            report,
            CorpusValidationReport {
                snapshots: 2,
                fixtures: 1,
                c03_sse_replays: 1,
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classification_registry_missing_evidence_snapshot_fails() {
        let root = temp_protocol();
        let path = root.join("schemas/field-classification.v1.json");
        let mut registry = read_json(&path).unwrap();
        registry["evidence_revisions"] = json!(["2026-09-29.002", "2026-09-30.001"]);
        fs::write(&path, serde_json::to_vec_pretty(&registry).unwrap()).unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("classification evidence revision 2026-09-30.001"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_revision_mismatch_fails() {
        let root = temp_protocol();
        let path = root.join("snapshots/2026-09-29.002/manifest.json");
        let mut manifest = read_json(&path).unwrap();
        manifest["revision"] = json!("2026-09-29.999");
        fs::write(&path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("manifest revision must equal directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_required_snapshot_note_fails() {
        let root = temp_protocol();
        fs::remove_file(root.join("snapshots/2026-09-29.002/sanitization.md")).unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("sanitization.md"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fixture_snapshot_mismatch_fails() {
        let root = temp_protocol();
        let mut fixture = c03_fixture();
        fixture["snapshot"] = json!("2026-09-29.001");
        fs::write(
            root.join("fixtures/2026-09-29.002/c03.json"),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("does not match directory"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sensitive_object_value_must_be_placeholder() {
        let root = temp_protocol();
        let fixture = json!({
            "snapshot": "2026-09-29.002",
            "access_token": "reusable-secret"
        });
        fs::write(
            root.join("fixtures/2026-09-29.002/secret.json"),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("credential-bearing object field"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sensitive_placeholder_is_allowed() {
        let root = temp_protocol();
        let fixture = json!({
            "snapshot": "2026-09-29.002",
            "access_token": "<REDACTED_TOKEN>"
        });
        fs::write(
            root.join("fixtures/2026-09-29.002/redacted.json"),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        assert!(validate_corpus(&root).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn c03_semantic_regression_fails() {
        let root = temp_protocol();
        let mut fixture = c03_fixture();
        fixture["representative_sequence"][3]["data"]["v"] = json!("WRONG");
        fs::write(
            root.join("fixtures/2026-09-29.002/c03.json"),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let error = validate_corpus(&root).unwrap_err();
        assert!(error.contains("reconstructed assistant text"));
        let _ = fs::remove_dir_all(root);
    }
}
