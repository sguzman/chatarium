//! Three explicit user-owned loopback MCP endpoints, never an arbitrary URL proxy.
//! The persistent Node transport keeps Mathematica MCP sessions per conversation.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
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

struct Worker {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn new() -> Result<Self, String> {
        let script: PathBuf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/local-http-mcp/worker.mjs");
        if !script.is_file() {
            return Err(format!("local MCP transport missing: {}", script.display()));
        }
        let mut child = Command::new("node")
            .arg(script)
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

pub fn invoke(
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
    }
}
