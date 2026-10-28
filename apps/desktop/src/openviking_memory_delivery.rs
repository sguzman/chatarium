//! Host-owned at-most-once OpenViking writes and strict read-back checks.
//! Never call this on the egui thread or journal worker. The caller must
//! persist an outbound "dispatched" claim before invoking this function.
use crate::local_http_mcp;
use serde_json::{Value, json};

const MAX_CHECKPOINT_BYTES: usize = 16384;

mod checkpoint;
mod project;

use checkpoint::{MAX_HISTORY_SCAN_ITEMS, MAX_RETRIEVAL_BYTES, MAX_RETRIEVAL_ITEMS, read_confirmed_evidence};
pub use checkpoint::{
    RetrievedCheckpoint, checkpoint_uri, deliver_create_once, readback_matches,
    retrieve_confirmed_for_conversation, retrieve_confirmed_for_conversation_query,
    verify_uncertain_read_only,
};
pub(crate) use checkpoint::{checkpoint_relevance, relevance_terms};
pub use project::{
    PROJECT_MEMORY_GRANTS_FILE_NAME, ProjectMemoryGrant, ProjectMemoryGrants,
    retrieve_confirmed_for_project, retrieve_confirmed_for_project_query,
};

#[cfg(test)]
use checkpoint::deliver_create_once_with;
