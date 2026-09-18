//! CLI entry point for the Chatarium capture harness.

use chatarium_capture::{
    canonical_experiment, canonical_experiment_ids, capture_profile_path, find_edge_executable,
    init::{StdinInitOperator, SystemInitLauncher, run_init},
    is_safe_capture_profile,
    smoke::{SystemSmokeLauncher, diagnostic_run_base, run_smoke},
};
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chatarium-capture: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "doctor" => doctor(),
        [command] if command == "smoke-edge" => smoke_edge(),
        [command] if command == "init" => init(),
        [command] if command == "--help" || command == "-h" => {
            print_usage();
            Ok(())
        }
        [command, experiment_id] if command == "run" => {
            let experiment = canonical_experiment(experiment_id).ok_or_else(|| {
                format!(
                    "unknown experiment '{experiment_id}'; known experiments: {}",
                    canonical_experiment_ids().join(", ")
                )
            })?;
            Err(format!(
                "experiment '{}' is defined and validated, but live CDP capture is not implemented yet; no browser was launched and no remote state was changed",
                experiment.id
            ))
        }
        _ => {
            print_usage();
            Err("invalid arguments".to_owned())
        }
    }
}

fn print_usage() {
    println!(
        "Usage:\n  chatarium-capture doctor\n  chatarium-capture smoke-edge\n  chatarium-capture init\n  chatarium-capture run <experiment-id>\n\nExperiments:\n  {}\n\nsmoke-edge launches installed Edge in incognito mode with the dedicated profile\n  %LOCALAPPDATA%\\Chatarium\\capture-browser\\edge-profile at about:blank.\n  It persists Edge-managed profile state there plus run.json/events.jsonl under\n  %LOCALAPPDATA%\\Chatarium\\captures\\diagnostics\\<run-id>. It does not contact\n  ChatGPT or use the normal Edge profile, and closes the launched browser.\n\ninit opens ChatGPT in Chatarium's dedicated persistent Edge profile; sign in manually\n  and press Enter once in this terminal when ready. Chatarium does not extract\n  credentials or cookies. The profile remains stored for future capture runs.\n  Windows uses the local anonymous-pipe DevTools transport.",
        canonical_experiment_ids().join("\n  ")
    );
}

fn init() -> Result<(), String> {
    let diagnostic_base = diagnostic_run_base().map_err(|error| {
        format!("FAIL\nprimary: prepare bootstrap run location: {error}\nrun: not created")
    })?;
    let mut operator = StdinInitOperator;
    match run_init(&diagnostic_base, &SystemInitLauncher, &mut operator) {
        Ok(success) => {
            println!(
                "PASS\nprofile bootstrap: operator confirmed\nfinal target: {}\nprofile: persistent\ncleanup: passed\nrun: {}",
                success.final_target_url,
                success.run_path.display()
            );
            Ok(())
        }
        Err(failure) => Err(failure.to_string()),
    }
}

fn smoke_edge() -> Result<(), String> {
    let diagnostic_base = diagnostic_run_base().map_err(|error| {
        format!(
            "FAIL\nprimary: {error}\ncleanup: not needed (Edge was not launched)\nrun: not created"
        )
    })?;
    match run_smoke(&diagnostic_base, &SystemSmokeLauncher) {
        Ok(result) => {
            println!(
                "PASS\nEdge: {}\nCDP: {}\ntarget: about:blank\ndiagnostic: passed\ncleanup: passed\nrun: {}",
                result.edge_version,
                result.cdp_protocol_version,
                result.run_path.display()
            );
            Ok(())
        }
        Err(failure) => Err(failure.to_string()),
    }
}

fn doctor() -> Result<(), String> {
    println!("Chatarium capture doctor");
    println!("harness version: {}", env!("CARGO_PKG_VERSION"));

    let profile = capture_profile_path().ok_or_else(|| {
        "LOCALAPPDATA is unavailable; cannot derive capture profile path".to_owned()
    })?;
    println!("capture profile: {}", profile.display());
    println!(
        "capture profile safety: {}",
        if is_safe_capture_profile(&profile) {
            "safe (distinct from known default Edge profile trees)"
        } else {
            "UNSAFE"
        }
    );

    if !is_safe_capture_profile(&profile) {
        return Err(
            "derived capture profile violates the default-profile safety invariant".to_owned(),
        );
    }

    match find_edge_executable() {
        Some(edge) => println!("edge executable: {}", edge.display()),
        None => println!("edge executable: not found in standard locations"),
    }

    println!(
        "capture profile state: {}",
        if profile.exists() {
            "exists"
        } else {
            "not initialized"
        }
    );
    println!("canonical experiments:");
    for id in canonical_experiment_ids() {
        let experiment = canonical_experiment(id)
            .ok_or_else(|| format!("embedded experiment '{id}' failed to parse"))?;
        println!(
            "  {} | mutation={} | timeout={}s | {}",
            experiment.id, experiment.mutation, experiment.timeout_seconds, experiment.description
        );
    }

    println!("doctor is read-only; no browser or ChatGPT state was changed");
    Ok(())
}
