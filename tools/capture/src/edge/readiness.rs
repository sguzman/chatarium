//! Endpoint readiness and bounded retry helpers, unchanged.
use super::*;

pub(super) fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

pub(super) fn ensure_profile_directories(config: &EdgeLaunchConfig) -> Result<(), TransportError> {
    let local = fs::canonicalize(&config.local_app_data)
        .map_err(|error| TransportError::UnsafeProfile(format!("resolve LOCALAPPDATA: {error}")))?;
    let expected = local
        .join("Chatarium")
        .join("capture-browser")
        .join("edge-profile");
    if expected != config.profile {
        return Err(TransportError::UnsafeProfile(
            "derived profile path changed after validation".to_owned(),
        ));
    }
    let paths = [
        local.join("Chatarium"),
        local.join("Chatarium").join("capture-browser"),
        expected.clone(),
    ];
    for path in paths {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let actual = fs::canonicalize(&path).map_err(|error| {
                    TransportError::UnsafeProfile(format!(
                        "resolve profile component {}: {error}",
                        path.display()
                    ))
                })?;
                if crate::normalized_windows_path(&actual).map_err(TransportError::UnsafeProfile)?
                    != crate::normalized_windows_path(&path)
                        .map_err(TransportError::UnsafeProfile)?
                {
                    return Err(TransportError::UnsafeProfile(format!(
                        "profile component {} resolves outside its dedicated path",
                        path.display()
                    )));
                }
                if !actual.is_dir() {
                    return Err(TransportError::UnsafeProfile(format!(
                        "profile component {} is not a directory",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|error| {
                    TransportError::Process(format!(
                        "create dedicated profile directory {}: {error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(TransportError::UnsafeProfile(format!(
                    "inspect profile component {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn ensure_active_port_absent(path: &Path) -> Result<(), TransportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(TransportError::StaleState(format!(
            "{} already exists; refusing to trust a stale debugging endpoint",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TransportError::Process(format!(
            "inspect DevToolsActivePort {}: {error}",
            path.display()
        ))),
    }
}

pub(super) fn wait_for_debugging_endpoint(
    process: &mut EdgeProcess,
    active_port_file: &Path,
    deadline: Instant,
) -> Result<DevToolsPort, TransportError> {
    loop {
        if let Some(exit_code) = process
            .child
            .as_mut()
            .expect("launched process")
            .try_wait()
            .map_err(TransportError::Process)?
        {
            return Err(TransportError::Process(format!(
                "Edge exited with code {exit_code} before debugging endpoint discovery"
            )));
        }
        match fs::read_to_string(active_port_file) {
            Ok(contents) => return DevToolsPort::from_active_port(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TransportError::Process(format!(
                    "read DevToolsActivePort: {error}"
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(TransportError::Timeout { command_id: None });
        }
        thread::sleep(DEVTOOLS_RETRY_DELAY.min(deadline.saturating_duration_since(Instant::now())));
    }
}

pub(super) fn wait_for_devtools_readiness(
    process: &mut EdgeProcess,
    transport: &mut DevToolsBrowserTransport,
    port: DevToolsPort,
    run: &mut CaptureRun,
    deadline: Instant,
    evidence: &mut StartupEvidence,
) -> Result<(), TransportError> {
    run.append_event(
        "devtools_readiness_started",
        json!({
            "port": port.port(),
            "candidate_addresses": ["127.0.0.1", "::1"],
        }),
    )
    .map_err(TransportError::Journal)?;

    let mut attempts = 0u32;
    let mut last_error: Option<String> = None;
    loop {
        for family in [LoopbackAddressFamily::Ipv4, LoopbackAddressFamily::Ipv6] {
            if let Some(exit_code) = process
                .child
                .as_mut()
                .expect("launched process")
                .try_wait()
                .map_err(TransportError::Process)?
            {
                let error = TransportError::Process(format!(
                    "Edge exited with code {exit_code} during DevTools readiness"
                ));
                journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
                return Err(error);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            attempts = attempts.saturating_add(1);
            let connection = evidence.connection_mut(family);
            connection.attempts = connection.attempts.saturating_add(1);
            let endpoint = DevToolsEndpoint::loopback(port, family);
            match transport.probe_browser_version_at(
                endpoint,
                run,
                remaining.min(DEVTOOLS_ATTEMPT_TIMEOUT),
            ) {
                Ok(_) => {
                    evidence.connection_mut(family).succeeded = true;
                    evidence.connection_mut(family).last_result = Some("connected".to_owned());
                    run.append_event(
                        "devtools_readiness_succeeded",
                        json!({
                            "attempts": attempts,
                            "address": endpoint.address().to_string(),
                            "address_family": endpoint.family().as_str(),
                            "last_transient_error": last_error,
                        }),
                    )
                    .map_err(TransportError::Journal)?;
                    return Ok(());
                }
                Err(error @ TransportError::ReadinessTransient(_)) => {
                    last_error = Some(error.to_string());
                    evidence.connection_mut(family).last_result = Some(error.to_string());
                }
                Err(error) => {
                    evidence.connection_mut(family).last_result = Some(error.to_string());
                    journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
                    return Err(error);
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let error = TransportError::ReadinessTimeout {
                attempts,
                last_error: last_error.clone().unwrap_or_else(|| {
                    "startup deadline expired before a DevTools request could be attempted"
                        .to_owned()
                }),
            };
            journal_readiness_failure(run, attempts, &error, last_error.as_deref())?;
            return Err(error);
        }
        thread::sleep(DEVTOOLS_RETRY_DELAY.min(remaining));
    }
}

pub(super) fn journal_readiness_failure(
    run: &mut CaptureRun,
    attempts: u32,
    error: &TransportError,
    last_transient_error: Option<&str>,
) -> Result<(), TransportError> {
    run.append_event(
        "devtools_readiness_failed",
        json!({
            "attempts": attempts,
            "error": error.to_string(),
            "last_transient_error": last_transient_error,
        }),
    )
    .map(|_| ())
    .map_err(TransportError::Journal)
}
