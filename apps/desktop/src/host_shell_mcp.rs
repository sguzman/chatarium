//! Opt-in native host shell behind the existing durable MCP route gate.
//! This deliberately uses the user's real environment (Fish config and PATH),
//! not the isolated stdio demo. A blacklist is a convenience filter, never a
//! sandbox: shell aliases, functions, substitutions, or encodings can evade it.
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE_NAME: &str = "host-shell-mcp.json";
pub const OPERATION: &str = "shell.command";
const MAX_COMMAND: usize = 8192;
// Capture complete build output to files up to this bound per stream; beyond
// it continue draining without blocking, and report that the log is partial.
const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
// Keep the journaled result below the existing 64 KiB tool-admission bound,
// so a verbose command can still return control to the agent.
const MAX_REPLY_JSON_BYTES: usize = 56_000;
const STDOUT_TAIL_BYTES: usize = 36 * 1024;
const STDERR_TAIL_BYTES: usize = 12 * 1024;
const SHELL_TIMEOUT: &str = "3600s";
const SCHEMA: &str = "chatarium-host-shell";
const SHELLS: &[&str] = &["fish", "bash", "zsh", "sh"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub shell: String,
    /// One command name per line, case sensitive. Empty means all permitted.
    pub blocked_commands: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            shell: "fish".to_owned(),
            blocked_commands: String::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !SHELLS.contains(&self.shell.as_str()) {
            return Err("shell must be fish, bash, zsh, or sh".to_owned());
        }
        if self.blocked_commands.len() > 16 * 1024 || self.blocked_commands.lines().count() > 256 {
            return Err("shell command blacklist exceeds allowed length".to_owned());
        }
        for name in self
            .blocked_commands
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if name.len() > 128
                || name.chars().any(|c| {
                    !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.' | '/' | '+')
                })
            {
                return Err(format!("invalid blocked command name: {name}"));
            }
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            let defaults = Self::default();
            defaults.save_atomic(path)?;
            return Ok(defaults);
        }
        let bytes = fs::read(path).map_err(|e| format!("cannot read host shell settings: {e}"))?;
        if bytes.len() > 32 * 1024 {
            return Err("host shell settings file too large".to_owned());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid host shell settings: {e}"))?;
        if value["schema"] != SCHEMA || value["version"] != 1 {
            return Err("unknown host shell settings version".to_owned());
        }
        let saved = Self {
            enabled: value["enabled"]
                .as_bool()
                .ok_or("missing shell enabled flag")?,
            shell: value["shell"]
                .as_str()
                .ok_or("missing shell selection")?
                .to_owned(),
            blocked_commands: value["blocked_commands"]
                .as_str()
                .ok_or("missing command blacklist")?
                .to_owned(),
        };
        saved.validate()?;
        Ok(saved)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "version": 1,
            "enabled": self.enabled,
            "shell": self.shell,
            "blocked_commands": self.blocked_commands,
        }))
        .map_err(|e| e.to_string())?;
        fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
        fs::rename(temporary, path).map_err(|e| e.to_string())
    }

    pub fn validate_request(&self, args: &Value) -> Result<(String, PathBuf), String> {
        self.validate()?;
        if !self.enabled {
            return Err("native host shell is disabled in local settings".to_owned());
        }
        let obj = args
            .as_object()
            .ok_or("shell arguments must be a JSON object")?;
        if obj.keys().any(|key| key != "command" && key != "cwd") {
            return Err("shell arguments allow only command and optional cwd".to_owned());
        }
        let command = obj
            .get("command")
            .and_then(Value::as_str)
            .ok_or("shell.command requires a command string")?;
        if command.trim().is_empty() || command.len() > MAX_COMMAND || command.contains('\0') {
            return Err("shell command is empty, oversized, or contains NUL".to_owned());
        }
        // Filter names at separator-delimited command positions. This is only
        // a convenience stoplist, not a security promise about shell programs.
        for name in self
            .blocked_commands
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if mentions_blocked_command(command, name) {
                return Err(format!("command {name} is blocked by local shell settings"));
            }
        }
        let cwd = if let Some(path) = obj.get("cwd") {
            let value = path.as_str().ok_or("cwd must be a string")?;
            if value.len() > 4096 || !value.starts_with('/') || value.contains('\0') {
                return Err("cwd must be an absolute local path".to_owned());
            }
            PathBuf::from(value)
        } else {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
        };
        if !cwd.is_dir() {
            return Err("shell working directory does not exist".to_owned());
        }
        Ok((command.to_owned(), cwd))
    }

    pub fn shell_path(&self) -> PathBuf {
        PathBuf::from(format!("/usr/bin/{}", self.shell))
    }
}

