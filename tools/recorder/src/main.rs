//! Offline protocol-capture ingestion and sanitization tool.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
        "Usage:\n  chatarium-recorder sanitize-har <input.har> <output.har>\n  chatarium-recorder snapshot-har <input.har> <snapshot-dir> <capture-id>\n  chatarium-recorder inspect-har <input.har>\n  chatarium-recorder fingerprint <file>"
    );
}

fn sanitize_har_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut value: Value = serde_json::from_slice(input).map_err(|error| format!("parse HAR JSON: {error}"))?;
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
        let name = object.get("name").and_then(Value::as_str).unwrap_or_default();
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

fn snapshot_har(input: &Path, snapshot_dir: &Path, capture_id: &str) -> Result<(), String> {
    validate_capture_id(capture_id)?;
    let source = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let sanitized = sanitize_har_bytes(&source)?;

    let captures_dir = snapshot_dir.join("captures");
    fs::create_dir_all(&captures_dir)
        .map_err(|error| format!("create {}: {error}", captures_dir.display()))?;

    let capture_path = captures_dir.join(format!("{capture_id}.har"));
    fs::write(&capture_path, &sanitized)
        .map_err(|error| format!("write {}: {error}", capture_path.display()))?;

    let metadata_path = captures_dir.join(format!("{capture_id}.meta.json"));
    let metadata = json!({
        "format": "chatarium-har-capture",
        "version": 1,
        "capture_id": capture_id,
        "source_file": input.file_name().and_then(|name| name.to_str()).unwrap_or("<unknown>"),
        "sanitized_file": capture_path.file_name().and_then(|name| name.to_str()).unwrap_or("<unknown>"),
        "sanitized_sha256": sha256_hex(&sanitized),
        "source_bytes": source.len(),
        "sanitized_bytes": sanitized.len(),
        "created_unix_ms": unix_ms()?,
        "recorder_version": env!("CARGO_PKG_VERSION"),
        "warning": "Sanitization is defense-in-depth, not proof that arbitrary private conversation content is safe to publish. Use controlled captures."
    });
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("serialize capture metadata: {error}"))?;
    fs::write(&metadata_path, metadata_bytes)
        .map_err(|error| format!("write {}: {error}", metadata_path.display()))?;

    println!("capture: {}", capture_path.display());
    println!("metadata: {}", metadata_path.display());
    println!("sha256: {}", sha256_hex(&sanitized));
    Ok(())
}

fn inspect_har(input: &Path) -> Result<(), String> {
    let bytes = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| format!("parse HAR JSON: {error}"))?;
    let entries = value
        .pointer("/log/entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "HAR is missing log.entries".to_owned())?;

    println!("entries: {}", entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let method = entry.pointer("/request/method").and_then(Value::as_str).unwrap_or("?");
        let raw_url = entry.pointer("/request/url").and_then(Value::as_str).unwrap_or("?");
        let status = entry.pointer("/response/status").and_then(Value::as_i64).unwrap_or_default();
        let mime = entry.pointer("/response/content/mimeType").and_then(Value::as_str).unwrap_or("?");
        let endpoint = Url::parse(raw_url)
            .ok()
            .map(|url| format!("{}{}", url.host_str().unwrap_or("?"), url.path()))
            .unwrap_or_else(|| raw_url.split('?').next().unwrap_or(raw_url).to_owned());
        println!("{index:04}  {method:7}  {status:3}  {endpoint}  {mime}");
    }
    Ok(())
}

fn validate_capture_id(capture_id: &str) -> Result<(), String> {
    if capture_id.is_empty()
        || capture_id.contains("..")
        || !capture_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err("capture-id must contain only ASCII letters, digits, '.', '-', '_' and may not contain '..'".to_owned());
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

    #[test]
    fn sanitizer_removes_common_credentials_but_keeps_shape() {
        let input = br#"{
          "log": {
            "entries": [{
              "request": {
                "method": "POST",
                "url": "https://chatgpt.com/backend/test?foo=bar&access_token=url-secret",
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
                  "text": "{\"message\":\"TEST123\",\"session_id\":\"body-secret\",\"nested\":{\"refresh_token\":\"nested-secret\"}}"
                }
              },
              "response": {
                "status": 200,
                "headers": [{"name": "Set-Cookie", "value": "response-secret"}],
                "content": {"mimeType": "application/json", "text": "{\"ok\":true}"}
              }
            }]
          }
        }"#;

        let output = sanitize_har_bytes(input).expect("sanitize");
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
