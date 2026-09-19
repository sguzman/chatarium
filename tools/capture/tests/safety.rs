//! CLI regression tests for the capture harness's live-mutation safety boundary.

use std::fs;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn run_canonical_experiments_remain_unavailable() {
    let binary = env!("CARGO_BIN_EXE_chatarium-capture");
    for args in [vec!["run", "C00-idle-load"], vec!["run", "C03-send-text"]] {
        let output = Command::new(binary).args(&args).output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not implemented yet"),
            "unexpected output for {args:?}: {stderr}"
        );
        assert!(
            stderr.contains("no state was changed") || stderr.contains("no browser was launched"),
            "unexpected output for {args:?}: {stderr}"
        );
    }
}

#[test]
fn init_enters_bootstrap_and_reports_missing_edge_without_waiting_for_operator() {
    let binary = env!("CARGO_BIN_EXE_chatarium-capture");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("chatarium-init-cli-{stamp}"));
    let local = root.join("localappdata");
    let no_edge = root.join("no-edge");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&no_edge).unwrap();
    let output = Command::new(binary)
        .arg("init")
        .env("LOCALAPPDATA", &local)
        .env("PROGRAMFILES", &no_edge)
        .env("PROGRAMFILES(X86)", &no_edge)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("not implemented yet"), "{stderr}");
    assert!(stderr.contains("Microsoft Edge was not found"), "{stderr}");
    assert!(!stdout.contains("Sign in to ChatGPT"));
    let run_dirs = fs::read_dir(local.join("Chatarium/captures/diagnostics"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(run_dirs.len(), 1);
    let events = fs::read_to_string(run_dirs[0].join("events.jsonl")).unwrap();
    assert!(events.contains("init_started"));
    assert!(events.contains("init_finished"));
    assert!(!events.contains("browser_process_started"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn help_documents_the_read_only_edge_smoke_boundary() {
    let binary = env!("CARGO_BIN_EXE_chatarium-capture");
    let output = Command::new(binary).arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for required in [
        "chatarium-capture smoke-edge",
        "capture-browser\\edge-profile",
        "about:blank",
        "captures\\diagnostics\\<run-id>",
        "does not contact",
        "closes the launched browser",
        "init first opens ChatGPT in a normal Edge window",
        "no DevTools or remote-debugging transport",
        "that window to continue",
        "reopens the profile over the Windows anonymous pipe",
        "does not inspect credentials, cookies, storage",
    ] {
        assert!(
            stdout.contains(required),
            "help output omitted {required:?}"
        );
    }
    assert!(!stdout.contains("press Enter"));
}