/// Best-effort stoplist. Not a security boundary. Cover direct command
/// positions after common shell separators, including paths to executables.
fn mentions_blocked_command(command: &str, blocked: &str) -> bool {
    command
        .split([';', '|', '&', '\n', '(', ')'])
        .any(|segment| {
            let segment = segment.trim_start();
            let first = segment.split_whitespace().next().unwrap_or_default();
            first == blocked || first.rsplit('/').next() == Some(blocked)
        })
}

/// A bounded excerpt plus a file containing the full logged stream.
/// The pipe is drained to EOF even after the file reaches its size ceiling,
/// so a verbose child does not block and isn't misreported as a failure.
struct ShellStream {
    tail: Vec<u8>,
    bytes_seen: u64,
    bytes_logged: u64,
    log_path: Option<PathBuf>,
    log_warning: Option<String>,
}

fn log_directory() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("chatarium/shell-logs")
}

fn new_log(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|error| error.to_string())
}

fn read_stream(mut source: impl Read, path: PathBuf, tail_limit: usize) -> ShellStream {
    let mut log_warning = None;
    let mut file = match new_log(&path) {
        Ok(file) => Some(file),
        Err(error) => {
            log_warning = Some(format!("could not create shell log: {error}"));
            None
        }
    };
    let mut tail = Vec::<u8>::new();
    let mut chunk = [0_u8; 8192];
    let mut bytes_seen = 0_u64;
    let mut bytes_logged = 0_u64;
    loop {
        match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(amount) => {
                bytes_seen = bytes_seen.saturating_add(amount as u64);
                let keep = (MAX_LOG_BYTES.saturating_sub(bytes_logged) as usize).min(amount);
                if let Some(output) = file.as_mut() {
                    if keep > 0 {
                        if let Err(error) = output.write_all(&chunk[..keep]) {
                            log_warning = Some(format!("could not finish shell log: {error}"));
                            file = None;
                        } else {
                            bytes_logged += keep as u64;
                        }
                    }
                }
                if amount >= tail_limit {
                    tail.clear();
                    tail.extend_from_slice(&chunk[amount - tail_limit..amount]);
                } else {
                    let drop = tail.len().saturating_add(amount).saturating_sub(tail_limit);
                    if drop > 0 {
                        tail.drain(..drop);
                    }
                    tail.extend_from_slice(&chunk[..amount]);
                }
            }
            Err(error) => {
                log_warning = Some(format!("could not finish reading shell output: {error}"));
                break;
            }
        }
    }
    ShellStream {
        tail,
        bytes_seen,
        bytes_logged,
        log_path: (bytes_logged > 0 || file.is_some()).then_some(path),
        log_warning,
    }
}

