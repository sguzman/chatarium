//! Typed local context composition for SIWC Responses requests.
//!
//! This module is intentionally transport-agnostic. It owns the ordered local
//! context plan that higher-level behavior/lifecycle code can inspect before the
//! SIWC bridge strips local provenance and sends only the Responses request shape.

use crate::local_inference_contract::LoadedContract;
use serde_json::{Value, json};

mod capabilities;
mod plan;
mod sources;
mod transcript;

pub use capabilities::*;
pub use plan::*;
pub use sources::*;
pub use transcript::*;

#[cfg(test)]
mod project_checkpoint_admission_tests;
#[cfg(test)]
mod tests;
