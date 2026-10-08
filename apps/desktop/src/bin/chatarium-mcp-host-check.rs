//! A provider-free, journal-free compatibility check for the production
//! Linux MCP one-shot sandbox. No permission or tool route is consumed.

#[path = "../local_stdio_runner.rs"]
#[allow(dead_code)]
mod local_stdio_runner;

fn main() {
    match local_stdio_runner::probe_confined_stdio_host() {
        Ok(()) => {
            println!("Chatarium MCP sandbox host: READY (fixed /usr/bin/true fixture only)");
            println!("No MCP provider, approval, journal, or network was accessed.");
        }
        Err(error) => {
            eprintln!("Chatarium MCP sandbox host: NOT READY");
            eprintln!("{error}");
            eprintln!("No MCP provider, approval, journal, or network was accessed.");
            std::process::exit(1);
        }
    }
}
