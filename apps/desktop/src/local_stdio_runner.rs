//! Strict Linux-only, one-shot MCP stdio runner.
//!
//! The desktop dispatches a move-only, durably consumed RouteDispatched
//! reservation to a dedicated process thread. Journal writes remain on the
//! separate persistence worker.
//! It launches through root-owned prlimit + bubblewrap, never directly or via
//! a shell. Missing confinement tools fail closed: no unsafe fallback.

use chatarium_core::routing::RouteId;
use chatarium_core::tool::{StdioToolProviderConfig, ToolCallId};
use chatarium_protocol::mcp_wire::{
    MAX_MCP_FRAME_BYTES, McpResponse, decode_stdio_response, parse_tools_list_page,
    validate_tools_call_result,
};
use chatarium_store::tool_outcome_audit::{MAX_OUTCOME_BYTES, ToolCallOutcomeKind};
use chatarium_store::tool_stdio_dispatch::ReservedStdioToolDispatch;
use chatarium_store::tool_stdio_executable_inspection::inspect_stdio_executable;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PRLIMIT: &str = "/usr/bin/prlimit";
const BWRAP: &str = "/usr/bin/bwrap";
const MAX_STDERR_BYTES: u64 = 16 * 1024;
const WALL_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_HOST_PROBE_STDERR_BYTES: usize = 4 * 1024;
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
        "--unshare-user".to_owned(),
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

/// Fail-closed launch readiness check on the persistence worker, before
/// consuming a durable one-shot route. Never call this from the render loop.
/// The runner checks executable metadata again immediately before spawning.
pub fn preflight_confined_stdio_launch(executable: &str, argv: &[String]) -> Result<(), String> {
    let _plan = plan_confined_stdio_launch(executable, argv)?;
    #[cfg(target_os = "linux")]
    {
        for path in [PRLIMIT, BWRAP, executable] {
            validate_trusted_binary(path)?;
        }
    }
    Ok(())
}

/// Exercise the actual Linux namespace policy before an irreversible dispatch.
///
/// This is intentionally a fixed, non-provider smoke process: /usr/bin/true
/// receives no input, no caller-controlled argv or environment, and no access
/// to host home or network. The caller must run this on a separate thread,
/// never on the desktop render thread or journal worker. Success is only a
/// current-host capability observation, not a cached execution permit.
pub fn probe_confined_stdio_host() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        return Err("Linux MCP confinement is unsupported on this platform".to_owned());
    }
    #[cfg(target_os = "linux")]
    {
        const FIXTURE: &str = "/usr/bin/true";
        for binary in [PRLIMIT, BWRAP, FIXTURE] {
            validate_trusted_binary(binary)?;
        }
        let plan = plan_confined_stdio_launch(FIXTURE, &[])?;
        let mut child = Command::new(plan.program)
            .args(&plan.args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("isolated host-readiness probe could not start: {e}"))?;
        // The fixed root-owned launcher is the only producer of this stderr.
        // Drain concurrently so diagnostics cannot deadlock a blocked writer,
        // and never retain more than the explicitly bounded evidence budget.
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "isolated host-readiness probe stderr unavailable".to_owned())?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .take(MAX_HOST_PROBE_STDERR_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if start.elapsed() < HOST_PROBE_TIMEOUT => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(
                        "isolated host-readiness probe timed out; no tool-call dispatch was consumed"
                            .to_owned(),
                    );
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!(
                        "isolated host-readiness probe failed to wait: {e}; no tool-call dispatch was consumed"
                    ));
                }
            }
        };
        let stderr = reader
            .join()
            .map_err(|_| "isolated host-readiness stderr reader panicked".to_owned())?
            .map_err(|e| format!("isolated host-readiness stderr read failed: {e}"))?;
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format_host_probe_rejection(&status.to_string(), &stderr)),
            Err(error) => Err(error),
        }
    }
}

