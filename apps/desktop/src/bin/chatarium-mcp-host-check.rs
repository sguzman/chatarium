//! A provider-free, journal-free compatibility check for the production
//! Linux MCP one-shot sandbox. No permission or tool route is consumed.

#[path = "../local_stdio_host_diagnostics.rs"]
mod local_stdio_host_diagnostics;
#[path = "../local_stdio_runner.rs"]
#[allow(dead_code)]
mod local_stdio_runner;

use local_stdio_host_diagnostics::failure_category;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Human,
    Json,
    Help,
}

// Parse arguments before probing: invalid flags and --help are inert.
fn parse_mode(args: &[String]) -> Result<Mode, &'static str> {
    match args {
        [] => Ok(Mode::Human),
        [flag] if flag == "--json" => Ok(Mode::Json),
        [flag] if flag == "--help" || flag == "-h" => Ok(Mode::Help),
        _ => Err("unknown or conflicting arguments"),
    }
}

fn json_report(result: &Result<(), String>) -> serde_json::Value {
    let (status, category, detail) = match result {
        Ok(()) => ("ready", "none", None),
        Err(error) => ("not_ready", failure_category(error), Some(error.as_str())),
    };
    serde_json::json!({
        "schema_version": 1,
        "check": "fixed_usr_bin_true_sandbox",
        "status": status,
        "failure_category": category,
        "detail": detail,
        "tool_route_consumed": false,
        "provider_executed": false,
        "journal_accessed": false,
        "host_network_accessed": false
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match parse_mode(&args) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("{error}");
            eprintln!("Usage: chatarium-mcp-host-check [--json | --help]");
            std::process::exit(2);
        }
    };
    if mode == Mode::Help {
        println!("Usage: chatarium-mcp-host-check [--json | --help]");
        println!("Probe the fixed Linux MCP sandbox fixture; no provider or tool route.");
        return;
    }

    let result = local_stdio_runner::probe_confined_stdio_host();
    match mode {
        Mode::Human => match &result {
            Ok(()) => {
                println!("Chatarium MCP sandbox host: READY (fixed /usr/bin/true fixture only)");
                println!("No MCP provider, approval, journal, or network was accessed.");
            }
            Err(error) => {
                eprintln!("Chatarium MCP sandbox host: NOT READY");
                eprintln!("{error}");
                eprintln!("No MCP provider, approval, journal, or network was accessed.");
            }
        },
        Mode::Json => println!("{}", json_report(&result)),
        Mode::Help => unreachable!("help returned before probing"),
    }
    if result.is_err() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_exclusive_and_checked_before_running_any_probe() {
        assert_eq!(parse_mode(&[]), Ok(Mode::Human));
        assert_eq!(parse_mode(&["--json".to_owned()]), Ok(Mode::Json));
        assert_eq!(parse_mode(&["--help".to_owned()]), Ok(Mode::Help));
        assert_eq!(parse_mode(&["-h".to_owned()]), Ok(Mode::Help));
        for args in [
            vec!["--json".to_owned(), "--help".to_owned()],
            vec!["--json".to_owned(), "--json".to_owned()],
            vec!["--unknown".to_owned()],
        ] {
            assert!(parse_mode(&args).is_err());
        }
    }

    #[test]
    fn json_report_preserves_no_authority_and_escapes_diagnostic_text() {
        let ready = json_report(&Ok(()));
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["failure_category"], "none");
        assert!(ready["detail"].is_null());
        let rejected = json_report(&Err(
            "isolated host-readiness probe exited exit status: 1; \"bad\"\\n".to_owned(),
        ));
        assert_eq!(rejected["schema_version"], 1);
        assert_eq!(rejected["status"], "not_ready");
        assert_eq!(rejected["failure_category"], "sandbox_probe_rejected");
        assert_eq!(rejected["tool_route_consumed"], false);
        assert_eq!(rejected["provider_executed"], false);
        assert_eq!(rejected["journal_accessed"], false);
        assert_eq!(rejected["host_network_accessed"], false);
        let roundtrip: serde_json::Value = serde_json::from_str(&rejected.to_string()).unwrap();
        assert_eq!(roundtrip, rejected);
        assert!(roundtrip["detail"].as_str().unwrap().contains("\"bad\""));
    }

    #[test]
    fn only_owned_failure_stage_prefixes_select_categories() {
        for (error, category) in [
            (
                "Linux MCP confinement is unsupported on this platform",
                "unsupported_platform",
            ),
            (
                "executable /usr/bin/bwrap failed metadata inspection: PathUnavailable",
                "launcher_trust_check_failed",
            ),
            (
                "restricted runner refuses non-root-owned executable /usr/bin/bwrap",
                "launcher_trust_check_failed",
            ),
            (
                "isolated host-readiness probe could not start: permission denied",
                "probe_spawn_failed",
            ),
            ("isolated host-readiness probe timed out", "probe_timed_out"),
            (
                "isolated host-readiness probe exited exit status: 1",
                "sandbox_probe_rejected",
            ),
            ("inconclusive launcher evidence", "probe_failed"),
        ] {
            assert_eq!(failure_category(error), category);
        }
    }
}
