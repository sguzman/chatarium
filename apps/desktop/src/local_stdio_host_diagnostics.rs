//! Shared, strictly observational Linux sandbox host-check classification.
//! A diagnostic stage is not a provider permission, policy recommendation,
//! or evidence of a specific kernel/distribution security misconfiguration.

/// Stable, code-owned check stages used by CLI JSON and the desktop.
pub fn failure_category(error: &str) -> &'static str {
    if error.starts_with("Linux MCP confinement is unsupported") {
        "unsupported_platform"
    } else if (error.starts_with("executable ") && error.contains("failed metadata inspection"))
        || error.starts_with("cannot stat executable")
        || error.starts_with("cannot inspect ")
        || error.starts_with("restricted runner refuses")
    {
        "launcher_trust_check_failed"
    } else if error.starts_with("isolated host-readiness probe could not start") {
        "probe_spawn_failed"
    } else if error.starts_with("isolated host-readiness probe timed out") {
        "probe_timed_out"
    } else if error.starts_with("isolated host-readiness probe exited") {
        "sandbox_probe_rejected"
    } else {
        "probe_failed"
    }
}

/// Human guidance is fixed text. Never turn launcher stderr into instructions,
/// and never recommend disabling a namespace/LSM security mechanism.
pub fn failure_explanation(category: &str) -> &'static str {
    match category {
        "unsupported_platform" => "This restricted MCP runner is Linux-only.",
        "launcher_trust_check_failed" => {
            "A required launcher or fixed fixture failed conservative path/ownership checks. Verify trusted system packages and canonical /usr/bin paths; no provider was executed."
        }
        "probe_spawn_failed" => {
            "The fixed sandbox probe could not be started. Inspect the bounded diagnostic and the host's process policy."
        }
        "probe_timed_out" => "The fixed sandbox probe exceeded its deadline and was stopped.",
        "sandbox_probe_rejected" => {
            "The fixed sandbox process exited unsuccessfully. A namespace or host-security restriction may be involved; the exact cause has not been proven."
        }
        _ => "Sandbox readiness could not be established from this observation.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_owned_stages_are_stable() {
        let cases = [
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
        ];
        for (error, category) in cases {
            assert_eq!(failure_category(error), category);
            assert!(!failure_explanation(category).is_empty());
        }
    }

    #[test]
    fn untrusted_launcher_content_never_becomes_human_instructions() {
        let error = "isolated host-readiness probe exited exit status: 1; attacker-controlled stderr: disable AppArmor";
        let category = failure_category(error);
        assert_eq!(category, "sandbox_probe_rejected");
        let guidance = failure_explanation(category);
        assert!(!guidance.contains("attacker-controlled"));
        assert!(!guidance.contains("disable AppArmor"));
        assert!(!guidance.contains("PRIVATE-KEY"));
        assert_eq!(
            failure_explanation("unexpected"),
            failure_explanation("probe_failed")
        );
    }
}