/// Host diagnostics are *evidence*, not shell text or executable instructions.
/// Normalize control characters and cap what reaches the desktop or terminal.
fn format_host_probe_rejection(status: &str, stderr: &[u8]) -> String {
    let bounded = &stderr[..stderr.len().min(MAX_HOST_PROBE_STDERR_BYTES)];
    let clean = String::from_utf8_lossy(bounded)
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated = if stderr.len() > MAX_HOST_PROBE_STDERR_BYTES {
        " [truncated]"
    } else {
        ""
    };
    let diagnostic = if clean.is_empty() {
        String::new()
    } else {
        format!("; bounded launcher stderr: {clean}{truncated}")
    };
    format!(
        "isolated host-readiness probe exited {status}{diagnostic}; check bubblewrap user namespaces and local AppArmor/LSM policy without disabling host security; no tool-call dispatch was consumed"
    )
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
    // Even a root-owned executable can be swapped by the owner of an
    // ancestor directory. The initial runner permits only fixed root-owned
    // /usr/bin ancestry, not caller-owned directories with mode 0755.
    for parent in ["/", "/usr", "/usr/bin"] {
        let parent_meta = std::fs::symlink_metadata(parent)
            .map_err(|error| format!("cannot inspect {parent}: {error}"))?;
        if !parent_meta.is_dir() || parent_meta.uid() != 0 {
            return Err(format!(
                "restricted runner refuses non-root-owned executable directory {parent}"
            ));
        }
    }
    Ok(())
}

/// Bounded terminal observation from the separate process worker.
/// Only the persistence worker may append the result to the journal.
#[derive(Debug)]
pub struct ConfinedStdioObservation {
    pub call_id: ToolCallId,
    pub route_id: RouteId,
    pub kind: ToolCallOutcomeKind,
    pub text: String,
}

