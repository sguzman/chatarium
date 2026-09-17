//! Core logic for Chatarium's one-command browser capture harness.

/// Durable private capture-run state and append-only event journal.
pub mod run;

use serde::Deserialize;
use std::env;
use std::path::{Path, PathBuf};

const C00_IDLE_LOAD: &str = include_str!("../../../protocol/experiments/C00-idle-load.toml");
const C03_SEND_TEXT: &str = include_str!("../../../protocol/experiments/C03-send-text.toml");

/// Parsed canonical capture experiment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Experiment {
    /// Experiment schema identifier.
    pub schema: String,
    /// Experiment schema version.
    pub version: u64,
    /// Stable experiment identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Initial page URL.
    pub start_url: String,
    /// Maximum experiment time in seconds.
    pub timeout_seconds: u64,
    /// Whether the experiment definition may mutate remote state.
    pub mutation: bool,
    /// Action specification.
    pub action: Action,
    /// Success observation specification.
    pub success: Success,
    /// Settle-window specification.
    pub settle: Settle,
    /// Optional retry policy.
    #[serde(default)]
    pub retry: Option<Retry>,
    /// Capture-feature switches.
    pub capture: Capture,
}

/// Experiment action.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Action {
    /// Action discriminator.
    #[serde(rename = "type")]
    pub kind: String,
    /// Exact text for text-send experiments.
    #[serde(default)]
    pub text: Option<String>,
}

/// Experiment success condition.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Success {
    /// Success-condition discriminator.
    #[serde(rename = "type")]
    pub kind: String,
    /// Exact marker text when applicable.
    #[serde(default)]
    pub text: Option<String>,
}

/// Quiet-period rules used to finalize an observation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Settle {
    /// Required network quiet period.
    pub quiet_network_ms: u64,
    /// Required DOM quiet period.
    pub quiet_dom_ms: u64,
    /// Minimum total observation time.
    pub minimum_observation_ms: u64,
}

/// Remote mutation retry policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Retry {
    /// Whether Chatarium may automatically retry after remote mutation begins.
    pub automatic_after_mutation: bool,
}

/// Capture-feature switches.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Capture {
    /// Capture HTTP network observations.
    pub network: bool,
    /// Capture WebSocket observations.
    pub websocket: bool,
    /// Attempt response body collection.
    pub response_bodies: bool,
    /// Observe/hash frontend assets.
    pub frontend_assets: bool,
    /// Capture page/DOM observations.
    pub page_observations: bool,
}

/// Return one embedded canonical experiment by stable ID.
#[must_use]
pub fn canonical_experiment(id: &str) -> Option<Experiment> {
    let source = match id {
        "C00-idle-load" => C00_IDLE_LOAD,
        "C03-send-text" => C03_SEND_TEXT,
        _ => return None,
    };
    toml::from_str(source).ok()
}

/// Return all embedded canonical experiment IDs.
#[must_use]
pub const fn canonical_experiment_ids() -> &'static [&'static str] {
    &["C00-idle-load", "C03-send-text"]
}

/// Chatarium-owned capture root on Windows-like environments.
#[must_use]
pub fn capture_root() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|base| base.join("Chatarium").join("capture-browser"))
}

/// Dedicated Edge user-data directory owned by Chatarium.
#[must_use]
pub fn capture_profile_path() -> Option<PathBuf> {
    capture_root().map(|root| root.join("edge-profile"))
}

/// Known default Edge user-data directories that must never be reused by the harness.
#[must_use]
pub fn known_default_edge_profile_roots() -> Vec<PathBuf> {
    let Some(local_app_data) = env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };

    [
        ["Microsoft", "Edge", "User Data"],
        ["Microsoft", "Edge Beta", "User Data"],
        ["Microsoft", "Edge Dev", "User Data"],
        ["Microsoft", "Edge SxS", "User Data"],
    ]
    .into_iter()
    .map(|parts| {
        parts
            .into_iter()
            .fold(local_app_data.clone(), |path, part| path.join(part))
    })
    .collect()
}

/// Whether a candidate user-data directory is safe for Chatarium capture ownership.
///
/// This intentionally rejects a known default Edge root, a path nested below one, or a path that
/// would contain one. The harness must not gain a `--force` escape hatch for this invariant.
#[must_use]
pub fn is_safe_capture_profile(candidate: &Path) -> bool {
    let candidate = normalized_for_compare(candidate);
    if candidate.as_os_str().is_empty() {
        return false;
    }

    known_default_edge_profile_roots().into_iter().all(|root| {
        let root = normalized_for_compare(&root);
        candidate != root && !candidate.starts_with(&root) && !root.starts_with(&candidate)
    })
}

/// Candidate installed Microsoft Edge executables, ordered by preference.
#[must_use]
pub fn edge_executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(program_files_x86) = env::var_os("PROGRAMFILES(X86)") {
        candidates.push(
            PathBuf::from(program_files_x86)
                .join("Microsoft")
                .join("Edge")
                .join("Application")
                .join("msedge.exe"),
        );
    }
    if let Some(program_files) = env::var_os("PROGRAMFILES") {
        candidates.push(
            PathBuf::from(program_files)
                .join("Microsoft")
                .join("Edge")
                .join("Application")
                .join("msedge.exe"),
        );
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(local_app_data)
                .join("Microsoft")
                .join("Edge")
                .join("Application")
                .join("msedge.exe"),
        );
    }

    candidates
}

/// First Edge executable candidate that currently exists.
#[must_use]
pub fn find_edge_executable() -> Option<PathBuf> {
    edge_executable_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

fn normalized_for_compare(path: &Path) -> PathBuf {
    let text = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    PathBuf::from(text.trim_end_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_experiments_parse() {
        for id in canonical_experiment_ids() {
            let experiment = canonical_experiment(id).expect("canonical experiment must parse");
            assert_eq!(&experiment.id, id);
            assert_eq!(experiment.schema, "chatarium-experiment");
            assert_eq!(experiment.version, 1);
        }
    }

    #[test]
    fn c03_exact_text_is_auditable_and_non_retrying() {
        let experiment = canonical_experiment("C03-send-text").expect("C03");
        assert!(experiment.mutation);
        assert_eq!(
            experiment.action.text.as_deref(),
            Some("respond with exactly CHATARIUM_PROTOCOL_TEST_001")
        );
        assert_eq!(
            experiment.success.text.as_deref(),
            Some("CHATARIUM_PROTOCOL_TEST_001")
        );
        assert_eq!(
            experiment
                .retry
                .as_ref()
                .map(|retry| retry.automatic_after_mutation),
            Some(false)
        );
    }

    #[test]
    fn unknown_experiment_is_not_invented() {
        assert!(canonical_experiment("C99-made-up").is_none());
    }

    #[test]
    fn profile_safety_rejects_default_tree_relationships() {
        let default = PathBuf::from(r"C:\Users\example\AppData\Local\Microsoft\Edge\User Data");
        let nested = default.join("Default");
        let parent = PathBuf::from(r"C:\Users\example\AppData\Local\Microsoft\Edge");

        fn local_check(candidate: &Path, root: &Path) -> bool {
            let candidate = normalized_for_compare(candidate);
            let root = normalized_for_compare(root);
            candidate != root && !candidate.starts_with(&root) && !root.starts_with(&candidate)
        }

        assert!(!local_check(&default, &default));
        assert!(!local_check(&nested, &default));
        assert!(!local_check(&parent, &default));
        assert!(local_check(
            Path::new(r"C:\Users\example\AppData\Local\Chatarium\capture-browser\edge-profile"),
            &default
        ));
    }
}
