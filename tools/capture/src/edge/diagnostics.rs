//! Durable Edge startup and cleanup diagnostics; no new remote transport.
use super::*;

pub(super) fn append_init_shutdown_event(
    writer: &dyn DiagnosticEventWriter,
    run: &mut CaptureRun,
    kind: &str,
    payload: serde_json::Value,
    failures: &mut Vec<String>,
) {
    if let Err(error) = writer.append(run, kind, payload) {
        failures.push(format!("{kind}: {error}"));
    }
}

pub(super) fn result_error(result: &Result<(), TransportError>) -> String {
    result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default()
}

pub(super) fn path_is_absent(path: &Path) -> Result<bool, TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(TransportError::Process(format!(
            "verify cleanup path {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(all(windows, test))]
pub(super) fn pipe_smoke_arguments(profile: &Path) -> Vec<String> {
    pipe_profile_arguments(profile, "about:blank", true)
}

#[cfg(windows)]
pub(super) fn pipe_profile_arguments(profile: &Path, start_url: &str, incognito: bool) -> Vec<String> {
    let mut arguments = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
    ];
    if incognito {
        arguments.push("--incognito".to_owned());
    }
    arguments.push(start_url.to_owned());
    arguments
}

// Also used by the non-Windows plain-browser launch failure path.
pub(super) fn record_prelaunch_cleanup(
    run: &mut CaptureRun,
    lock_path: PathBuf,
    lock: ProfileLock,
    primary: TransportError,
) -> TransportError {
    drop(lock);
    let (lock_absent, cleanup_error) = match path_is_absent(&lock_path) {
        Ok(absent) => (absent, None),
        Err(error) => (false, Some(error.to_string())),
    };
    let cleanup_succeeded = lock_absent && cleanup_error.is_none();
    let cleanup_journal = run.append_event(
        "browser_shutdown_cleanup",
        json!({
            "exit_code": null,
            "process_exited": true,
            "harness_lock_absent": lock_absent,
            "active_port_file_absent": true,
            "active_port_file_required": false,
            "cleanup_succeeded": cleanup_succeeded,
            "cleanup_error": cleanup_error,
        }),
    );
    let original = primary.clone();
    let (primary_failure, mut journal_failures) = match primary {
        TransportError::Journal(error) => (None, vec![error]),
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure,
        } => (primary_failure, vec![journal_failure]),
        other => (Some(other.to_string()), Vec::new()),
    };
    if let Err(error) = cleanup_journal {
        journal_failures.push(error);
    }
    if journal_failures.is_empty() {
        original
    } else {
        TransportError::DiagnosticJournalFailure {
            primary_failure,
            journal_failure: journal_failures.join("; "),
        }
    }
}
