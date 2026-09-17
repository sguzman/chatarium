//! Offline protocol-capture ingestion, sanitization, and structural inventory tool.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const REDACTED: &str = "<redacted>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chatarium-recorder: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, input, output] if command == "sanitize-har" => {
            let bytes = fs::read(input).map_err(|error| format!("read {input}: {error}"))?;
            let sanitized = sanitize_har_bytes(&bytes)?;
            fs::write(output, &sanitized).map_err(|error| format!("write {output}: {error}"))?;
            println!("{}  {}", sha256_hex(&sanitized), output);
            Ok(())
        }
        [command, input, output] if command == "inventory-har" => {
            let bytes = fs::read(input).map_err(|error| format!("read {input}: {error}"))?;
            let sanitized = sanitize_har_bytes(&bytes)?;
            let value = parse_har(&sanitized)?;
            let inventory = request_inventory(&value)?;
            let output_bytes = serde_json::to_vec_pretty(&inventory)
                .map_err(|error| format!("serialize request inventory: {error}"))?;
            fs::write(output, &output_bytes).map_err(|error| format!("write {output}: {error}"))?;
            println!("{}  {}", sha256_hex(&output_bytes), output);
            Ok(())
        }
        [command, input] if command == "inspect-har" => inspect_har(Path::new(input)),
        [command, input, snapshot_dir, capture_id] if command == "snapshot-har" => {
            snapshot_har(Path::new(input), Path::new(snapshot_dir), capture_id)
        }
        [command, input] if command == "fingerprint" => {
            let bytes = fs::read(input).map_err(|error| format!("read {input}: {error}"))?;
            println!("{}  {}", sha256_hex(&bytes), input);
            Ok(())
        }
        _ => {
            print_usage();
            Err("invalid arguments".to_owned())
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:\n  chatarium-recorder sanitize-har <input.har> <output.har>\n  chatarium-recorder inventory-har <input.har> <output.json>\n  chatarium-recorder snapshot-har <input.har> <snapshot-dir> <capture-id>\n  chatarium-recorder inspect-har <input.har>\n  chatarium-recorder fingerprint <file>"
    );
}

fn parse_har(input: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(input).map_err(|error| format!("parse HAR JSON: {error}"))
}

fn har_entries(value: &Value) -> Result<&Vec<Value>, String> {
    value
        .pointer("/log/entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "HAR is missing log.entries".to_owned())
}

fn sanitize_har_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut value = parse_har(input)?;
    sanitize_value(&mut value);
    serde_json::to_vec_pretty(&value).map_err(|error| format!("serialize sanitized HAR: {error}"))
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                sanitize_value(item);
            }
        }
        Value::Object(map) => {
            if let Some(value) = map.get_mut("cookies") {
                sanitize_name_value_array(value, true);
            }
            for field in ["headers", "queryString", "params"] {
                if let Some(value) = map.get_mut(field) {
                    sanitize_name_value_array(value, false);
                }
            }

            if let Some(Value::String(url)) = map.get_mut("url") {
                *url = sanitize_url(url);
            }

            if let Some(Value::String(text)) = map.get_mut("text") {
                sanitize_embedded_json(text);
            }

            for (key, nested) in map.iter_mut() {
                if sensitive_name(key) {
                    *nested = Value::String(REDACTED.to_owned());
                } else {
                    sanitize_value(nested);
                }
            }
        }
        _ => {}
    }
}

fn sanitize_name_value_array(value: &mut Value, redact_all: bool) {
    let Some(items) = value.as_array_mut() else {
        return;
    };

    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if redact_all || sensitive_name(name) {
            if object.contains_key("value") {
                object.insert("value".to_owned(), Value::String(REDACTED.to_owned()));
            }
        }
    }
}

fn sanitize_embedded_json(text: &mut String) {
    let Ok(mut nested) = serde_json::from_str::<Value>(text) else {
        return;
    };
    sanitize_value(&mut nested);
    if let Ok(serialized) = serde_json::to_string(&nested) {
        *text = serialized;
    }
}

