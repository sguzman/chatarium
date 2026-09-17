//! Typed interpretations of empirically observed ChatGPT web-client behavior.
//!
//! This crate must not invent a protocol before evidence exists in `protocol/`.

/// Newest protocol observation revision against which this crate has been validated.
///
/// `None` is intentional during bootstrap: the first revision is created only after
/// controlled capture evidence has been sanitized and committed.
pub const LATEST_VALIDATED_OBSERVATION: Option<&str> = None;

/// Compatibility result when comparing runtime evidence to known observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// Runtime evidence matches a named observation closely enough for the requested flow.
    ValidatedAgainst(String),
    /// No empirical baseline exists yet for this flow.
    NoBaseline,
    /// Runtime evidence materially differs from the supported interpretation.
    Mismatch {
        /// The newest observation the code understood before the mismatch.
        expected_revision: String,
        /// Human-readable diagnostic detail; raw evidence belongs in diagnostics/storage.
        detail: String,
    },
}
