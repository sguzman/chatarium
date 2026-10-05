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
const MAX_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationInferenceSettings {
    pub model: Option<String>,
    pub instructions: String,
    pub developer_context: String,
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
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read inference settings {}: {error}", path.display()))?;
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
            validate(&ConversationInferenceSettings {
                model: model.clone(),
                instructions: instructions.clone(),
                developer_context: developer_context.clone(),
            })?;
            entries.insert(
                conversation_id.clone(),
                ConversationInferenceSettings {
                    model,
                    instructions,
                    developer_context,
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
        };
        store.set(conversation, expected.clone()).unwrap();
        store.save_atomic(&path).unwrap();

        let loaded = InferenceSettingsStore::load(&path).unwrap();
        assert_eq!(loaded.for_conversation(conversation), expected);

        let _ = fs::remove_file(path);
    }
}
