//! Strict Linux-only, one-shot MCP stdio runner.
//!
//! This module is intentionally NOT wired to the desktop. A call can reach
//! it only with a move-only, durably consumed RouteDispatched reservation.
//! It launches through root-owned prlimit + bubblewrap, never directly or via
//! a shell. Missing confinement tools fail closed: no unsafe fallback.

use chatarium_core::tool::StdioToolProviderConfig;
use chatarium_protocol::mcp_wire::{MAX_MCP_FRAME_BYTES, McpResponse, decode_stdio_response};
use chatarium_store::tool_outcome_audit::{
    MAX_OUTCOME_BYTES, ToolCallOutcomeKind, append_tool_call_outcome_checked,
};
use chatarium_store::tool_stdio_dispatch::ReservedStdioToolDispatch;
use chatarium_store::tool_stdio_executable_inspection::inspect_stdio_executable;
use chatarium_store::{EventEnvelope, EventStore};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PRLIMIT: &str = "/usr/bin/prlimit";
const BWRAP: &str = "/usr/bin/bwrap";
const MAX_STDERR_BYTES: u64 = 16 * 1024;
const WALL_TIMEOUT: Duration = Duration::from_secs(10);
const LIMIT_AS_BYTES: &str = "--as=536870912";
const LIMIT_CPU_SECONDS: &str = "--cpu=8";
const LIMIT_FSIZE_BYTES: &str = "--fsize=8388608";
const LIMIT_OPEN_FILES: &str = "--nofile=64";
const LIMIT_PROCESSES: &str = "--nproc=256";
const LIMIT_CORE_DUMPS: &str = "--core=0";
const SCRATCH_TMPFS_BYTES: &str = "33554432";

/// Restricted launcher plan. All arguments are distinct argv entries: no
/// interpretation of quotes, pipes, expansions, or shell command strings.
#[derive(Debug, PartialEq, Eq)]
pub struct ConfinementPlan {
    program: &'static str,
    args: Vec<String>,
}

impl ConfinementPlan {
    /// Returns the absolute root-owned resource-limit wrapper executable.
    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }

    /// Returns the exact launcher argument vector.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Prepare a fixed, isolated Linux execution plan for a single provider.
///
/// This initial policy only admits root-owned programs within /usr/bin.
/// The sandbox exposes read-only /usr, a synthetic /dev and /proc, and
/// disposable /tmp. The entire network namespace is private and does not
/// inherit host /home, user dotfiles, sockets, credentials, or environment.
pub fn plan_confined_stdio_launch(
    executable: &str,
    argv: &[String],
) -> Result<ConfinementPlan, String> {
    if !cfg!(target_os = "linux") {
        return Err("external MCP stdio execution is Linux-only".to_owned());
    }
    if !executable.starts_with("/usr/bin/")
        || executable.len() <= "/usr/bin/".len()
        || executable.contains('\0')
        || executable.contains("//")
        || executable
            .split('/')
            .any(|part| part == "." || part == "..")
    {
        return Err("this restricted sandbox only runs canonical /usr/bin executables".to_owned());
    }
    if argv.len() > StdioToolProviderConfig::MAX_ARGUMENTS
        || argv.iter().any(|s| {
            s.len() > StdioToolProviderConfig::MAX_ARGUMENT_BYTES || s.chars().any(char::is_control)
        })
    {
        return Err("external MCP argv exceeds immutable configuration bounds".to_owned());
    }
    // No mount of the host home, filesystem root, /etc, or /run. The target
    // binary remains inside the read-only /usr tree. Symlink paths in its
    // executable prefix are independently rejected by metadata inspection.
    let mut args = vec![
        LIMIT_AS_BYTES.to_owned(),
        LIMIT_CPU_SECONDS.to_owned(),
        LIMIT_FSIZE_BYTES.to_owned(),
        LIMIT_OPEN_FILES.to_owned(),
        LIMIT_PROCESSES.to_owned(),
        LIMIT_CORE_DUMPS.to_owned(),
        "--".to_owned(),
        BWRAP.to_owned(),
        "--unshare-all".to_owned(),
        "--disable-userns".to_owned(),
        "--die-with-parent".to_owned(),
        "--new-session".to_owned(),
        "--clearenv".to_owned(),
        "--ro-bind".to_owned(),
        "/usr".to_owned(),
        "/usr".to_owned(),
        "--symlink".to_owned(),
        "usr/bin".to_owned(),
        "/bin".to_owned(),
        "--symlink".to_owned(),
        "usr/lib".to_owned(),
        "/lib".to_owned(),
        "--symlink".to_owned(),
        "usr/lib64".to_owned(),
        "/lib64".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
        "--dev".to_owned(),
        "/dev".to_owned(),
        "--size".to_owned(),
        SCRATCH_TMPFS_BYTES.to_owned(),
        "--tmpfs".to_owned(),
        "/tmp".to_owned(),
        "--setenv".to_owned(),
        "HOME".to_owned(),
        "/nonexistent".to_owned(),
        "--setenv".to_owned(),
        "PATH".to_owned(),
        "/usr/bin".to_owned(),
        "--chdir".to_owned(),
        "/".to_owned(),
        "--".to_owned(),
        executable.to_owned(),
    ];
    args.extend(argv.iter().cloned());
    Ok(ConfinementPlan {
        program: PRLIMIT,
        args,
    })
}

