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
    let _ = fs::remove_dir_all(dir);
}
