//! Fail-closed, one-shot compatibility attestation before a checkpoint is
//! durably marked Dispatched. This runs ONLY on a background thread.
use serde_json::Value;
use std::path::PathBuf;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Do not accept discovery-only or a successful synthetic create/read alone.
/// The probe must demonstrate that a second create was rejected and that the
/// original bytes were preserved.
pub(crate) fn attested_exclusive_create(value: &Value) -> bool {
    value.get("ok") == Some(&Value::Bool(true))
        && value.get("mode").and_then(Value::as_str) == Some("synthetic-exclusive-create")
        && value.get("verified") == Some(&Value::Bool(true))
        && value.get("exclusiveCreate") == Some(&Value::Bool(true))
        && value.get("exclusiveCreateVerified") == Some(&Value::Bool(true))
        && value.get("remoteMutation") == Some(&Value::Bool(true))
}

/// One disposable, randomly named synthetic namespace per attempted checkpoint.
/// No user text or production URI is sent to the diagnostic. It may leave a
/// synthetic QA file behind; an ambiguous outcome never authorizes the write.
pub fn verify_before_checkpoint_write() -> Result<(), String> {
    let script: PathBuf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/local-http-mcp/openviking-live-probe.mjs");
    if !script.is_file() {
        return Err("OpenViking write preflight is missing; checkpoint remains Ready".to_owned());
    }
    let mut child = Command::new("node")
        .arg(script)
        .arg("--verify-exclusive-create")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "OpenViking write preflight could not start; checkpoint remains Ready".to_owned())?;
    // Bound the background preflight without locking the journal worker or
    // leaving an unbounded process when an MCP server stops responding.
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(90) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("OpenViking preflight timed out or could not finish; checkpoint remains Ready".to_owned());
            }
        }
    };
    if !status.success() {
        return Err("OpenViking exclusive-create compatibility was not proven; checkpoint remains Ready".to_owned());
    }
    // Never include raw stdout/stderr in logs or UI, even on errors: the
    // external service or transport might accidentally disclose secrets.
    let mut stdout = Vec::new();
    child.stdout.take().ok_or("OpenViking preflight evidence unavailable")?
        .take(4096)
        .read_to_end(&mut stdout)
        .map_err(|_| "OpenViking preflight evidence unreadable".to_owned())?;
    let value: Value = serde_json::from_slice(&stdout)
        .map_err(|_| "OpenViking preflight returned malformed evidence; checkpoint remains Ready".to_owned())?;
    if !attested_exclusive_create(&value) {
        return Err("OpenViking preflight did not prove safe exclusive creation; checkpoint remains Ready".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attestation_requires_actual_exclusive_create_proof() {
        let confirmed = json!({
            "ok": true,
            "mode": "synthetic-exclusive-create",
            "verified": true,
            "exclusiveCreate": true,
            "exclusiveCreateVerified": true,
            "remoteMutation": true
        });
        assert!(attested_exclusive_create(&confirmed));
        for (field, invalid) in [
            ("ok", json!(false)),
            ("mode", json!("discovery-only")),
            ("mode", json!("synthetic-create-once")),
            ("verified", json!(false)),
            ("exclusiveCreate", json!(false)),
            ("exclusiveCreateVerified", json!(false)),
            ("remoteMutation", json!(false)),
        ] {
            let mut changed = confirmed.clone();
            changed[field] = invalid;
            assert!(!attested_exclusive_create(&changed), "{field}");
        }
        assert!(!attested_exclusive_create(&json!({})));
        assert!(!attested_exclusive_create(&json!(null)));
    }
}