/// Check each launched program's on-disk metadata immediately before spawn.
/// This cannot attest authenticity in the presence of root compromise, and
/// it does not replace the separate *durable* activation and route checks.
#[cfg(target_os = "linux")]
fn validate_trusted_binary(path: &str) -> Result<(), String> {
    use chatarium_core::tool::ToolOperationName;
    use std::os::unix::fs::MetadataExt;

    let conf = StdioToolProviderConfig::new(
        path,
        Vec::new(),
        vec![ToolOperationName::new("launch").map_err(|e| format!("{e:?}"))?],
    )
    .map_err(|e| format!("{e:?}"))?;
    inspect_stdio_executable(&conf)
        .map_err(|e| format!("executable {path} failed metadata inspection: {e:?}"))?;
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat executable {path}: {e}"))?;
    if md.uid() != 0 {
        return Err(format!(
            "restricted runner refuses non-root-owned executable {path}"
        ));
    }
    Ok(())
}

/// Consume an already-reserved dispatch exactly once and record a checked
/// terminal observation only when the child has actually terminated.
///
/// This intentionally operates on the persistence worker: the journal must
/// not change between reservation and launch. When the process or journal
/// fails unexpectedly, the route remains dispatched without a synthesized
/// result and must never be automatically retried.
pub fn execute_reserved_stdio_tool_call(
    store: &mut impl EventStore,
    reservation: ReservedStdioToolDispatch,
) -> Result<EventEnvelope, String> {
    let invocation = &reservation.invocation().invocation;
    if store.events().last().map(|e| e.sequence) != Some(reservation.dispatch_sequence()) {
        return Err("external call reservation is not the current journal tip".to_owned());
    }
    let call_id = invocation.call_id;
    let route_id = invocation.route_id;
    let kind_and_text = run_one_shot(
        invocation.executable.as_str(),
        &invocation.argv,
        invocation.request_frame.as_str(),
        call_id.get(),
    );
    // Adapter errors are terminal *observations*, not a reason to rerun the
    // route. If the journal write fails the dispatch remains unresolved.
    let (kind, text) = match kind_and_text {
        Ok(frame) => (ToolCallOutcomeKind::Result, frame),
        Err(error) => (ToolCallOutcomeKind::Error, error),
    };
    append_tool_call_outcome_checked(store, call_id, route_id, kind, text)
}

