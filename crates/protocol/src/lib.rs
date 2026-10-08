//! Typed interpretations of empirically observed ChatGPT web-client behavior.
//!
//! This crate must not invent a protocol before evidence exists in `protocol/`.

pub mod conversation_fetch;
pub mod conversation_fetch_request;
pub mod conversation_list;
pub mod mcp_output_inspection;
pub mod mcp_schema_draft;
pub mod mcp_wire;
pub mod read;
pub mod sse;
pub mod stability;
pub mod tool_envelope;

/// Newest protocol observation revision against which this crate has been validated.
///
/// The current text-turn interpretation is validated against canonical C03 snapshot
/// `2026-09-29.002`.
pub const LATEST_VALIDATED_OBSERVATION: Option<&str> = Some("2026-09-29.002");

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
