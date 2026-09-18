//! CLI regression tests for the capture harness's live-mutation safety boundary.

use std::process::Command;

#[test]
fn init_and_run_remain_unavailable_for_remote_mutation() {
    let binary = env!("CARGO_BIN_EXE_chatarium-capture");
    for args in [
        vec!["init"],
        vec!["run", "C00-idle-load"],
        vec!["run", "C03-send-text"],
    ] {
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