fn run_one_shot(
    executable: &str,
    argv: &[String],
    frame: &str,
    request_id: u64,
) -> Result<String, String> {
    if !frame.ends_with('\n') || frame.len() > MAX_MCP_FRAME_BYTES {
        return Err("refused an unbounded or malformed MCP request frame".to_owned());
    }
    let plan = plan_confined_stdio_launch(executable, argv)?;
    #[cfg(target_os = "linux")]
    {
        for path in [PRLIMIT, BWRAP, executable] {
            validate_trusted_binary(path)?;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        return Err("external MCP stdio execution is Linux-only".to_owned());
    }

    #[cfg(target_os = "linux")]
    {
        let mut child = Command::new(plan.program)
            .args(&plan.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sandbox spawn failed: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "sandbox stdin pipe unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "sandbox stdout pipe unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "sandbox stderr pipe unavailable".to_owned())?;

        let request = frame.as_bytes().to_vec();
        let writer = thread::spawn(move || {
            let mut stdin = stdin;
            stdin.write_all(&request).map_err(|e| e.to_string())
        });
        let out_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            stdout
                .take(MAX_MCP_FRAME_BYTES as u64 + 1)
                .read_to_end(&mut buf)
                .map(|_| buf)
                .map_err(|e| e.to_string())
        });
        let err_reader = thread::spawn(move || {
            let mut buf = Vec::new();
            stderr
                .take(MAX_STDERR_BYTES + 1)
                .read_to_end(&mut buf)
                .map(|_| buf)
                .map_err(|e| e.to_string())
        });

        let start = Instant::now();
        let mut timeout = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if start.elapsed() < WALL_TIMEOUT => {
                    thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => {
                    timeout = true;
                    let _ = child.kill();
                    break child.wait().map_err(|e| e.to_string());
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(e.to_string());
                }
            }
        };
        let wrote = writer
            .join()
            .map_err(|_| "sandbox stdin writer panicked".to_owned())?;
        let output = out_reader
            .join()
            .map_err(|_| "sandbox stdout reader panicked".to_owned())??;
        let stderr = err_reader
            .join()
            .map_err(|_| "sandbox stderr reader panicked".to_owned())??;
        if timeout {
            return Err("sandbox MCP call exceeded its 10-second wall-time limit".to_owned());
        }
        let status = status.map_err(|e| format!("sandbox wait failed: {e}"))?;
        wrote.map_err(|e| format!("sandbox request write failed: {e}"))?;
        if output.len() > MAX_MCP_FRAME_BYTES || stderr.len() > MAX_STDERR_BYTES as usize {
            return Err("sandbox MCP stdout/stderr exceeded byte budget".to_owned());
        }
        if !status.success() {
            let diagnostic = String::from_utf8_lossy(&stderr);
            return Err(format!(
                "isolated MCP process exited unsuccessfully: {status}; bounded stderr: {diagnostic}"
            ));
        }
        let text = std::str::from_utf8(&output)
            .map_err(|_| "sandbox MCP stdout was not UTF-8".to_owned())?;
        let response = decode_stdio_response(text, request_id)
            .map_err(|e| format!("sandbox MCP response rejected: {e:?}"))?;
        match response {
            McpResponse::Complete(_) => {
                if text.len() > MAX_OUTCOME_BYTES {
                    return Err("sandbox MCP response exceeds durable outcome budget".to_owned());
                }
                Ok(text.to_owned())
            }
            McpResponse::Error { code, message, .. } => {
                Err(format!("sandbox MCP protocol error {code}: {message}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_remains_exact_and_separate_from_sandbox_options() {
        #[cfg(target_os = "linux")]
        {
            let plan = plan_confined_stdio_launch(
                "/usr/bin/example-mcp",
                &["with spaces".to_owned(), "--flag=;\\$HOME".to_owned()],
            )
            .unwrap();
            assert_eq!(plan.program(), PRLIMIT);
            let tool_index = plan
                .args()
                .iter()
                .position(|arg| arg == "/usr/bin/example-mcp")
                .unwrap();
            assert_eq!(&plan.args()[tool_index - 1], "--");
            assert_eq!(
                &plan.args()[tool_index + 1..],
                &["with spaces", "--flag=;\\$HOME"],
            );
            assert!(plan.args().contains(&"--unshare-all".to_owned()));
            assert!(plan.args().contains(&"--disable-userns".to_owned()));
            assert!(plan.args().contains(&LIMIT_PROCESSES.to_owned()));
            assert!(plan.args().contains(&LIMIT_CORE_DUMPS.to_owned()));
            let scratch = plan.args().iter().position(|arg| arg == "--tmpfs").unwrap();
            assert_eq!(plan.args()[scratch - 2], "--size");
            assert_eq!(plan.args()[scratch - 1], SCRATCH_TMPFS_BYTES);
            assert!(plan.args().contains(&"--clearenv".to_owned()));
            assert!(plan.args().contains(&"--ro-bind".to_owned()));
            assert!(!plan.args().contains(&"/home".to_owned()));
            assert!(!plan.args().contains(&"--share-net".to_owned()));
        }
    }

    #[test]
    fn untrusted_program_paths_fail_before_any_process_is_spawned() {
        assert!(plan_confined_stdio_launch("/home/user/bin/tool", &[]).is_err());
        assert!(plan_confined_stdio_launch("/usr/bin/../lib/tool", &[]).is_err());
        assert!(plan_confined_stdio_launch("/usr/bin//tool", &[]).is_err());
        assert!(plan_confined_stdio_launch("relative", &[]).is_err());
    }

    #[test]
    fn infinite_argv_and_control_characters_are_rejected() {
        assert!(
            plan_confined_stdio_launch(
                "/usr/bin/tool",
                &vec!["ok".to_owned(); StdioToolProviderConfig::MAX_ARGUMENTS + 1],
            )
            .is_err()
        );
        assert!(plan_confined_stdio_launch("/usr/bin/tool", &["bad\nvalue".to_owned()],).is_err());
    }

    #[test]
    fn request_frame_must_be_bounded_even_without_sandbox_installed() {
        assert!(run_one_shot("/usr/bin/true", &[], "{}", 1).is_err());
        assert!(
            run_one_shot(
                "/usr/bin/true",
                &[],
                &"x".repeat(MAX_MCP_FRAME_BYTES + 1),
                1
            )
            .is_err()
        );
    }

    /// This runs in the Linux CI sandbox job with explicit opt-in. It uses
    /// stock root-owned sed as a deterministic stdio MCP fixture; the stdin
    /// writer must finish before sed emits one correlated terminal response.
    #[cfg(target_os = "linux")]
    #[test]
    fn live_bubblewrap_stdio_smoke() {
        if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
            return;
        }
        use chatarium_protocol::mcp_wire::{encode_stdio_frame, tools_call_request};
        let request = tools_call_request(71, "fixture.echo", &serde_json::json!({})).unwrap();
        let frame = encode_stdio_frame(&request).unwrap();
        let reply = r#"{"jsonrpc":"2.0","id":71,"result":{"resultType":"complete","content":[{"type":"text","text":"sandbox-ok"}]}}"#;
        let command = format!("s/.*/{reply}/p");
        let returned = run_one_shot(
            "/usr/bin/sed",
            &["-n".to_owned(), "-e".to_owned(), command],
            &frame,
            71,
        )
        .unwrap();
        assert_eq!(returned, format!("{reply}\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_confinement_binary_never_triggers_unsafe_direct_launch() {
        let candidate = "/usr/bin/chatarium-nonexistent-test-executable-981293";
        assert!(validate_trusted_binary(candidate).is_err());
    }
}
