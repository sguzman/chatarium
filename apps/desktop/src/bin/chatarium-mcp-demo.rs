//! Three intentionally small MCP 2026-07-28 tools for validating Chatarium's
//! existing user-approved, one-shot Linux stdio dispatch path.
//!
//! This is a *provider*, not an in-process Chatarium permission bypass.
//! The shell demonstration is a fixed command allowlist, not a host shell.
//! Install only into a trusted, root-owned /usr/bin path when explicitly
//! enabling it through Chatarium's external MCP provider controls.

use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REQUEST: u64 = 1_048_576;
const MAX_SHELL_OUTPUT: usize = 4096;

fn catalog() -> Value {
    json!({
        "resultType": "complete",
        "tools": [
            {
                "name": "hello",
                "description": "Return a deterministic greeting (no side effects).",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            },
            {
                "name": "time.now",
                "description": "Read the current UTC wall-clock time and Unix epoch milliseconds.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            },
            {
                "name": "shell.command",
                "description": "Demonstrate a sandboxed subprocess. Only echo, pwd, id, date, and uname are available; no shell, pipes, expansion, files or network.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "enum": ["echo", "pwd", "id", "date", "uname"]},
                        "args": {"type": "array", "items": {"type": "string"}, "maxItems": 8}
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

fn utc_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60
    )
}

fn empty_arguments(args: &Value) -> Result<(), &'static str> {
    if args.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err("this tool requires exactly {}")
    }
}

fn shell_command(args: &Value) -> Result<Value, &'static str> {
    let obj = args.as_object().ok_or("arguments must be a JSON object")?;
    if obj.keys().any(|key| key != "command" && key != "args") {
        return Err("unknown shell.command argument");
    }
    let name = obj
        .get("command")
        .and_then(Value::as_str)
        .ok_or("command must be a string")?;
    let argv = match obj.get("args") {
        None => Vec::new(),
        Some(Value::Array(values)) if values.len() <= 8 => values
            .iter()
            .map(|v| v.as_str().ok_or("all arguments must be strings"))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("args must be an array with at most 8 strings"),
    };
    if argv
        .iter()
        .any(|v| v.len() > 256 || v.chars().any(char::is_control))
    {
        return Err("argument exceeds 256 bytes or contains control characters");
    }
    match name {
        "echo" => {
            if argv.iter().any(|v| v.starts_with('-')) {
                return Err("echo flags are not supported");
            }
        }
        "uname" if argv.is_empty() || argv == ["-s"] || argv == ["-m"] => {}
        "pwd" | "id" | "date" if argv.is_empty() => {}
        _ => return Err("command or flags outside the fixed demonstration allowlist"),
    }
    // Deliberately no /bin/sh, bash, PATH lookup, pipes or interpolation.
    // Chatarium's outer runner supplies namespaces, no-network isolation and
    // resource limits. The command is inert until the user separately allows
    // and runs its one-shot route.
    let program = format!("/usr/bin/{name}");
    let output = std::process::Command::new(program)
        .args(&argv)
        .env_clear()
        .output()
        .map_err(|_| "allowed demo command could not start")?;
    if output.stdout.len() > MAX_SHELL_OUTPUT || output.stderr.len() > MAX_SHELL_OUTPUT {
        return Err("demo command output exceeds 4096 bytes");
    }
    Ok(json!({
        "command": name,
        "args": argv,
        "exit_code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    }))
}

fn tool_result(payload: Value, is_error: bool) -> Value {
    json!({
        "resultType": "complete",
        "isError": is_error,
        "content": [{"type": "text", "text": payload.to_string()}],
        "structuredContent": payload
    })
}

fn tool_call(name: &str, args: &Value) -> Value {
    let result = match name {
        "hello" => empty_arguments(args).map(|()| json!({"message": "hello"})),
        "time.now" => empty_arguments(args).map(|()| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            json!({
                "utc": utc_timestamp(now.as_secs() as i64),
                "unix_epoch_ms": now.as_millis().min(u128::from(u64::MAX)) as u64
            })
        }),
        "shell.command" => shell_command(args),
        _ => Err("unknown tool"),
    };
    match result {
        Ok(value) => tool_result(value, false),
        Err(reason) => tool_result(json!({"error": reason}), true),
    }
}

fn handle(request: &Value) -> Value {
    let id = request.get("id").and_then(Value::as_u64);
    let method = request.get("method").and_then(Value::as_str);
    let params = request.get("params").and_then(Value::as_object);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || id.is_none()
        || params.is_none()
    {
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32600, "message": "invalid one-shot MCP request"}
        });
    }
    let id = id.expect("checked above");
    let params = params.expect("checked above");
    let result = match method {
        Some("tools/list") => catalog(),
        Some("tools/call") => {
            let name = params.get("name").and_then(Value::as_str);
            let args = params.get("arguments");
            match (name, args) {
                (Some(name), Some(args)) if args.is_object() => tool_call(name, args),
                _ => {
                    return json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32602, "message": "tools/call requires name and object arguments"}
                    });
                }
            }
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "only tools/list and tools/call are supported"}
            });
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut data = Vec::new();
    io::stdin().take(MAX_REQUEST + 1).read_to_end(&mut data)?;
    if data.is_empty()
        || data.len() > MAX_REQUEST as usize
        || data.last() != Some(&b'\n')
        || data[..data.len() - 1].contains(&b'\n')
    {
        eprintln!("expected one bounded newline-delimited MCP JSON request");
        std::process::exit(2);
    }
    let request: Value = serde_json::from_slice(&data[..data.len() - 1])?;
    let response = handle(&request);
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Value) -> Value {
        handle(&json!({
            "jsonrpc": "2.0", "id": 71, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        }))
    }

    #[test]
    fn catalog_exposes_exactly_three_described_tools() {
        let reply = handle(&json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {}
        }));
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["resultType"], "complete");
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["name"], "hello");
        assert_eq!(tools[1]["name"], "time.now");
        assert_eq!(tools[2]["name"], "shell.command");
    }

    #[test]
    fn hello_is_deterministic_and_validates_arguments() {
        let reply = call("hello", json!({}));
        assert_eq!(reply["result"]["structuredContent"]["message"], "hello");
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(call("hello", json!({"extra": 1}))["result"]["isError"], true);
    }

    #[test]
    fn utc_conversion_handles_epoch_and_leap_day() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_704_067_200), "2024-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn clock_result_is_utc_and_nonempty() {
        let reply = call("time.now", json!({}));
        assert_eq!(reply["result"]["isError"], false);
        assert!(reply["result"]["structuredContent"]["utc"].as_str().unwrap().ends_with('Z'));
        assert!(reply["result"]["structuredContent"]["unix_epoch_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn shell_refuses_arbitrary_execution_and_expansion() {
        for args in [
            json!({"command": "sh", "args": ["-c", "echo secret"]}),
            json!({"command": "cat", "args": ["/etc/passwd"]}),
            json!({"command": "uname", "args": ["-a"]}),
            json!({"command": "date", "args": ["--file=/home/private"]}),
            json!({"command": "echo", "args": ["-e", "payload"]}),
            json!({"command": "echo", "args": ["one\n two"]}),
            json!({"command": "pwd", "args": ["extra"]}),
            json!({"command": "id", "override": "yes"}),
        ] {
            assert_eq!(call("shell.command", args)["result"]["isError"], true);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn safe_shell_echo_command_executes_without_a_shell() {
        let reply = call("shell.command", json!({"command": "echo", "args": ["hello", "mcp"]}));
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["structuredContent"]["stdout"], "hello mcp\n");
    }

    #[test]
    fn call_and_catalog_keep_correlated_numeric_ids() {
        assert_eq!(call("hello", json!({}))["id"], 71);
        let rejected = handle(&json!({
            "jsonrpc": "2.0", "id": 72, "method": "initialize", "params": {}
        }));
        assert_eq!(rejected["id"], 72);
        assert_eq!(rejected["error"]["code"], -32601);
    }
}
