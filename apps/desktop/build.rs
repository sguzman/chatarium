//! Embed a local source revision so stale executable builds are recognizable
//! directly in the Chatarium sidebar. Builds from source archives work too.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-env-changed=CHATARIUM_BUILD_REVISION");
    let revision = std::env::var("CHATARIUM_BUILD_REVISION")
        .ok()
        .filter(|value| !value.is_empty() && value.len() <= 40 && value.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .or_else(|| {
            let output = Command::new("git")
                .args(["rev-parse", "--short=8", "HEAD"])
                .output()
                .ok()?;
            output.status.success().then(|| {
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            })
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=CHATARIUM_BUILD_REVISION={revision}");
}
