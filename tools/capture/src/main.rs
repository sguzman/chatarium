//! CLI entry point for the Chatarium capture harness.

use chatarium_capture::{
    canonical_experiment, canonical_experiment_ids, capture_profile_path, find_edge_executable,
    is_safe_capture_profile,
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
        [command] if command == "init" => Err(
            "`init` is specified but CDP/browser bootstrap is not implemented yet; no state was changed"
                .to_owned(),
        ),
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
            eprintln!(
                "Usage:\n  chatarium-capture doctor\n  chatarium-capture init\n  chatarium-capture run <experiment-id>\n\nExperiments:\n  {}",
                canonical_experiment_ids().join("\n  ")
            );
            Err("invalid arguments".to_owned())
        }
    }
}

fn doctor() -> Result<(), String> {
    println!("Chatarium capture doctor");
    println!("harness version: {}", env!("CARGO_PKG_VERSION"));

    let profile = capture_profile_path()
        .ok_or_else(|| "LOCALAPPDATA is unavailable; cannot derive capture profile path".to_owned())?;
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
        return Err("derived capture profile violates the default-profile safety invariant".to_owned());
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