fn sanitize_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_owned();
    };

    let query = url
        .query_pairs()
        .map(|(name, value)| {
            let value = if sensitive_name(&name) {
                REDACTED.to_owned()
            } else {
                value.into_owned()
            };
            (name.into_owned(), value)
        })
        .collect::<Vec<_>>();

    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }

    if !url.username().is_empty() {
        let _ = url.set_username(REDACTED);
    }
    if url.password().is_some() {
        let _ = url.set_password(Some(REDACTED));
    }

    url.to_string()
}

fn sensitive_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    const EXACT: &[&str] = &[
        "authorization",
        "proxy_authorization",
        "cookie",
        "set_cookie",
        "csrf",
        "xsrf",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "access_key",
        "access_token",
        "refresh_token",
        "session_id",
        "sessionid",
        "session_token",
        "device_id",
        "deviceid",
    ];

    EXACT.contains(&normalized.as_str())
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.contains("csrf_token")
        || normalized.contains("xsrf_token")
        || normalized.contains("auth_token")
        || normalized.contains("sentinel")
}

fn request_inventory(har: &Value) -> Result<Value, String> {
    let entries = har_entries(har)?;
    let rows = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| inventory_row(index, entry))
        .collect::<Vec<_>>();

    Ok(json!({
        "format": "chatarium-request-inventory",
        "version": 1,
        "entry_count": rows.len(),
        "entries": rows,
    }))
}

fn inventory_row(index: usize, entry: &Value) -> Value {
    let method = entry
        .pointer("/request/method")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let raw_url = entry
        .pointer("/request/url")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let status = entry
        .pointer("/response/status")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let mime = entry
        .pointer("/response/content/mimeType")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let request_mime = entry
        .pointer("/request/postData/mimeType")
        .and_then(Value::as_str);
    let has_request_body = entry.pointer("/request/postData/text").is_some();
    let resource_type = entry.get("_resourceType").and_then(Value::as_str);

    let (host, path) = endpoint_shape(raw_url);
    let query_names = name_list(entry.pointer("/request/queryString"));
    let request_header_names = name_list(entry.pointer("/request/headers"));
    let response_header_names = name_list(entry.pointer("/response/headers"));

    json!({
        "index": index,
        "method": method,
        "host": host,
        "path": path,
        "status": status,
        "response_mime": mime,
        "request_mime": request_mime,
        "has_request_body": has_request_body,
        "resource_type": resource_type,
        "query_names": query_names,
        "request_header_names": request_header_names,
        "response_header_names": response_header_names,
    })
}

fn name_list(value: Option<&Value>) -> Vec<String> {
    let mut names = BTreeSet::new();
    if let Some(items) = value.and_then(Value::as_array) {
        for item in items {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                names.insert(name.to_ascii_lowercase());
            }
        }
    }
    names.into_iter().collect()
}

fn endpoint_shape(raw_url: &str) -> (String, String) {
    let Ok(url) = Url::parse(raw_url) else {
        return ("?".to_owned(), normalize_path(raw_url.split('?').next().unwrap_or(raw_url)));
    };
    (
        url.host_str().unwrap_or("?").to_owned(),
        normalize_path(url.path()),
    )
}

