//! Offline protocol-capture ingestion, sanitization, and structural inventory tool.

pub mod corpus;
pub mod flight;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const REDACTED: &str = "<redacted>";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HarSanitizationStats {
    sensitive_values_redacted: u64,
    url_query_values_redacted: u64,
    url_credentials_redacted: u64,
    urls_rewritten: u64,
    embedded_json_documents_rewritten: u64,
}

impl HarSanitizationStats {
    fn report(self) -> Value {
        json!({
            "format": "chatarium-har-sanitization-report",
            "version": 1,
            "policy": "har-defense-in-depth-v1",
            "counts": {
                "sensitive_values_redacted": self.sensitive_values_redacted,
                "url_query_values_redacted": self.url_query_values_redacted,
                "url_credentials_redacted": self.url_credentials_redacted,
                "urls_rewritten": self.urls_rewritten,
                "embedded_json_documents_rewritten": self.embedded_json_documents_rewritten,
            },
            "raw_sensitive_values_retained": false,
            "publication_safety_proven": false,
            "warning": "This report counts sanitizer transformations; it does not prove arbitrary source material safe to publish."
        })
    }
}

/// Run the existing recorder command line interface with arguments excluding argv[0].
pub fn run_cli(args: &[String]) -> Result<(), String> {
    match args {
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
        [command, protocol_dir] if command == "validate-corpus" => {
            let report = corpus::validate_corpus(Path::new(protocol_dir))?;
            println!(
                "snapshots={} fixtures={} c03_sse_replays={} read_fixtures={}",
                report.snapshots, report.fixtures, report.c03_sse_replays, report.read_fixtures
            );
            Ok(())
        }
        [command, input, snapshot_dir, capture_id] if command == "snapshot-har" => {
            snapshot_har(Path::new(input), Path::new(snapshot_dir), capture_id)
        }
        [command, input, experiment, snapshot_dir, capture_id] if command == "snapshot-flight" => {
            flight::snapshot_flight(
                Path::new(input),
                Path::new(experiment),
                Path::new(snapshot_dir),
                capture_id,
            )
        }
        [command, input] if command == "fingerprint" => {
            println!("{}  {}", fingerprint_file(Path::new(input))?, input);
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
        "Usage:\n  chatarium-recorder sanitize-har <input.har> <output.har>\n  chatarium-recorder inventory-har <input.har> <output.json>\n  chatarium-recorder snapshot-har <input.har> <snapshot-dir> <capture-id>\n  chatarium-recorder snapshot-flight <input.json> <experiment.toml> <snapshot-dir> <capture-id>\n  chatarium-recorder inspect-har <input.har>\n  chatarium-recorder validate-corpus <protocol-dir>\n  chatarium-recorder fingerprint <file>"
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

/// Parse and sanitize HAR JSON bytes, returning pretty-printed sanitized JSON.
pub fn sanitize_har_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    sanitize_har_bytes_with_report(input).map(|(bytes, _)| bytes)
}

fn sanitize_har_bytes_with_report(input: &[u8]) -> Result<(Vec<u8>, Value), String> {
    let mut value = parse_har(input)?;
    let mut stats = HarSanitizationStats::default();
    sanitize_value_with_stats(&mut value, &mut stats);
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("serialize sanitized HAR: {error}"))?;
    Ok((bytes, stats.report()))
}

/// Sanitize sensitive values in a JSON structure using the recorder's shared rules.
pub fn sanitize_value(value: &mut Value) {
    let mut stats = HarSanitizationStats::default();
    sanitize_value_with_stats(value, &mut stats);
}

fn sanitize_value_with_stats(value: &mut Value, stats: &mut HarSanitizationStats) {
    match value {
        Value::Array(items) => {
            for item in items {
                sanitize_value_with_stats(item, stats);
            }
        }
        Value::Object(map) => {
            if let Some(value) = map.get_mut("cookies") {
                sanitize_name_value_array(value, true, stats);
            }
            for field in ["headers", "queryString", "params"] {
                if let Some(value) = map.get_mut(field) {
                    sanitize_name_value_array(value, false, stats);
                }
            }

            if let Some(Value::String(url)) = map.get_mut("url") {
                let sanitized = sanitize_url(url, stats);
                if sanitized != *url {
                    stats.urls_rewritten = stats.urls_rewritten.saturating_add(1);
                    *url = sanitized;
                }
            }

            if let Some(Value::String(text)) = map.get_mut("text") {
                sanitize_embedded_json(text, stats);
            }

            for (key, nested) in map.iter_mut() {
                if sensitive_name(key) {
                    if !is_redacted_value(nested) {
                        stats.sensitive_values_redacted =
                            stats.sensitive_values_redacted.saturating_add(1);
                    }
                    *nested = Value::String(REDACTED.to_owned());
                } else {
                    sanitize_value_with_stats(nested, stats);
                }
            }
        }
        _ => {}
    }
}

fn sanitize_name_value_array(
    value: &mut Value,
    redact_all: bool,
    stats: &mut HarSanitizationStats,
) {
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
            if let Some(current) = object.get("value") {
                if !is_redacted_value(current) {
                    stats.sensitive_values_redacted =
                        stats.sensitive_values_redacted.saturating_add(1);
                }
            }
            if object.contains_key("value") {
                object.insert("value".to_owned(), Value::String(REDACTED.to_owned()));
            }
        }
    }
}

