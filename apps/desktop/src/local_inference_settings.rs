//! Durable mutable per-conversation inference controls.
//!
//! This is deliberately separate from the append-only conversation journal: the journal owns
//! authored/observed conversation history, while these values are user-editable preferences.
//! Writes are atomic and the file is intended to be included in local archive backups.

use chatarium_core::LocalConversationId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const SCHEMA: &str = "chatarium-local-inference-settings";
const VERSION: u64 = 1;
const MAX_MODEL_BYTES: usize = 512;
pub const DEFAULT_SLOW_WARNING_SECONDS: u32 = 12;
pub const MIN_SLOW_WARNING_SECONDS: u32 = 3;
pub const MAX_SLOW_WARNING_SECONDS: u32 = 120;
const MAX_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationInferenceSettings {
    pub model: Option<String>,
    pub instructions: String,
    pub developer_context: String,
    pub mcp_enabled: bool,
    pub slow_warning_seconds: u32,
}

impl Default for ConversationInferenceSettings {
    fn default() -> Self {
        Self {
            model: None,
            instructions: String::new(),
            developer_context: String::new(),
            mcp_enabled: false,
            slow_warning_seconds: DEFAULT_SLOW_WARNING_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InferenceSettingsStore {
    entries: BTreeMap<String, ConversationInferenceSettings>,
}

impl InferenceSettingsStore {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).map_err(|error| {
            format!(
                "failed to read inference settings {}: {error}",
                path.display()
            )
        })?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid inference settings JSON: {error}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err("unsupported local inference settings schema".to_owned());
        }
        let conversations = value
            .get("conversations")
            .and_then(Value::as_object)
            .ok_or_else(|| "local inference settings conversations missing".to_owned())?;
        let mut entries = BTreeMap::new();
        for (conversation_id, raw) in conversations {
            let object = raw
                .as_object()
                .ok_or_else(|| format!("settings for {conversation_id} are not an object"))?;
            let model = object
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let instructions = object
                .get("instructions")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let developer_context = object
                .get("developer_context")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            // Previous local settings did not contain MCP activation state.
            let mcp_enabled = object
                .get("mcp_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // Preserve compatibility with existing version 1 settings.
            let slow_warning_seconds = match object.get("slow_warning_seconds") {
                None => DEFAULT_SLOW_WARNING_SECONDS,
                Some(value) => value
                    .as_u64()
                    .and_then(|seconds| u32::try_from(seconds).ok())
                    .ok_or_else(|| "invalid slow response warning setting".to_owned())?,
            };
            validate(&ConversationInferenceSettings {
                model: model.clone(),
                instructions: instructions.clone(),
                developer_context: developer_context.clone(),
                mcp_enabled,
                slow_warning_seconds,
            })?;
            entries.insert(
                conversation_id.clone(),
                ConversationInferenceSettings {
                    model,
                    instructions,
                    developer_context,
                    mcp_enabled,
                    slow_warning_seconds,
                },
            );
        }
        Ok(Self { entries })
    }

    #[must_use]
    pub fn for_conversation(
        &self,
        conversation_id: LocalConversationId,
    ) -> ConversationInferenceSettings {
        self.entries
            .get(&conversation_id.to_string())
            .cloned()
            .unwrap_or_default()
    }

    /// Use the saved global new-chat preference only when a conversation has
    /// no explicit inference settings. Existing per-chat MCP opt-outs survive.
    pub fn for_conversation_with_mcp_defaults(
        &self,
        conversation_id: LocalConversationId,
        agent_defaults: &crate::mcp_agent_defaults::AgentDefaults,
    ) -> ConversationInferenceSettings {
        self.entries
            .get(&conversation_id.to_string())
            .cloned()
            .unwrap_or_else(|| {
                let mut settings = ConversationInferenceSettings::default();
                if agent_defaults.seed_new_conversations {
                    settings.mcp_enabled = true;
                    settings.developer_context = agent_defaults.bootstrap.clone();
                }
                settings
            })
    }

    pub fn set(
        &mut self,
        conversation_id: LocalConversationId,
        settings: ConversationInferenceSettings,
    ) -> Result<(), String> {
        validate(&settings)?;
        self.entries.insert(conversation_id.to_string(), settings);
        Ok(())
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create settings directory: {error}"))?;
        }
        let conversations = self
            .entries
            .iter()
            .map(|(conversation_id, settings)| {
                (
                    conversation_id.clone(),
                    json!({
                        "model": settings.model,
                        "instructions": settings.instructions,
                        "developer_context": settings.developer_context,
                        "mcp_enabled": settings.mcp_enabled,
                        "slow_warning_seconds": settings.slow_warning_seconds,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "version": VERSION,
            "conversations": conversations,
        }))
        .map_err(|error| format!("failed to encode inference settings: {error}"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes)
            .map_err(|error| format!("failed to write temporary inference settings: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to replace inference settings: {error}"))
    }
}

fn validate(settings: &ConversationInferenceSettings) -> Result<(), String> {
    if settings
        .model
        .as_ref()
        .is_some_and(|model| model.len() > MAX_MODEL_BYTES || model.chars().any(char::is_control))
    {
        return Err("invalid persisted model selector".to_owned());
    }
    if settings.instructions.len() > MAX_TEXT_BYTES {
        return Err("conversation instructions are too large".to_owned());
    }
    if settings.developer_context.len() > MAX_TEXT_BYTES {
        return Err("developer context is too large".to_owned());
    }
    if !(MIN_SLOW_WARNING_SECONDS..=MAX_SLOW_WARNING_SECONDS)
        .contains(&settings.slow_warning_seconds)
    {
        return Err("slow response warning must be between 3 and 120 seconds".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-local-inference-settings-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn new_local_conversations_follow_saved_mcp_defaults_without_overriding_opt_outs() {
        let conversation = LocalConversationId::new();
        let mut settings = InferenceSettingsStore::default();
        let defaults = crate::mcp_agent_defaults::AgentDefaults::default();
        let enabled = settings.for_conversation_with_mcp_defaults(conversation, &defaults);
        assert!(enabled.mcp_enabled);
        assert!(
            enabled
                .developer_context
                .contains("chatarium.builtin::tools.list")
        );

        let mut disabled_global = defaults.clone();
        disabled_global.seed_new_conversations = false;
        let disabled = settings.for_conversation_with_mcp_defaults(conversation, &disabled_global);
        assert!(!disabled.mcp_enabled);
        assert!(disabled.developer_context.is_empty());

        let mut explicit_off = ConversationInferenceSettings::default();
        explicit_off.developer_context = "deliberately no MCP".to_owned();
        settings.set(conversation, explicit_off.clone()).unwrap();
        assert_eq!(
            settings.for_conversation_with_mcp_defaults(conversation, &defaults),
            explicit_off,
        );
    }

    #[test]
    fn settings_are_isolated_by_local_conversation() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut store = InferenceSettingsStore::default();
        store
            .set(
                first,
                ConversationInferenceSettings {
                    model: Some("gpt-example".to_owned()),
                    instructions: "first instructions".to_owned(),
                    developer_context: "first developer context".to_owned(),
                    mcp_enabled: true,
                    slow_warning_seconds: 20,
                },
            )
            .unwrap();

        assert_eq!(
            store.for_conversation(first).instructions,
            "first instructions"
        );
        assert_eq!(
            store.for_conversation(second),
            ConversationInferenceSettings::default()
        );
    }

    #[test]
    fn settings_round_trip_atomically() {
        let path = temp_path();
        let conversation = LocalConversationId::new();
        let mut store = InferenceSettingsStore::default();
        let expected = ConversationInferenceSettings {
            model: Some("gpt-example".to_owned()),
            instructions: "Keep answers compact.".to_owned(),
            developer_context: "Lifecycle: working".to_owned(),
            mcp_enabled: true,
            slow_warning_seconds: 27,
        };
        store.set(conversation, expected.clone()).unwrap();
        store.save_atomic(&path).unwrap();

        let loaded = InferenceSettingsStore::load(&path).unwrap();
        assert_eq!(loaded.for_conversation(conversation), expected);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn legacy_settings_load_default_warning_and_invalid_values_fail() {
        let path = temp_path();
        let conversation = LocalConversationId::new();
        let mut legacy = json!({
            "schema": SCHEMA,
            "version": VERSION,
            "conversations": {}
        });
        legacy["conversations"].as_object_mut().unwrap().insert(
            conversation.to_string(),
            json!({"model": null, "instructions": "", "developer_context": ""}),
        );
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let loaded = InferenceSettingsStore::load(&path).unwrap();
        assert_eq!(
            loaded.for_conversation(conversation).slow_warning_seconds,
            DEFAULT_SLOW_WARNING_SECONDS
        );

        let mut store = InferenceSettingsStore::default();
        let mut settings = ConversationInferenceSettings::default();
        settings.slow_warning_seconds = 0;
        assert!(store.set(conversation, settings.clone()).is_err());
        settings.slow_warning_seconds = MAX_SLOW_WARNING_SECONDS + 1;
        assert!(store.set(conversation, settings).is_err());
        let _ = fs::remove_file(path);
    }
}
