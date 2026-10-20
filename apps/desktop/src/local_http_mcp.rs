//! Three explicit user-owned loopback MCP endpoints, never an arbitrary URL proxy.
//! The persistent Node transport keeps Mathematica MCP sessions per conversation.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Mutex, OnceLock},
};

pub const MANIFOLDR: &str = "chatarium.local.manifoldr";
pub const MATHEMATICA: &str = "chatarium.local.mathematica";
pub const OPENVIKING: &str = "chatarium.local.openviking";
const OPENVIKING_TOOLS: &[&str] = &["find", "search", "read", "write"];

const MANIFOLDR_TOOLS: &[&str] = &[
    "popular_markets",
    "trending_markets",
    "high_volume_markets",
    "search_markets",
    "suggested_markets",
    "list_categories",
    "get_category",
    "get_market",
    "get_probabilities",
];
const MATHEMATICA_TOOLS: &[&str] = &[
    "mathematica_create_session",
    "mathematica_execute_code",
    "mathematica_close_session",
    "mathematica_list_sessions",
    "mathematica_get_finance",
    "mathematica_time",
];

pub fn known_provider(name: &str) -> bool {
    matches!(name, MANIFOLDR | MATHEMATICA | OPENVIKING)
}

pub fn allowed_operation(provider: &str, operation: &str) -> bool {
    let allowed: &[&str] = match provider {
        MANIFOLDR => MANIFOLDR_TOOLS,
        MATHEMATICA => MATHEMATICA_TOOLS,
        OPENVIKING => OPENVIKING_TOOLS,
        _ => return false,
    };
    allowed.contains(&operation)
}

/// Model proposals may read scoped memory but can never bypass the host-owned
/// durable checkpoint outbox by calling OpenViking write directly.
pub fn allowed_model_operation(provider: &str, operation: &str) -> bool {
    allowed_operation(provider, operation) && !(provider == OPENVIKING && operation == "write")
}

pub fn registered_tools() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    MANIFOLDR_TOOLS
        .iter()
        .map(|&operation| (MANIFOLDR, operation, "Manifoldr read-only market data"))
        .chain(MATHEMATICA_TOOLS.iter().map(|&operation| {
            (
                MATHEMATICA,
                operation,
                "Wolfram Mathematica; executes arbitrary Wolfram code with local user privileges",
            )
        }))
        .chain(OPENVIKING_TOOLS.iter().map(|&operation| {
            (
                OPENVIKING,
                operation,
                "Scoped OpenViking memory; write creates new checkpoints only",
            )
        }))
}

// The native executable must still find its fixed-loopback MCP helpers
// after it is moved away from its build checkout. Embed the small audited
// scripts and materialize them in a private, process-scoped temporary folder
// only when the developer checkout no longer exists.
const MCP_CLIENT: &str = include_str!("../../../tools/local-http-mcp/client.mjs");
const MCP_WORKER: &str = include_str!("../../../tools/local-http-mcp/worker.mjs");
const MCP_PROBE: &str = include_str!("../../../tools/local-http-mcp/openviking-live-probe.mjs");

pub(crate) struct HelperScript {
    path: PathBuf,
    temporary_directory: Option<PathBuf>,
}

impl HelperScript {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn bundled(script_name: &str) -> Result<Self, String> {
        if !matches!(script_name, "worker.mjs" | "openviking-live-probe.mjs") {
            return Err("unsupported embedded MCP helper".to_owned());
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| format!("MCP helper clock: {e}"))?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "chatarium-mcp-helper-{}-{nonce}-{ordinal}",
            std::process::id(),
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&directory)
            .map_err(|e| format!("private MCP helper directory: {e}"))?;
        let bundle = Self {
            path: directory.join(script_name),
            temporary_directory: Some(directory.clone()),
        };
        for (name, contents) in [
            ("client.mjs", MCP_CLIENT),
            ("worker.mjs", MCP_WORKER),
            ("openviking-live-probe.mjs", MCP_PROBE),
        ] {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(directory.join(name))
                .map_err(|e| format!("private MCP helper creation: {e}"))?;
            file.write_all(contents.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("private MCP helper durable content: {e}"))?;
        }
        Ok(bundle)
    }
}