fn sanitize_embedded_json(text: &mut String, stats: &mut HarSanitizationStats) {
    let Ok(mut nested) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let before = nested.clone();
    sanitize_value_with_stats(&mut nested, stats);
    if nested != before {
        stats.embedded_json_documents_rewritten =
            stats.embedded_json_documents_rewritten.saturating_add(1);
    }
    if let Ok(serialized) = serde_json::to_string(&nested) {
        *text = serialized;
    }
}

fn sanitize_url(raw: &str, stats: &mut HarSanitizationStats) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.to_owned();
    };

    let query = url
        .query_pairs()
        .map(|(name, value)| {
            let value = if sensitive_name(&name) {
                stats.url_query_values_redacted = stats.url_query_values_redacted.saturating_add(1);
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
        stats.url_credentials_redacted = stats.url_credentials_redacted.saturating_add(1);
        let _ = url.set_username(REDACTED);
    }
    if url.password().is_some() {
        stats.url_credentials_redacted = stats.url_credentials_redacted.saturating_add(1);
        let _ = url.set_password(Some(REDACTED));
    }

    url.to_string()
}

fn is_redacted_value(value: &Value) -> bool {
    value.as_str() == Some(REDACTED)
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

/// Generate the value-free structural request inventory from a HAR value.
pub fn request_inventory(har: &Value) -> Result<Value, String> {
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

/// Derive a deterministic frontend script/stylesheet identity manifest from sanitized HAR evidence.
pub fn frontend_asset_manifest(har: &Value) -> Result<Value, String> {
    let entries = har_entries(har)?;
    let mut assets = entries
        .iter()
        .filter_map(frontend_asset_row)
        .collect::<Vec<_>>();
    assets.sort_by_cached_key(|value| serde_json::to_string(value).unwrap_or_default());

    let hashed_asset_count = assets
        .iter()
        .filter(|asset| {
            asset
                .get("body_sha256")
                .is_some_and(|value| !value.is_null())
        })
        .count();
    let warning_count = assets
        .iter()
        .filter(|asset| {
            asset
                .get("body_warning")
                .is_some_and(|value| !value.is_null())
        })
        .count();

    Ok(json!({
        "format": "chatarium-frontend-asset-manifest",
        "version": 1,
        "asset_count": assets.len(),
        "hashed_asset_count": hashed_asset_count,
        "warning_count": warning_count,
        "assets": assets,
    }))
}

fn frontend_asset_row(entry: &Value) -> Option<Value> {
    let resource_type = entry
        .get("_resourceType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mime = entry
        .pointer("/response/content/mimeType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = frontend_asset_kind(resource_type, mime)?;

    let raw_url = entry.pointer("/request/url").and_then(Value::as_str)?;
    let (host, path) = asset_url_shape(raw_url);
    let status = entry
        .pointer("/response/status")
        .and_then(Value::as_i64)
        .unwrap_or_default();

    let content = entry.pointer("/response/content");
    let text = content
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str);
    let raw_encoding = content
        .and_then(|value| value.get("encoding"))
        .and_then(Value::as_str);
    let public_encoding = raw_encoding.map(|value| {
        if value.eq_ignore_ascii_case("base64") {
            "base64"
        } else {
            "<unsupported>"
        }
    });

    let (body_available, decoded_body_bytes, body_sha256, body_warning) =
        decode_asset_body(text, raw_encoding);

    Some(json!({
        "kind": kind,
        "host": host,
        "path": path,
        "status": status,
        "mime_type": mime,
        "content_encoding": public_encoding,
        "body_available": body_available,
        "decoded_body_bytes": decoded_body_bytes,
        "body_sha256": body_sha256,
        "body_warning": body_warning,
    }))
}

fn frontend_asset_kind(resource_type: &str, mime: &str) -> Option<&'static str> {
    let resource_type = resource_type.to_ascii_lowercase();
    let mime = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();

    if resource_type == "script"
        || matches!(
            mime.as_str(),
            "application/javascript"
                | "text/javascript"
                | "application/x-javascript"
                | "application/ecmascript"
                | "text/ecmascript"
        )
    {
        return Some("script");
    }
    if resource_type == "stylesheet" || mime == "text/css" {
        return Some("stylesheet");
    }
    None
}

fn asset_url_shape(raw_url: &str) -> (String, String) {
    let Ok(url) = Url::parse(raw_url) else {
        return (
            "?".to_owned(),
            normalize_path(raw_url.split('?').next().unwrap_or(raw_url)),
        );
    };
    (
        url.host_str().unwrap_or("?").to_owned(),
        normalize_path(url.path()),
    )
}

fn decode_asset_body(
    text: Option<&str>,
    encoding: Option<&str>,
) -> (bool, Option<usize>, Option<String>, Option<&'static str>) {
    let Some(text) = text else {
        return (false, None, None, None);
    };

    match encoding {
        None => {
            let bytes = text.as_bytes();
            (true, Some(bytes.len()), Some(sha256_hex(bytes)), None)
        }
        Some(value) if value.eq_ignore_ascii_case("base64") => match decode_base64_standard(text) {
            Ok(bytes) => (true, Some(bytes.len()), Some(sha256_hex(&bytes)), None),
            Err(()) => (false, None, None, Some("invalid-base64-content")),
        },
        Some(_) => (false, None, None, Some("unsupported-content-encoding")),
    }
}

fn decode_base64_standard(text: &str) -> Result<Vec<u8>, ()> {
    let input = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if input.len() % 4 != 0 {
        return Err(());
    }

    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for (chunk_index, chunk) in input.chunks_exact(4).enumerate() {
        let last_chunk = chunk_index + 1 == input.len() / 4;
        let padding = match (chunk[2] == b'=', chunk[3] == b'=') {
            (true, true) => 2,
            (false, true) => 1,
            (false, false) => 0,
            (true, false) => return Err(()),
        };
        if padding > 0 && !last_chunk {
            return Err(());
        }

        let a = base64_value(chunk[0]).ok_or(())?;
        let b = base64_value(chunk[1]).ok_or(())?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            base64_value(chunk[2]).ok_or(())?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            base64_value(chunk[3]).ok_or(())?
        };

        if padding == 2 && (b & 0x0f) != 0 {
            return Err(());
        }
        if padding == 1 && (c & 0x03) != 0 {
            return Err(());
        }

        output.push((a << 2) | (b >> 4));
        if padding < 2 {
            output.push((b << 4) | (c >> 2));
        }
        if padding == 0 {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
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
        return (
            "?".to_owned(),
            normalize_path(raw_url.split('?').next().unwrap_or(raw_url)),
        );
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
        && segment.chars().enumerate().all(|(index, character)| {
            [8, 13, 18, 23].contains(&index) || character.is_ascii_hexdigit()
        });
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

/// Write sanitized evidence and derived inventory/metadata for a HAR file.
pub fn snapshot_har(input: &Path, snapshot_dir: &Path, capture_id: &str) -> Result<(), String> {
    validate_capture_id(capture_id)?;
    let source = fs::read(input).map_err(|error| format!("read {}: {error}", input.display()))?;
    let (sanitized, sanitization_report) = sanitize_har_bytes_with_report(&source)?;
    let sanitization_bytes = serde_json::to_vec_pretty(&sanitization_report)
        .map_err(|error| format!("serialize HAR sanitization report: {error}"))?;
    let sanitized_value = parse_har(&sanitized)?;
    let inventory = request_inventory(&sanitized_value)?;
    let inventory_bytes = serde_json::to_vec_pretty(&inventory)
        .map_err(|error| format!("serialize request inventory: {error}"))?;
    let frontend_assets = frontend_asset_manifest(&sanitized_value)?;
    let frontend_asset_bytes = serde_json::to_vec_pretty(&frontend_assets)
        .map_err(|error| format!("serialize frontend asset manifest: {error}"))?;

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

    let relative_sanitization_path = format!("derived/{capture_id}.sanitization.json");
    let sanitization_path = derived_dir.join(format!("{capture_id}.sanitization.json"));
    fs::write(&sanitization_path, &sanitization_bytes)
        .map_err(|error| format!("write {}: {error}", sanitization_path.display()))?;

    let relative_frontend_assets_path = format!("derived/{capture_id}.frontend-assets.json");
    let frontend_assets_path = derived_dir.join(format!("{capture_id}.frontend-assets.json"));
    fs::write(&frontend_assets_path, &frontend_asset_bytes)
        .map_err(|error| format!("write {}: {error}", frontend_assets_path.display()))?;

    let metadata_path = derived_dir.join(format!("{capture_id}.meta.json"));
    let entry_count = inventory
        .get("entry_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let asset_count = frontend_assets
        .get("asset_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let hashed_asset_count = frontend_assets
        .get("hashed_asset_count")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let asset_warning_count = frontend_assets
        .get("warning_count")
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
        "sanitization_report_file": relative_sanitization_path,
        "sanitization_report_sha256": sha256_hex(&sanitization_bytes),
        "sanitization_report_bytes": sanitization_bytes.len(),
        "frontend_asset_manifest_file": relative_frontend_assets_path,
        "frontend_asset_manifest_sha256": sha256_hex(&frontend_asset_bytes),
        "frontend_asset_manifest_bytes": frontend_asset_bytes.len(),
        "frontend_asset_count": asset_count,
        "frontend_asset_hashed_count": hashed_asset_count,
        "frontend_asset_warning_count": asset_warning_count,
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
    println!("sanitization: {}", sanitization_path.display());
    println!("frontend-assets: {}", frontend_assets_path.display());
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
        let index = entry
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let method = entry.get("method").and_then(Value::as_str).unwrap_or("?");
        let status = entry
            .get("status")
            .and_then(Value::as_i64)
            .unwrap_or_default();
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

/// Return the lowercase SHA-256 fingerprint of bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Fingerprint a file using the same SHA-256 representation as the CLI.
pub fn fingerprint_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(sha256_hex(&bytes))
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

    fn asset_har() -> &'static [u8] {
        br#"{
          "log": {
            "entries": [
              {
                "_resourceType": "script",
                "request": {
                  "method": "GET",
                  "url": "https://cdn.example.test/assets/app.js?build=123&access_token=asset-secret",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 200,
                  "headers": [],
                  "content": {
                    "mimeType": "application/javascript; charset=utf-8",
                    "text": "console.log('asset');"
                  }
                }
              },
              {
                "_resourceType": "stylesheet",
                "request": {
                  "method": "GET",
                  "url": "https://cdn.example.test/assets/app.css?v=456",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 200,
                  "headers": [],
                  "content": {
                    "mimeType": "text/css",
                    "encoding": "base64",
                    "text": "Ym9keXtjb2xvcjpyZWR9"
                  }
                }
              },
              {
                "_resourceType": "fetch",
                "request": {
                  "method": "GET",
                  "url": "https://chatgpt.com/backend-api/config",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 200,
                  "headers": [],
                  "content": {
                    "mimeType": "application/json",
                    "text": "{\"feature\":true}"
                  }
                }
              },
              {
                "_resourceType": "image",
                "request": {
                  "method": "GET",
                  "url": "https://cdn.example.test/assets/logo.png",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 200,
                  "headers": [],
                  "content": {
                    "mimeType": "image/png",
                    "encoding": "base64",
                    "text": "iVBORw0KGgo="
                  }
                }
              },
              {
                "_resourceType": "script",
                "request": {
                  "method": "GET",
                  "url": "https://cdn.example.test/assets/lazy.js",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 304,
                  "headers": [],
                  "content": {
                    "mimeType": "application/javascript"
                  }
                }
              },
              {
                "_resourceType": "script",
                "request": {
                  "method": "GET",
                  "url": "https://cdn.example.test/assets/broken.js",
                  "headers": [],
                  "cookies": [],
                  "queryString": []
                },
                "response": {
                  "status": 200,
                  "headers": [],
                  "content": {
                    "mimeType": "application/javascript",
                    "encoding": "base64",
                    "text": "%%%PRIVATE_MALFORMED_BASE64%%%"
                  }
                }
              }
            ]
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
    fn har_sanitization_report_counts_transformations_without_leaking_values() {
        let (_sanitized, report) = sanitize_har_bytes_with_report(sample_har()).unwrap();
        let report_text = serde_json::to_string(&report).unwrap();

        assert!(
            report
                .pointer("/counts/sensitive_values_redacted")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                >= 6
        );
        assert_eq!(
            report.pointer("/counts/url_query_values_redacted"),
            Some(&json!(1))
        );
        assert_eq!(report.pointer("/counts/urls_rewritten"), Some(&json!(1)));
        assert_eq!(
            report.pointer("/counts/embedded_json_documents_rewritten"),
            Some(&json!(1))
        );
        for secret in [
            "header-secret",
            "cookie-secret",
            "query-secret",
            "url-secret",
            "body-secret",
            "nested-secret",
            "response-secret",
        ] {
            assert!(!report_text.contains(secret));
        }
        assert_eq!(
            report.get("publication_safety_proven"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn har_sanitization_report_is_deterministic() {
        let (_, first) = sanitize_har_bytes_with_report(sample_har()).unwrap();
        let (_, second) = sanitize_har_bytes_with_report(sample_har()).unwrap();
        assert_eq!(first, second);
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
    fn frontend_asset_manifest_includes_only_code_assets_and_hashes_decoded_bodies() {
        let sanitized = sanitize_har_bytes(asset_har()).expect("sanitize assets");
        let value = parse_har(&sanitized).expect("parse assets");
        let manifest = frontend_asset_manifest(&value).expect("asset manifest");
        let assets = manifest["assets"].as_array().expect("assets array");

        assert_eq!(manifest["format"], "chatarium-frontend-asset-manifest");
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["asset_count"], 4);
        assert_eq!(manifest["hashed_asset_count"], 2);
        assert_eq!(manifest["warning_count"], 1);

        let script = assets
            .iter()
            .find(|asset| asset["path"] == "/assets/app.js")
            .expect("plain script asset");
        assert_eq!(script["kind"], "script");
        assert_eq!(script["host"], "cdn.example.test");
        assert_eq!(script["body_available"], true);
        assert_eq!(script["body_sha256"], sha256_hex(b"console.log('asset');"));
        assert_eq!(
            script["decoded_body_bytes"],
            json!(b"console.log('asset');".len())
        );

        let css = assets
            .iter()
            .find(|asset| asset["path"] == "/assets/app.css")
            .expect("base64 stylesheet asset");
        assert_eq!(css["kind"], "stylesheet");
        assert_eq!(css["content_encoding"], "base64");
        assert_eq!(css["body_available"], true);
        assert_eq!(css["body_sha256"], sha256_hex(b"body{color:red}"));
        assert_eq!(css["decoded_body_bytes"], json!(b"body{color:red}".len()));

        let missing = assets
            .iter()
            .find(|asset| asset["path"] == "/assets/lazy.js")
            .expect("missing body asset");
        assert_eq!(missing["body_available"], false);
        assert!(missing["body_sha256"].is_null());
        assert!(missing["body_warning"].is_null());

        let broken = assets
            .iter()
            .find(|asset| asset["path"] == "/assets/broken.js")
            .expect("malformed base64 asset");
        assert_eq!(broken["body_available"], false);
        assert!(broken["body_sha256"].is_null());
        assert_eq!(broken["body_warning"], "invalid-base64-content");

        let text = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(!text.contains("backend-api/config"));
        assert!(!text.contains("logo.png"));
        assert!(!text.contains("asset-secret"));
        assert!(!text.contains("PRIVATE_MALFORMED_BASE64"));
        assert!(!text.contains("?build="));
        assert!(!text.contains("?v="));
    }

    #[test]
    fn frontend_asset_manifest_is_deterministic() {
        let sanitized = sanitize_har_bytes(asset_har()).unwrap();
        let value = parse_har(&sanitized).unwrap();
        assert_eq!(
            frontend_asset_manifest(&value).unwrap(),
            frontend_asset_manifest(&value).unwrap()
        );
    }

    #[test]
    fn path_normalizer_preserves_endpoint_names() {
        assert_eq!(
            normalize_path(
                "/backend-api/conversation/01234567-89ab-cdef-0123-456789abcdef/messages"
            ),
            "/backend-api/conversation/<id>/messages"
        );
        assert_eq!(
            normalize_path("/backend-api/conversation_limit_info"),
            "/backend-api/conversation_limit_info"
        );
        assert_eq!(
            normalize_path("/api/account/123456789012"),
            "/api/account/<id>"
        );
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

    #[test]
    fn library_har_api_matches_json_value_sanitizer() {
        let mut value = serde_json::from_slice::<Value>(sample_har()).unwrap();
        sanitize_value(&mut value);
        let from_value = serde_json::to_vec_pretty(&value).unwrap();
        assert_eq!(sanitize_har_bytes(sample_har()).unwrap(), from_value);
    }

    #[test]
    fn snapshot_har_writes_and_hashes_sanitization_report() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("chatarium-har-report-test-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("private.har");
        let output = root.join("snapshot");
        fs::write(&input, sample_har()).unwrap();

        snapshot_har(&input, &output, "C03").unwrap();

        let report_path = output.join("derived/C03.sanitization.json");
        let report_bytes = fs::read(&report_path).unwrap();
        let metadata: Value =
            serde_json::from_slice(&fs::read(output.join("derived/C03.meta.json")).unwrap())
                .unwrap();
        assert_eq!(
            metadata["sanitization_report_file"],
            "derived/C03.sanitization.json"
        );
        assert_eq!(
            metadata["sanitization_report_sha256"],
            sha256_hex(&report_bytes)
        );
        let report_text = String::from_utf8(report_bytes).unwrap();
        assert!(!report_text.contains("header-secret"));
        assert!(!report_text.contains("private.har"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_har_writes_and_hashes_frontend_asset_manifest() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("chatarium-asset-manifest-test-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let input = root.join("private-assets.har");
        let output = root.join("snapshot");
        fs::write(&input, asset_har()).unwrap();

        snapshot_har(&input, &output, "C00").unwrap();

        let asset_path = output.join("derived/C00.frontend-assets.json");
        let asset_bytes = fs::read(&asset_path).unwrap();
        let manifest: Value = serde_json::from_slice(&asset_bytes).unwrap();
        let metadata: Value =
            serde_json::from_slice(&fs::read(output.join("derived/C00.meta.json")).unwrap())
                .unwrap();

        assert_eq!(
            metadata["frontend_asset_manifest_file"],
            "derived/C00.frontend-assets.json"
        );
        assert_eq!(
            metadata["frontend_asset_manifest_sha256"],
            sha256_hex(&asset_bytes)
        );
        assert_eq!(
            metadata["frontend_asset_manifest_bytes"],
            json!(asset_bytes.len())
        );
        assert_eq!(metadata["frontend_asset_count"], 4);
        assert_eq!(metadata["frontend_asset_hashed_count"], 2);
        assert_eq!(metadata["frontend_asset_warning_count"], 1);
        assert_eq!(manifest["asset_count"], 4);

        let text = String::from_utf8(asset_bytes).unwrap();
        assert!(!text.contains("asset-secret"));
        assert!(!text.contains("PRIVATE_MALFORMED_BASE64"));
        assert!(!text.contains("private-assets.har"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn value_sanitizer_is_schema_agnostic() {
        let mut value = json!({
            "metadata": {"access_token": "sensitive"},
            "records": [{"headers": [{"name": "Authorization", "value": "Bearer sensitive"}] }]
        });
        sanitize_value(&mut value);
        assert_eq!(value["metadata"]["access_token"], REDACTED);
        assert_eq!(value["records"][0]["headers"][0]["value"], REDACTED);
    }
}
