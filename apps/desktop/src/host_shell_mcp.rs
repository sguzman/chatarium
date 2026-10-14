//! Opt-in native host shell behind the existing durable MCP route gate.
//! This deliberately uses the user's real environment (Fish config and PATH),
//! not the isolated stdio demo. A blacklist is a convenience filter, never a
//! sandbox: shell aliases, functions, substitutions, or encodings can evade it.
use serde_json::{Value, json};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

pub const FILE_NAME: &str = "host-shell-mcp.json";
pub const OPERATION: &str = "shell.command";
const MAX_COMMAND: usize = 8192;
const MAX_CAPTURE: u64 = 24 * 1024;
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
        if self.blocked_commands.len() > 16 * 1024
            || self.blocked_commands.lines().count() > 256
        {
            return Err("shell command blacklist exceeds allowed length".to_owned());
        }
        for name in self.blocked_commands.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if name.len() > 128 || name.chars().any(|c| {
                !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.' | '/' | '+')
            }) {
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
            enabled: value["enabled"].as_bool().ok_or("missing shell enabled flag")?,
            shell: value["shell"].as_str().ok_or("missing shell selection")?.to_owned(),
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
        })).map_err(|e| e.to_string())?;
        fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
        fs::rename(temporary, path).map_err(|e| e.to_string())
    }

    pub fn validate_request(&self, args: &Value) -> Result<(String, PathBuf), String> {
        self.validate()?;
        if !self.enabled {
            return Err("native host shell is disabled in local settings".to_owned());
        }
        let obj = args.as_object().ok_or("shell arguments must be a JSON object")?;
        if obj.keys().any(|key| key != "command" && key != "cwd") {
            return Err("shell arguments allow only command and optional cwd".to_owned());
        }
        let command = obj.get("command").and_then(Value::as_str)
            .ok_or("shell.command requires a command string")?;
        if command.trim().is_empty() || command.len() > MAX_COMMAND || command.contains('\0') {
            return Err("shell command is empty, oversized, or contains NUL".to_owned());
        }
        // Filter names at separator-delimited command positions. This is only
        // a convenience stoplist, not a security promise about shell programs.
        for name in self.blocked_commands.lines().map(str::trim).filter(|s| !s.is_empty()) {
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
            std::env::var_os("HOME").map(PathBuf::from)
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
    command.split([';', '|', '&', '\n', '(', ')'])
        .any(|segment| {
            let segment = segment.trim_start();
            let first = segment.split_whitespace().next().unwrap_or_default();
            first == blocked || first.rsplit('/').next() == Some(blocked)
        })
}

/// Execute a single host shell command after durable route dispatch.
/// No retry occurs after crashes or ambiguous effects. The caller journals
/// the exact terminal observation in the persistence worker.
pub fn execute(settings: &Settings, args: &Value) -> Result<Value, String> {
    let (command, cwd) = settings.validate_request(args)?;
    let shell = settings.shell_path();
    if !shell.is_file() {
        return Err(format!("selected shell {} is not installed", shell.display()));
    }
    if !Path::new("/usr/bin/timeout").is_file() {
        return Err("bounded native shell requires /usr/bin/timeout".to_owned());
    }
    // /usr/bin/timeout applies a wall clock bound to the shell's process
    // group; child output is read concurrently with bounded buffers.
    // Fish -c loads its normal noninteractive configuration and inherited PATH.
    let mut child = Command::new("/usr/bin/timeout")
        .args(["--signal=KILL", "--kill-after=2s", "30s"])
        .arg(&shell)
        .args(["-c", &command])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start selected shell: {e}"))?;
    let stdout = child.stdout.take().ok_or("missing shell stdout")?;
    let stderr = child.stderr.take().ok_or("missing shell stderr")?;
    let out_worker = thread::spawn(move || -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        stdout.take(MAX_CAPTURE + 1).read_to_end(&mut out)
            .map_err(|e| format!("cannot read shell stdout: {e}"))?;
        Ok(out)
    });
    let err_worker = thread::spawn(move || -> Result<Vec<u8>, String> {
        let mut err = Vec::new();
        stderr.take(MAX_CAPTURE + 1).read_to_end(&mut err)
            .map_err(|e| format!("cannot read shell stderr: {e}"))?;
        Ok(err)
    });
    let status = child.wait().map_err(|e| format!("cannot wait for shell: {e}"))?;
    let stdout = out_worker.join().map_err(|_| "shell stdout worker panicked")??;
    let stderr = err_worker.join().map_err(|_| "shell stderr worker panicked")??;
    if stdout.len() > MAX_CAPTURE as usize || stderr.len() > MAX_CAPTURE as usize {
        return Err("shell output exceeds 24 KiB capture limit".to_owned());
    }
    Ok(json!({
        "shell": settings.shell,
        "cwd": cwd.to_string_lossy(),
        "exit_code": status.code(),
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "timed_out": status.code() == Some(124) || status.code() == Some(137)
    }))
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
        assert!(settings.validate_request(&json!({"command": "rm -rf ./target"})).is_err());
        assert!(settings.validate_request(&json!({"command": "echo yes; /usr/bin/sudo whoami"})).is_err());
        assert!(settings.validate_request(&json!({"command": "echo safe"})).is_ok());
        assert!(settings.validate_request(&json!({"command": "echo ok", "cwd": "relative"})).is_err());
        assert!(settings.validate_request(&json!({"command": "echo ok", "args": []})).is_err());
        settings.enabled = false;
        assert!(settings.validate_request(&json!({"command": "echo yes"})).is_err());
    }

    #[test]
    fn selected_shell_and_stoplist_round_trip() {
        let path = std::env::temp_dir().join(format!("chatarium-shell-settings-test-{}.json", std::process::id()));
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
        let settings = Settings { shell: "sh".to_owned(), ..Settings::default() };
        let output = execute(&settings, &json!({"command": "printf shell_mcp_works"})).unwrap();
        assert_eq!(output["stdout"], "shell_mcp_works");
        assert_eq!(output["exit_code"], 0);
    }
}
