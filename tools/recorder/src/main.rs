//! Command-line entry point for the reusable recorder library.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match chatarium_recorder::run_cli(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chatarium-recorder: {error}");
            ExitCode::from(2)
        }
    }
}