fn normalize_path(path: &str) -> String {
    let normalized = path
        .split('/')
        .map(|segment| {
            if looks_like_instance_id(segment) {
                "<id>"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized
    }
}

fn looks_like_instance_id(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }

    let bytes = segment.as_bytes();
    let uuid_shape = bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && segment
            .chars()
            .enumerate()
            .all(|(index, character)| [8, 13, 18, 23].contains(&index) || character.is_ascii_hexdigit());
    if uuid_shape {
        return true;
    }

    if segment.len() >= 12 && segment.chars().all(|character| character.is_ascii_digit()) {
        return true;
    }

    segment.len() >= 20
        && segment.chars().any(|character| character.is_ascii_digit())
        && segment
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn snapshot_har(input: &Path, snapshot_dir: &Path, capture_id: &str) -> Result<(), String> {
    validate_capture_id(capture_id)?;
    let source = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let sanitized = sanitize_har_bytes(&source)?;
    let sanitized_value = parse_har(&sanitized)?;
    let inventory = request_inventory(&sanitized_value)?;
    let inventory_bytes = serde_json::to_vec_pretty(&inventory)
        .map_err(|error| format!("serialize request inventory: {error}"))?;

    let evidence_dir = snapshot_dir.join("evidence");
    let derived_dir = snapshot_dir.join("derived");
    fs::create_dir_all(&evidence_dir)
        .map_err(|error| format!("create {}: {error}", evidence_dir.display()))?;
    fs::create_dir_all(&derived_dir)
        .map_err(|error| format!("create {}: {error}", derived_dir.display()))?;

    let relative_capture_path = format!("evidence/{capture_id}.har.json");
    let capture_path = evidence_dir.join(format!("{capture_id}.har.json"));
    fs::write(&capture_path, &sanitized)
        .map_err(|error| format!("write {}: {error}", capture_path.display()))?;

    let relative_inventory_path = format!("derived/{capture_id}.requests.json");
    let inventory_path = derived_dir.join(format!("{capture_id}.requests.json"));
    fs::write(&inventory_path, &inventory_bytes)
        .map_err(|error| format!("write {}: {error}", inventory_path.display()))?;

    let metadata_path = derived_dir.join(format!("{capture_id}.meta.json"));
    let entry_count = inventory
        .get("entry_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let metadata = json!({
        "format": "chatarium-har-capture",
        "version": 2,
        "capture_id": capture_id,
        "sanitized_file": relative_capture_path,
        "sanitized_sha256": sha256_hex(&sanitized),
        "sanitized_bytes": sanitized.len(),
        "request_inventory_file": relative_inventory_path,
        "request_inventory_sha256": sha256_hex(&inventory_bytes),
        "entry_count": entry_count,
        "created_unix_ms": unix_ms()?,
        "recorder_version": env!("CARGO_PKG_VERSION"),
        "raw_retained_outside_git": true,
        "warning": "Sanitization is defense-in-depth, not proof that arbitrary private conversation content is safe to publish. Use controlled captures."
    });
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("serialize capture metadata: {error}"))?;
    fs::write(&metadata_path, metadata_bytes)
        .map_err(|error| format!("write {}: {error}", metadata_path.display()))?;

    println!("capture: {}", capture_path.display());
    println!("inventory: {}", inventory_path.display());
    println!("metadata: {}", metadata_path.display());
    println!("sha256: {}", sha256_hex(&sanitized));
    Ok(())
}

fn inspect_har(input: &Path) -> Result<(), String> {
    let bytes = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let sanitized = sanitize_har_bytes(&bytes)?;
    let value = parse_har(&sanitized)?;
    let inventory = request_inventory(&value)?;
    let entries = inventory
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "generated inventory is missing entries".to_owned())?;

    println!("entries: {}", entries.len());
    for entry in entries {
        let index = entry.get("index").and_then(Value::as_u64).unwrap_or_default();
        let method = entry.get("method").and_then(Value::as_str).unwrap_or("?");
        let status = entry.get("status").and_then(Value::as_i64).unwrap_or_default();
        let host = entry.get("host").and_then(Value::as_str).unwrap_or("?");
        let path = entry.get("path").and_then(Value::as_str).unwrap_or("?");
        let mime = entry
            .get("response_mime")
            .and_then(Value::as_str)
            .unwrap_or("?");
        println!("{index:04}  {method:7}  {status:3}  {host}{path}  {mime}");
    }
    Ok(())
}