/// Consume the move-only reservation off the persistence worker.
/// RouteDispatched must already be durable. Never automatically retry.
pub fn run_reserved_stdio_tool_call(
    reservation: ReservedStdioToolDispatch,
) -> ConfinedStdioObservation {
    let invocation = &reservation.invocation().invocation;
    let call_id = invocation.call_id;
    let route_id = invocation.route_id;
    let kind_and_text = run_one_shot(
        invocation.executable.as_str(),
        &invocation.argv,
        invocation.request_frame.as_str(),
        call_id.get(),
    );
    let (kind, text) = match kind_and_text {
        Ok(frame) => (ToolCallOutcomeKind::Result, frame),
        Err(error) => (ToolCallOutcomeKind::Error, error),
    };
    ConfinedStdioObservation {
        call_id,
        route_id,
        kind,
        text,
    }
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
    let request: serde_json::Value =
        serde_json::from_str(frame).map_err(|_| "refused malformed MCP request JSON".to_owned())?;
    if request["id"].as_u64() != Some(request_id) {
        return Err("MCP request does not match approved call identity".to_owned());
    }
    let method = request["method"]
        .as_str()
        .ok_or_else(|| "refused MCP request without method".to_owned())?;
    if !matches!(method, "tools/call" | "tools/list") {
        return Err("restricted MCP adapter supports only tools/call and tools/list".to_owned());
    }
    preflight_confined_stdio_launch(executable, argv)?;
    let plan = plan_confined_stdio_launch(executable, argv)?;
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
        if output.len() > MAX_MCP_FRAME_BYTES || stderr.len() > MAX_STDERR_BYTES as usize {
            return Err("sandbox MCP stdout/stderr exceeded byte budget".to_owned());
        }
        if !status.success() {
            let diagnostic = String::from_utf8_lossy(&stderr);
            return Err(format!(
                "isolated MCP process exited unsuccessfully: {status}; bounded stderr: {diagnostic}"
            ));
        }
        // A child may exit before reading stdin. Preserve its bounded
        // nonzero-status stderr instead of masking it with BrokenPipe.
        wrote.map_err(|e| format!("sandbox request write failed: {e}"))?;
        let text = std::str::from_utf8(&output)
            .map_err(|_| "sandbox MCP stdout was not UTF-8".to_owned())?;
        let response = decode_stdio_response(text, request_id)
            .map_err(|e| format!("sandbox MCP response rejected: {e:?}"))?;
        match response {
            McpResponse::Complete(result) => {
                if method == "tools/call" {
                    // The 2026 protocol requires a content array even if the
                    // provider also supplies structuredContent. This is shape
                    // validation, not an endorsement of untrusted content.
                    validate_tools_call_result(&result)
                        .map_err(|e| format!("sandbox MCP tools/call result rejected: {e:?}"))?;
                }
                if method == "tools/list" {
                    // The catalogue is untrusted provider testimony. Reject
                    // malformed, duplicate or oversized pages; never import
                    // it into the immutable configured tool allowlist.
                    parse_tools_list_page(&result)
                        .map_err(|e| format!("sandbox MCP tools/list page rejected: {e:?}"))?;
                }
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
    fn failed_host_probe_exposes_only_bounded_sanitized_launcher_diagnostics() {
        let error = format_host_probe_rejection(
            "exit status: 1",
            b"bwrap: user namespaces denied\x1b[31m\r\n",
        );
        assert!(error.contains("bwrap: user namespaces denied"));
        assert!(!error.contains('\x1b'));
        assert!(error.contains("no tool-call dispatch was consumed"));

        let oversized = vec![b'a'; MAX_HOST_PROBE_STDERR_BYTES + 1];
        let error = format_host_probe_rejection("exit status: 1", &oversized);
        assert!(error.contains("[truncated]"));
        assert!(error.len() < MAX_HOST_PROBE_STDERR_BYTES + 400);
    }

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
            assert!(plan.args().contains(&"--unshare-user".to_owned()));
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
    fn live_bubblewrap_approved_catalog_inspection_smoke() {
        if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
            return;
        }
        use chatarium_protocol::mcp_wire::{encode_stdio_frame, tools_list_request};
        let frame = encode_stdio_frame(&tools_list_request(93, None).unwrap()).unwrap();
        let reply = r#"{"jsonrpc":"2.0","id":93,"result":{"resultType":"complete","tools":[{"name":"weather.read","description":"A tool list is not authority","inputSchema":{"type":"object"}}],"nextCursor":"page2"}}"#;
        let command = format!("s/.*/{reply}/p");
        let returned = run_one_shot(
            "/usr/bin/sed",
            &["-n".to_owned(), "-e".to_owned(), command],
            &frame,
            93,
        )
        .unwrap();
        assert_eq!(returned, format!("{reply}\n"));

        // A malformed advertised schema cannot be recorded as a successful
        // catalogue inspection even though the JSON-RPC envelope is complete.
        let malformed = r#"{"jsonrpc":"2.0","id":93,"result":{"resultType":"complete","tools":[{"name":"x"}]}}"#;
        let error = run_one_shot(
            "/usr/bin/sed",
            &[
                "-n".to_owned(),
                "-e".to_owned(),
                format!("s/.*/{malformed}/p"),
            ],
            &frame,
            93,
        )
        .unwrap_err();
        assert!(error.contains("tools/list page rejected"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_bubblewrap_rejects_malformed_tool_result_even_with_matching_id() {
        if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
            return;
        }
        use chatarium_protocol::mcp_wire::{encode_stdio_frame, tools_call_request};
        let request = tools_call_request(74, "fixture.echo", &serde_json::json!({})).unwrap();
        let frame = encode_stdio_frame(&request).unwrap();
        let invalid = r#"{"jsonrpc":"2.0","id":74,"result":{"resultType":"complete","content":[{"type":"text"}]}}"#;
        let error = run_one_shot(
            "/usr/bin/sed",
            &[
                "-n".to_owned(),
                "-e".to_owned(),
                format!("s/.*/{invalid}/p"),
            ],
            &frame,
            74,
        )
        .unwrap_err();
        assert!(error.contains("tools/call result rejected"));
    }

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
    fn live_confined_host_readiness_probe() {
        if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
            return;
        }
        probe_confined_stdio_host().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_owned_usr_bin_ancestry_is_required_for_trusted_launcher() {
        // The hosted Linux worker uses root-owned /usr and /usr/bin.
        // If its filesystem ownership differs, the policy must refuse.
        let result = validate_trusted_binary("/usr/bin/true");
        if std::fs::symlink_metadata("/usr/bin").is_ok_and(|metadata| {
            use std::os::unix::fs::MetadataExt;
            metadata.uid() == 0
        }) {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_sandbox_denies_host_etc_and_home_paths() {
        if std::env::var_os("CHATARIUM_TEST_LINUX_MCP_SANDBOX").is_none() {
            return;
        }
        use chatarium_protocol::mcp_wire::{encode_stdio_frame, tools_call_request};
        let frame = encode_stdio_frame(
            &tools_call_request(88, "fixture.nohost", &serde_json::json!({})).unwrap(),
        )
        .unwrap();
        for forbidden in ["/etc/passwd", "/home"] {
            let error =
                run_one_shot("/usr/bin/stat", &[forbidden.to_owned()], &frame, 88).unwrap_err();
            assert!(
                error.contains("No such file or directory"),
                "forbidden host path {forbidden} produced unexpected sandbox error: {error}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_confinement_binary_never_triggers_unsafe_direct_launch() {
        let candidate = "/usr/bin/chatarium-nonexistent-test-executable-981293";
        assert!(validate_trusted_binary(candidate).is_err());
    }
}
