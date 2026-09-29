//! Compatibility checks for the documented recorder command line interface.

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> std::path::PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "chatarium-recorder-cli-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn documented_cli_commands_remain_operational() {
    let dir = temp_dir();
    let input = dir.join("sample.har");
    let sanitized = dir.join("sanitized.har");
    let inventory = dir.join("inventory.json");
    fs::write(&input, br#"{"log":{"entries":[{"request":{"method":"GET","url":"https://chatgpt.com/backend-api/test?access_token=secret","headers":[]},"response":{"status":200,"headers":[],"content":{"mimeType":"application/json"}}}]}}"#).unwrap();

    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_chatarium-recorder"))
            .args(args)
            .output()
            .unwrap()
    };
    let sanitized_run = invoke(&[
        "sanitize-har",
        input.to_str().unwrap(),
        sanitized.to_str().unwrap(),
    ]);
    assert!(sanitized_run.status.success());
    let sanitized_bytes = fs::read(&sanitized).unwrap();
    assert!(!String::from_utf8_lossy(&sanitized_bytes).contains("secret"));
    assert_eq!(
        sanitized_bytes,
        chatarium_recorder::sanitize_har_bytes(&fs::read(&input).unwrap()).unwrap()
    );

    let inventory_run = invoke(&[
        "inventory-har",
        input.to_str().unwrap(),
        inventory.to_str().unwrap(),
    ]);
    assert!(inventory_run.status.success());
    let inventory_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&inventory).unwrap()).unwrap();
    assert_eq!(inventory_value["entry_count"], 1);
    let sanitized_value: serde_json::Value = serde_json::from_slice(&sanitized_bytes).unwrap();
    assert_eq!(
        inventory_value,
        chatarium_recorder::request_inventory(&sanitized_value).unwrap()
    );

    let inspect_run = invoke(&["inspect-har", input.to_str().unwrap()]);
    assert!(inspect_run.status.success());
    assert!(
        String::from_utf8(inspect_run.stdout)
            .unwrap()
            .contains("chatgpt.com/backend-api/test")
    );

    let fingerprint_run = invoke(&["fingerprint", input.to_str().unwrap()]);
    assert!(fingerprint_run.status.success());
    assert!(
        String::from_utf8(fingerprint_run.stdout)
            .unwrap()
            .contains(input.to_str().unwrap())
    );

    let snapshot_dir = dir.join("snapshot");
    let snapshot_run = invoke(&[
        "snapshot-har",
        input.to_str().unwrap(),
        snapshot_dir.to_str().unwrap(),
        "C01",
    ]);
    assert!(snapshot_run.status.success());
    assert!(snapshot_dir.join("evidence/C01.har.json").exists());
    assert!(snapshot_dir.join("derived/C01.requests.json").exists());
    assert!(snapshot_dir.join("derived/C01.sanitization.json").exists());
    assert!(
        snapshot_dir
            .join("derived/C01.frontend-assets.json")
            .exists()
    );
    assert!(snapshot_dir.join("derived/C01.meta.json").exists());

    let after_inventory = dir.join("inventory-after.json");
    fs::write(&after_inventory, fs::read(&inventory).unwrap()).unwrap();
    let diff = dir.join("diff.json");
    let diff_run = Command::new(env!("CARGO_BIN_EXE_chatarium-inventory-diff"))
        .args([
            inventory.to_str().unwrap(),
            after_inventory.to_str().unwrap(),
            diff.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(diff_run.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(diff).unwrap()).unwrap()["summary"]["unchanged_endpoints"],
        1
    );

    let flight_before = dir.join("flight-before.json");
    let flight_after = dir.join("flight-after.json");
    let flight_diff = dir.join("flight-diff.json");
    let base_flight = serde_json::json!({
        "format": "chatarium-flight-inventory",
        "version": 1,
        "experiment_id": "C03-send-text",
        "recorder_version": "0.6.0",
        "selected_run": {
            "started_seq": 10,
            "started_at": "2026-09-29T11:00:00Z",
            "event_count": 5
        },
        "event_kind_counts": {"network-stream-chunk": 2},
        "send_state_counts": {"confirmed": 1},
        "confirmation_evidence_counts": {"protocol-input-message": 1},
        "message_role_counts": {"assistant": 1, "user": 1},
        "message_source_counts": {"protocol-sse": 2},
        "assistant_wal": {"present": true, "protocol_is_complete": true},
        "streams": [{
            "stream": "stream-1",
            "endpoint": "/backend-api/f/conversation",
            "method": "POST",
            "status": 200,
            "content_type": "text/event-stream; charset=utf-8",
            "frame_count": 2,
            "named_event_counts": {"delta": 1},
            "control_type_counts": {"message_stream_complete": 1},
            "delta_operation_counts": {"append": 1},
            "delta_path_counts": {"/message/content/parts/0": 1},
            "marker_counts": {},
            "delta_encodings": ["v1"],
            "completion": {"message_stream_complete": true, "done": true, "terminal": "sse-done"},
            "parse_warning_count": 0
        }],
        "warning_count": 0
    });
    let mut changed_flight = base_flight.clone();
    changed_flight["streams"][0]["control_type_counts"]["conversation_detail_metadata"] =
        serde_json::json!(1);
    fs::write(
        &flight_before,
        serde_json::to_vec_pretty(&base_flight).unwrap(),
    )
    .unwrap();
    fs::write(
        &flight_after,
        serde_json::to_vec_pretty(&changed_flight).unwrap(),
    )
    .unwrap();

    let flight_diff_run = Command::new(env!("CARGO_BIN_EXE_chatarium-inventory-diff"))
        .args([
            flight_before.to_str().unwrap(),
            flight_after.to_str().unwrap(),
            flight_diff.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(flight_diff_run.status.success());
    let flight_diff_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&flight_diff).unwrap()).unwrap();
    assert_eq!(
        flight_diff_value["format"],
        "chatarium-flight-inventory-diff"
    );
    assert_eq!(flight_diff_value["summary"]["added_paths"], 1);

    let asset_before = dir.join("asset-before.json");
    let asset_after = dir.join("asset-after.json");
    let asset_diff = dir.join("asset-diff.json");
    let base_assets = serde_json::json!({
        "format": "chatarium-frontend-asset-manifest",
        "version": 1,
        "asset_count": 1,
        "hashed_asset_count": 1,
        "warning_count": 0,
        "assets": [{
            "kind": "script",
            "host": "cdn.example.test",
            "path": "/assets/app.js",
            "status": 200,
            "mime_type": "application/javascript",
            "content_encoding": null,
            "body_available": true,
            "decoded_body_bytes": 10,
            "body_sha256": "aaaaaaaa",
            "body_warning": null
        }]
    });
    let mut changed_assets = base_assets.clone();
    changed_assets["assets"][0]["body_sha256"] = serde_json::json!("bbbbbbbb");
    fs::write(
        &asset_before,
        serde_json::to_vec_pretty(&base_assets).unwrap(),
    )
    .unwrap();
    fs::write(
        &asset_after,
        serde_json::to_vec_pretty(&changed_assets).unwrap(),
    )
    .unwrap();

    let asset_diff_run = Command::new(env!("CARGO_BIN_EXE_chatarium-inventory-diff"))
        .args([
            asset_before.to_str().unwrap(),
            asset_after.to_str().unwrap(),
            asset_diff.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(asset_diff_run.status.success());
    let asset_diff_value: serde_json::Value =
        serde_json::from_slice(&fs::read(&asset_diff).unwrap()).unwrap();
    assert_eq!(
        asset_diff_value["format"],
        "chatarium-frontend-asset-manifest-diff"
    );
    assert_eq!(asset_diff_value["summary"]["changed_paths"], 1);

    let _ = fs::remove_dir_all(dir);
}