impl Drop for HelperScript {
    fn drop(&mut self) {
        if let Some(directory) = &self.temporary_directory {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

pub(crate) fn helper_script(script_name: &str) -> Result<HelperScript, String> {
    if !matches!(script_name, "worker.mjs" | "openviking-live-probe.mjs") {
        return Err("unsupported MCP helper script".to_owned());
    }
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/local-http-mcp")
        .join(script_name);
    if checkout.is_file() {
        return Ok(HelperScript {
            path: checkout,
            temporary_directory: None,
        });
    }
    HelperScript::bundled(script_name)
}

struct Worker {
    _child: Child,
    _script: HelperScript,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Stop the worker before removing its private helper files.
        let _ = self._child.kill();
        let _ = self._child.wait();
    }
}

impl Worker {
    fn new() -> Result<Self, String> {
        let script = helper_script("worker.mjs")?;
        let mut child = Command::new("node")
            .arg(script.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start local MCP transport: {error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or("MCP transport stdin unavailable")?;
        let output = child
            .stdout
            .take()
            .ok_or("MCP transport stdout unavailable")?;
        Ok(Self {
            _child: child,
            _script: script,
            input,
            output: BufReader::new(output),
        })
    }

    fn invoke(
        &mut self,
        conversation: &str,
        call_key: &str,
        provider: &str,
        operation: &str,
        arguments: &Value,
    ) -> Result<Value, String> {
        if !allowed_operation(provider, operation) {
            return Err("MCP operation is not in the exact fixed-provider allowlist".to_owned());
        }
        if !arguments.is_object() {
            return Err("local MCP arguments must be a JSON object".to_owned());
        }
        let provider_key = match provider {
            MANIFOLDR => "manifoldr",
            MATHEMATICA => "mathematica",
            OPENVIKING => "openviking",
            _ => return Err("unknown local MCP provider".to_owned()),
        };
        let request = json!({
            "id": call_key,
            "conversation_id": conversation,
            "provider": provider_key,
            "method": "call",
            "name": operation,
            "arguments": arguments,
        });
        let data = request.to_string();
        if data.len() > 256 * 1024 {
            return Err("local MCP request exceeds 256 KiB".to_owned());
        }
        self.input
            .write_all(data.as_bytes())
            .map_err(|e| format!("MCP worker stdin: {e}"))?;
        self.input
            .write_all(b"\n")
            .map_err(|e| format!("MCP worker stdin: {e}"))?;
        self.input
            .flush()
            .map_err(|e| format!("MCP worker flush: {e}"))?;
        let mut response = String::new();
        let count = self
            .output
            .read_line(&mut response)
            .map_err(|e| format!("MCP transport read: {e}"))?;
        if count == 0 {
            return Err(
                "local MCP transport exited; request outcome unknown; not retried".to_owned(),
            );
        }
        if count > 4 * 1024 * 1024 {
            return Err("local MCP transport response too large".to_owned());
        }
        let value: Value = serde_json::from_str(&response)
            .map_err(|e| format!("invalid local MCP transport response: {e}"))?;
        if value["id"].as_str() != Some(call_key) {
            return Err("local MCP transport response correlation failed; not retried".to_owned());
        }
        if value["ok"] != true {
            return Err(value["error"]
                .as_str()
                .unwrap_or("MCP transport failed")
                .to_owned());
        }
        let result = value
            .get("result")
            .cloned()
            .ok_or("MCP response result missing")?;
        if result["isError"] == true {
            return Err(format!(
                "MCP tool error: {}",
                result.to_string().chars().take(1500).collect::<String>()
            ));
        }
        Ok(result)
    }
}

// The mutex serializes the JSONL channel, not the GUI or journal workers.
// Every invocation is executed on a dedicated background thread. Node keeps
// transport sessions keyed by provider plus owning local conversation.
static BRIDGE: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();

/// The general MCP dispatcher is model-accessible after its standing policy
/// or user approval. Neither grants permission for a raw OpenViking write.
pub fn invoke(
    conversation: &str,
    call_key: &str,
    provider: &str,
    operation: &str,
    arguments: &Value,
) -> Result<Value, String> {
    if provider == OPENVIKING && operation == "write" {
        return Err(
            "OpenViking checkpoint writes require the host-owned durable outbox".to_owned(),
        );
    }
    invoke_transport(conversation, call_key, provider, operation, arguments)
}

/// Only the background host checkpoint dispatcher calls this path, after a
/// live collision-safety preflight and the durable Dispatched transition.
/// Never expose it as an ordinary model-selected tool.
pub fn invoke_checkpoint_write(
    conversation: &str,
    turn: &str,
    content: &str,
) -> Result<Value, String> {
    let uri = crate::openviking_memory_delivery::checkpoint_uri(conversation, turn)?;
    let required_header = format!(
        "# Chatarium memory checkpoint\n\nConversation: {conversation}\nTurn: {turn}\nSource journal sequence: "
    );
    if content.is_empty() || content.len() > 16384 || !content.starts_with(&required_header) {
        return Err("OpenViking host checkpoint content identity is invalid".to_owned());
    }
    invoke_transport(
        conversation,
        &format!("memory-create:{conversation}:{turn}"),
        OPENVIKING,
        "write",
        &json!({
            "uri": uri,
            "content": content,
            "mode": "create",
            "wait": false,
        }),
    )
}

fn invoke_transport(
    conversation: &str,
    call_key: &str,
    provider: &str,
    operation: &str,
    arguments: &Value,
) -> Result<Value, String> {
    if !allowed_operation(provider, operation) {
        return Err("unsupported localhost MCP operation".to_owned());
    }
    if !arguments.is_object() {
        return Err("MCP tool arguments must be a JSON object".to_owned());
    }
    // Initialize only for an actually approved, durably dispatched call.
    let bridge = BRIDGE.get_or_init(|| Mutex::new(None));
    let mut state = bridge
        .lock()
        .map_err(|_| "local MCP transport lock poisoned")?;
    if state.is_none() {
        *state = Some(Worker::new()?);
    }
    let result = state
        .as_mut()
        .ok_or("local MCP transport was not initialized")?
        .invoke(conversation, call_key, provider, operation, arguments);
    // A *future, separate* approved call can create a new transport after a
    // crashed worker. Never replay the uncertain call itself.
    if result.as_ref().err().is_some_and(|error| {
        error.contains("MCP worker stdin")
            || error.contains("MCP worker flush")
            || error.contains("MCP transport read")
            || error.contains("MCP transport exited")
            || error.contains("MCP transport response correlation")
            || error.contains("invalid local MCP transport response")
    }) {
        *state = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_mcp_helpers_survive_outside_checkout_and_clean_up() {
        use std::process::Command;
        assert!(helper_script("unexpected.mjs").is_err());
        let bundle = HelperScript::bundled("worker.mjs").unwrap();
        let parent = bundle.path().parent().unwrap().to_owned();
        assert_eq!(
            fs::read_to_string(parent.join("client.mjs")).unwrap(),
            MCP_CLIENT
        );
        assert_eq!(fs::read_to_string(bundle.path()).unwrap(), MCP_WORKER);
        assert_eq!(
            fs::read_to_string(parent.join("openviking-live-probe.mjs")).unwrap(),
            MCP_PROBE
        );
        for name in ["worker.mjs", "openviking-live-probe.mjs"] {
            let result = Command::new("node")
                .arg("--check")
                .arg(parent.join(name))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "embedded {name} failed JavaScript syntax check"
            );
        }
        // Unlike a syntax check, this executes the detached bridge and its
        // relative import of client.mjs, with no live MCP network request.
        let mut worker = Command::new("node")
            .arg(bundle.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        worker
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"id\":2}\n")
            .unwrap();
        let output = worker.wait_with_output().unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"], "Invalid MCP bridge request");
        // The relocated preflight must also resolve its client import, but
        // invalid CLI usage must stop before touching a real OpenViking server.
        let probe = Command::new("node")
            .arg(parent.join("openviking-live-probe.mjs"))
            .arg("--not-a-probe-mode")
            .output()
            .unwrap();
        assert_eq!(probe.status.code(), Some(2));
        assert!(String::from_utf8(probe.stderr).unwrap().contains("Usage:"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(bundle.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(bundle);
        assert!(!parent.exists());
    }

    #[test]
    fn both_servers_have_exact_explicit_allowlists() {
        assert!(allowed_operation(MANIFOLDR, "get_market"));
        assert!(allowed_operation(MATHEMATICA, "mathematica_create_session"));
        assert!(!allowed_operation(MANIFOLDR, "bet_place"));
        assert!(!allowed_operation(MATHEMATICA, "shell.command"));
        assert!(!known_provider("chatarium.local.user-controlled-host"));
        assert!(known_provider(OPENVIKING));
        for operation in ["find", "search", "read", "write"] {
            assert!(allowed_operation(OPENVIKING, operation));
        }
        for operation in ["forget", "remember", "add_resource", "edit", "set_acl"] {
            assert!(!allowed_operation(OPENVIKING, operation));
        }
        assert_eq!(registered_tools().count(), 19);
        assert!(allowed_model_operation(OPENVIKING, "read"));
        assert!(allowed_model_operation(OPENVIKING, "find"));
        assert!(!allowed_model_operation(OPENVIKING, "write"));
        assert!(allowed_operation(OPENVIKING, "write")); // host-only transport
        assert!(invoke("owner", "7", OPENVIKING, "write", &json!({})).is_err());
        assert!(invoke_checkpoint_write("owner", "turn-1", "forged content").is_err());
    }
}
