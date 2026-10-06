//! Durable per-conversation behavior controls admitted through the Local Inference Contract.
//!
//! This first profile intentionally exposes only exact request shapes that were
//! empirically accepted by the fixed SIWC capability suite.

use crate::context_composer::{CapabilityGate, CapabilitySlot};
use chatarium_core::LocalConversationId;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SCHEMA: &str = "chatarium-behavior-profiles";
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningMode {
    #[default]
    Default,
    Low,
}

impl ReasoningMode {
    const fn stable_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Low => "low",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "low" => Ok(Self::Low),
            _ => Err(format!("unsupported reasoning behavior value {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerbosityMode {
    #[default]
    Default,
    Low,
}

impl VerbosityMode {
    const fn stable_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Low => "low",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "default" => Ok(Self::Default),
            "low" => Ok(Self::Low),
            _ => Err(format!("unsupported verbosity behavior value {value}")),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorProfile {
    pub reasoning: ReasoningMode,
    pub verbosity: VerbosityMode,
    pub web_search: bool,
}

impl BehaviorProfile {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn request_patch(&self, gate: &CapabilityGate) -> Result<Value, String> {
        let mut patch = Map::new();

        if self.reasoning == ReasoningMode::Low {
            require(gate, CapabilitySlot::Reasoning)?;
            patch.insert("reasoning".to_owned(), json!({"effort": "low"}));
        }

        if self.verbosity == VerbosityMode::Low {
            require(gate, CapabilitySlot::Verbosity)?;
            patch.insert("text".to_owned(), json!({"verbosity": "low"}));
        }

        if self.web_search {
            require(gate, CapabilitySlot::WebSearch)?;
            patch.insert("tools".to_owned(), json!([{"type": "web_search"}]));
        }

        Ok(Value::Object(patch))
    }
}

fn require(gate: &CapabilityGate, slot: CapabilitySlot) -> Result<(), String> {
    if gate.allows(slot) {
        Ok(())
    } else {
        Err(format!(
            "behavior profile requests {}, but the active Local Inference Contract does not admit it",
            slot.label()
        ))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorProfileStore {
    entries: BTreeMap<String, BehaviorProfile>,
}

impl BehaviorProfileStore {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = fs::read(path).map_err(|error| {
            format!(
                "failed to read behavior profiles {}: {error}",
                path.display()
            )
        })?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid behavior profiles JSON: {error}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err("unsupported behavior profiles schema".to_owned());
        }

        let conversations = value
            .get("conversations")
            .and_then(Value::as_object)
            .ok_or_else(|| "behavior profiles conversations missing".to_owned())?;

        let mut entries = BTreeMap::new();
        for (conversation_id, raw) in conversations {
            let object = raw.as_object().ok_or_else(|| {
                format!("behavior profile for {conversation_id} is not an object")
            })?;
            let reasoning = ReasoningMode::parse(
                object
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .unwrap_or("default"),
            )?;
            let verbosity = VerbosityMode::parse(
                object
                    .get("verbosity")
                    .and_then(Value::as_str)
                    .unwrap_or("default"),
            )?;
            let web_search = object
                .get("web_search")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            entries.insert(
                conversation_id.clone(),
                BehaviorProfile {
                    reasoning,
                    verbosity,
                    web_search,
                },
            );
        }

        Ok(Self { entries })
    }

    #[must_use]
    pub fn for_conversation(&self, conversation_id: LocalConversationId) -> BehaviorProfile {
        self.entries
            .get(&conversation_id.to_string())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set(&mut self, conversation_id: LocalConversationId, profile: BehaviorProfile) {
        self.entries.insert(conversation_id.to_string(), profile);
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create behavior profile directory: {error}"))?;
        }

        let conversations = self
            .entries
            .iter()
            .map(|(conversation_id, profile)| {
                (
                    conversation_id.clone(),
                    json!({
                        "reasoning": profile.reasoning.stable_name(),
                        "verbosity": profile.verbosity.stable_name(),
                        "web_search": profile.web_search,
                    }),
                )
            })
            .collect::<Map<_, _>>();

        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "version": VERSION,
            "conversations": conversations,
        }))
        .map_err(|error| format!("failed to encode behavior profiles: {error}"))?;

        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .map_err(|error| format!("failed to write temporary behavior profiles: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to replace behavior profiles: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_composer::{CapabilityAdmission, CapabilityAdmissionState, CapabilitySlot};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-behavior-profile-{}-{nonce}.json",
            std::process::id()
        ))
    }

    fn gate(state: CapabilityAdmissionState) -> CapabilityGate {
        CapabilityGate {
            admissions: CapabilitySlot::ALL
                .into_iter()
                .map(|slot| CapabilityAdmission { slot, state })
                .collect(),
        }
    }

    #[test]
    fn default_profile_emits_no_request_patch() {
        assert_eq!(
            BehaviorProfile::default()
                .request_patch(&gate(CapabilityAdmissionState::BlockedContract))
                .unwrap(),
            json!({})
        );
    }

    #[test]
    fn proven_profile_values_emit_only_exact_tested_shapes() {
        let profile = BehaviorProfile {
            reasoning: ReasoningMode::Low,
            verbosity: VerbosityMode::Low,
            web_search: true,
        };

        assert_eq!(
            profile
                .request_patch(&gate(CapabilityAdmissionState::Available))
                .unwrap(),
            json!({
                "reasoning": {"effort": "low"},
                "text": {"verbosity": "low"},
                "tools": [{"type": "web_search"}],
            })
        );
    }

    #[test]
    fn nondefault_profile_fails_closed_when_capability_is_not_admitted() {
        let profile = BehaviorProfile {
            reasoning: ReasoningMode::Low,
            ..BehaviorProfile::default()
        };
        assert!(
            profile
                .request_patch(&gate(CapabilityAdmissionState::BlockedContract))
                .unwrap_err()
                .contains("reasoning")
        );
    }

    #[test]
    fn profiles_are_isolated_and_round_trip_atomically() {
        let path = temp_path();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let expected = BehaviorProfile {
            reasoning: ReasoningMode::Low,
            verbosity: VerbosityMode::Low,
            web_search: true,
        };

        let mut store = BehaviorProfileStore::default();
        store.set(first, expected.clone());
        store.save_atomic(&path).unwrap();

        let loaded = BehaviorProfileStore::load(&path).unwrap();
        assert_eq!(loaded.for_conversation(first), expected);
        assert_eq!(loaded.for_conversation(second), BehaviorProfile::default());

        let _ = fs::remove_file(path);
    }
}
