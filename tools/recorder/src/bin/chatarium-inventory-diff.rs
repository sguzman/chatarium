#[path = "../diff.rs"]
mod diff;

use std::env;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let [before, after, output] = args.as_slice() else {
        eprintln!(
            "Usage: chatarium-inventory-diff <before.requests.json> <after.requests.json> <output.diff.json>"
        );
        return ExitCode::from(2);
    };

    match diff::diff_inventory_files(Path::new(before), Path::new(after), Path::new(output)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chatarium-inventory-diff: {error}");
            ExitCode::from(2)
        }
    }
}