fn validate_capture_id(capture_id: &str) -> Result<(), String> {
    if capture_id.is_empty()
        || capture_id.contains("..")
        || !capture_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(
            "capture-id must contain only ASCII letters, digits, '.', '-', '_' and may not contain '..'"
                .to_owned(),
        );
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_ms() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| format!("system clock before Unix epoch: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_har() -> &'static [u8] {
        br#"{
          "log": {
            "entries": [{
              "_resourceType": "fetch",
              "request": {
                "method": "POST",
                "url": "https://chatgpt.com/backend-api/conversation/01234567-89ab-cdef-0123-456789abcdef?foo=bar&access_token=url-secret",
                "headers": [
                  {"name": "Authorization", "value": "Bearer header-secret"},
                  {"name": "Content-Type", "value": "application/json"}
                ],
                "cookies": [{"name": "session", "value": "cookie-secret"}],
                "queryString": [
                  {"name": "foo", "value": "bar"},
                  {"name": "access_token", "value": "query-secret"}
                ],
                "postData": {
                  "mimeType": "application/json",
                  "text": "{\"message\":\"TEST123\",\"session_id\":\"body-secret\",\"nested\":{\"refresh_token\":\"nested-secret\"}}"
                }
              },
              "response": {
                "status": 200,
                "headers": [
                  {"name": "Set-Cookie", "value": "response-secret"},
                  {"name": "Content-Type", "value": "text/event-stream"}
                ],
                "content": {"mimeType": "text/event-stream", "text": "data: TEST123"}
              }
            }]
          }
        }"#
    }

    #[test]
    fn sanitizer_removes_common_credentials_but_keeps_shape() {
        let output = sanitize_har_bytes(sample_har()).expect("sanitize");
        let text = String::from_utf8(output).expect("utf8");
        assert!(!text.contains("header-secret"));
        assert!(!text.contains("cookie-secret"));
        assert!(!text.contains("query-secret"));
        assert!(!text.contains("url-secret"));
        assert!(!text.contains("body-secret"));
        assert!(!text.contains("nested-secret"));
        assert!(!text.contains("response-secret"));
        assert!(text.contains("TEST123"));
        assert!(text.contains("application/json"));
        assert!(text.contains("foo=bar"));
        assert!(text.contains(REDACTED));
    }

    #[test]
    fn inventory_keeps_structure_without_values_or_instance_ids() {
        let sanitized = sanitize_har_bytes(sample_har()).expect("sanitize");
        let value = parse_har(&sanitized).expect("parse");
        let inventory = request_inventory(&value).expect("inventory");
        let text = serde_json::to_string(&inventory).expect("json");

        assert!(text.contains("/backend-api/conversation/<id>"));
        assert!(text.contains("access_token"));
        assert!(text.contains("authorization"));
        assert!(text.contains("text/event-stream"));
        assert!(!text.contains("url-secret"));
        assert!(!text.contains("header-secret"));
        assert!(!text.contains("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!text.contains("TEST123"));
    }

    #[test]
    fn path_normalizer_preserves_endpoint_names() {
        assert_eq!(
            normalize_path("/backend-api/conversation/01234567-89ab-cdef-0123-456789abcdef/messages"),
            "/backend-api/conversation/<id>/messages"
        );
        assert_eq!(normalize_path("/backend-api/conversation_limit_info"), "/backend-api/conversation_limit_info");
        assert_eq!(normalize_path("/api/account/123456789012"), "/api/account/<id>");
    }

    #[test]
    fn capture_id_rejects_path_traversal() {
        assert!(validate_capture_id("C03-send-text").is_ok());
        assert!(validate_capture_id("../oops").is_err());
        assert!(validate_capture_id("bad/name").is_err());
    }

    #[test]
    fn fingerprint_is_stable() {
        assert_eq!(
            sha256_hex(b"chatarium"),
            "6e6953accffab7da7e0a6c14e6d9b2d7f9f33e84c5ee5dc045872bccb9a64f7d"
        );
    }
}
