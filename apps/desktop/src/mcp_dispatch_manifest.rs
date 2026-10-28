//! Bounded, content-free evidence manifest for each prepared Responses dispatch.
//!
//! Recording admission is distinct from inclusion in a composed request.
//! DispatchAttempted is an intent boundary, not proof that the remote accepted
//! the request. This module never grants context or tool execution authority.

use crate::context_composer::{ContextPlan, ContextSource, InclusionDecision};
use chatarium_core::{EventKind, LocalConversationId};
use chatarium_store::EventEnvelope;
use chatarium_store::authored::{DecodedUserMessageCommit, decode_user_message_commit};
use chatarium_store::continuation_execution_audit::replay_worker_continuation_execution_audit;
use chatarium_store::controller_coordination_audit::replay_controller_coordination_audit;
use eframe::egui;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const MAX_MANIFEST_TOOL_ROWS: usize = 32;
const MAX_VISIBLE_DISPATCHES: usize = 12;
const SCHEMA: &str = "chatarium-dispatch-context-evidence";
const VERSION: u64 = 1;

mod capture;
mod history;
mod provenance;
mod render;
#[cfg(test)]
mod tests;

pub use capture::{attach_to_dispatch_payload, capture};
pub use history::{
    DispatchTransportTrace, ListedToolEvidence, RecordedDispatchManifest,
    ToolCompositionReason, dispatch_history, portable_audit_json,
};
pub use provenance::{
    ToolProvenanceDisposition, ToolProvenanceEntry, ToolProvenanceReport,
    reverse_query_id, reverse_tool_provenance, reverse_tool_provenance_json, select_reverse_provenance,
};
pub use render::render_dispatches;

#[cfg(test)]
use history::parse_dispatch;
#[cfg(test)]
use render::DispatchHistoryCache;