fn utf8_tail_after(text: &str, bytes_to_drop: usize) -> &str {
    let mut start = bytes_to_drop.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Execute a single host shell command after durable route dispatch.
/// This returns the real exit code even when output is huge. Full output is
/// logged on disk (up to 64 MiB per stream), and a bounded excerpt returns
/// to the agent without exceeding automatic context-admission limits.
/// No retry occurs after crashes or ambiguous effects.
pub fn execute(settings: &Settings, args: &Value) -> Result<Value, String> {
    let (command, cwd) = settings.validate_request(args)?;
    let shell = settings.shell_path();
    if !shell.is_file() {
        return Err(format!(
            "selected shell {} is not installed",
            shell.display()
        ));
    }
    if !Path::new("/usr/bin/timeout").is_file() {
        return Err("bounded native shell requires /usr/bin/timeout".to_owned());
    }
    let output_dir = log_directory();
    let (stdout_path, stderr_path) = match fs::create_dir_all(&output_dir) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&output_dir, fs::Permissions::from_mode(0o700));
            }
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let stem = format!("shell-{}-{stamp}", std::process::id());
            (
                output_dir.join(format!("{stem}.stdout.log")),
                output_dir.join(format!("{stem}.stderr.log")),
            )
        }
        Err(_) => (
            output_dir.join("unavailable-stdout"),
            output_dir.join("unavailable-stderr"),
        ),
    };
    // /usr/bin/timeout applies a wall clock bound to the shell process
    // group; stdout and stderr are drained continuously into log files.
    // Fish -c loads its noninteractive configuration and inherited PATH.
    let mut child = Command::new("/usr/bin/timeout")
        .args(["--signal=KILL", "--kill-after=2s", SHELL_TIMEOUT])
        .arg(&shell)
        .args(["-c", &command])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start selected shell: {error}"))?;
    let stdout = child.stdout.take().ok_or("missing shell stdout")?;
    let stderr = child.stderr.take().ok_or("missing shell stderr")?;
    let out_worker = thread::spawn(move || read_stream(stdout, stdout_path, STDOUT_TAIL_BYTES));
    let err_worker = thread::spawn(move || read_stream(stderr, stderr_path, STDERR_TAIL_BYTES));
    let status = child.wait().map_err(|error| format!("cannot wait for shell: {error}"))?;
    let stdout = out_worker.join().map_err(|_| "shell stdout worker panicked")?;
    let stderr = err_worker.join().map_err(|_| "shell stderr worker panicked")?;
    let mut out_excerpt = String::from_utf8_lossy(&stdout.tail).into_owned();
    let mut err_excerpt = String::from_utf8_lossy(&stderr.tail).into_owned();
    let mut outcome = json!({
        "shell": settings.shell,
        "cwd": cwd.to_string_lossy(),
        "exit_code": status.code(),
        "stdout": out_excerpt,
        "stderr": err_excerpt,
        "stdout_total_bytes": stdout.bytes_seen,
        "stderr_total_bytes": stderr.bytes_seen,
        "stdout_log": stdout.log_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "stderr_log": stderr.log_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "stdout_logged_bytes": stdout.bytes_logged,
        "stderr_logged_bytes": stderr.bytes_logged,
        "stdout_truncated": stdout.bytes_seen > stdout.tail.len() as u64,
        "stderr_truncated": stderr.bytes_seen > stderr.tail.len() as u64,
        "stdout_log_truncated": stdout.bytes_seen > stdout.bytes_logged,
        "stderr_log_truncated": stderr.bytes_seen > stderr.bytes_logged,
        "stdout_log_warning": stdout.log_warning,
        "stderr_log_warning": stderr.log_warning,
        "timed_out": status.code() == Some(124) || status.code() == Some(137),
    });
    // ANSI-colored build logs can expand substantially when escaped in JSON.
    // Keep the exact log files but shrink only the excerpts as necessary;
    // admission of the terminal result must not fail due to output size.
    while serde_json::to_vec(&outcome)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_REPLY_JSON_BYTES
    {
        let shrink_stdout = out_excerpt.len() >= err_excerpt.len();
        let target = if shrink_stdout {
            &mut out_excerpt
        } else {
            &mut err_excerpt
        };
        if target.is_empty() {
            break;
        }
        let bytes_to_drop = (target.len() / 4).max(1);
        *target = utf8_tail_after(target, bytes_to_drop).to_owned();
        outcome["stdout"] = json!(out_excerpt);
        outcome["stderr"] = json!(err_excerpt);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_fish_with_all_commands_allowed() {
        let settings = Settings::default();
        assert!(settings.enabled);
        assert_eq!(settings.shell, "fish");
        assert!(settings.blocked_commands.is_empty());
        assert_eq!(settings.shell_path(), PathBuf::from("/usr/bin/fish"));
    }

    #[test]
    fn optional_cwd_and_blocked_command_are_validated() {
        let mut settings = Settings::default();
        settings.blocked_commands = "rm\nsudo".to_owned();
        assert!(
            settings
                .validate_request(&json!({"command": "rm -rf ./target"}))
                .is_err()
        );
        assert!(
            settings
                .validate_request(&json!({"command": "echo yes; /usr/bin/sudo whoami"}))
                .is_err()
        );
        assert!(
            settings
                .validate_request(&json!({"command": "echo safe"}))
                .is_ok()
        );
        assert!(
            settings
                .validate_request(&json!({"command": "echo ok", "cwd": "relative"}))
                .is_err()
        );
        assert!(
            settings
                .validate_request(&json!({"command": "echo ok", "args": []}))
                .is_err()
        );
        settings.enabled = false;
        assert!(
            settings
                .validate_request(&json!({"command": "echo yes"}))
                .is_err()
        );
    }

    #[test]
    fn verbose_shell_output_does_not_fail_or_block_and_preserves_exit_code() {
        let settings = Settings {
            shell: "sh".to_owned(),
            ..Settings::default()
        };
        let result = execute(
            &settings,
            &json!({"command": "yes verbose-build-output | head -n 25000; exit 7"}),
        )
        .unwrap();
        assert_eq!(result["exit_code"], 7);
        assert!(result["stdout_truncated"].as_bool().unwrap());
        assert!(result["stdout_total_bytes"].as_u64().unwrap() > 24 * 1024);
        assert!(result["stdout"].as_str().unwrap().contains("verbose-build-output"));
        assert!(result["stdout_log"].as_str().is_some());
        assert!(std::path::Path::new(result["stdout_log"].as_str().unwrap()).is_file());
        assert!(serde_json::to_vec(&result).unwrap().len() < 65_536);
    }

    #[test]
    fn selected_shell_and_stoplist_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-shell-settings-test-{}.json",
            std::process::id()
        ));
        let mut settings = Settings::default();
        settings.shell = "bash".to_owned();
        settings.blocked_commands = "shutdown\nreboot".to_owned();
        settings.save_atomic(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        let _ = fs::remove_file(path);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_host_shell_executes_with_explicit_user_environment() {
        let settings = Settings {
            shell: "sh".to_owned(),
            ..Settings::default()
        };
        let output = execute(&settings, &json!({"command": "printf shell_mcp_works"})).unwrap();
        assert_eq!(output["stdout"], "shell_mcp_works");
        assert_eq!(output["exit_code"], 0);
    }
}
