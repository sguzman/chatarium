//! Embed a local source revision so stale executable builds are recognizable
//! directly in the Chatarium sidebar. Builds from source archives work too.
use std::process::Command;

fn main() {
    // HEAD usually contains only a branch name. Watching HEAD alone does
    // not notice a fast-forward pull, so a rebuilt binary can falsely claim
    // to be an older revision. Track the real branch ref as well.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    if let Ok(symbolic) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
    {
        if symbolic.status.success() {
            let branch_ref = String::from_utf8_lossy(&symbolic.stdout);
            let branch_ref = branch_ref.trim();
            if !branch_ref.is_empty() && !branch_ref.contains("..") {
                if let Ok(path) = Command::new("git")
                    .args(["rev-parse", "--git-path", branch_ref])
                    .output()
                {
                    if path.status.success() {
                        let ref_path = String::from_utf8_lossy(&path.stdout);
                        println!("cargo:rerun-if-changed={}", ref_path.trim());
                    }
                }
            }
        }
    }
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-env-changed=CHATARIUM_BUILD_REVISION");
    let revision = std::env::var("CHATARIUM_BUILD_REVISION")
        .ok()
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 40
                && value.chars().all(|ch| ch.is_ascii_alphanumeric())
        })
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "--short=8", "HEAD"])
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=CHATARIUM_BUILD_REVISION={revision}");
}
