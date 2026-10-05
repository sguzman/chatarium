//! Mutable local conversation workspace metadata.
//!
//! Conversation message history remains authoritative in the append-only journal. This store owns
//! user-editable workspace metadata that cannot be derived from message events: empty conversations,
//! explicit titles, archive state, ordering timestamps, and the selected local conversation.

use chatarium_core::LocalConversationId;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = "chatarium-local-conversation-catalog";
const VERSION: u64 = 1;
const MAX_TITLE_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalConversationEntry {
    pub id: LocalConversationId,
    pub title: Option<String>,
    pub archived: bool,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalConversationCatalog {
    active: Option<LocalConversationId>,
    entries: BTreeMap<String, LocalConversationEntry>,
}

impl LocalConversationCatalog {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read local conversation catalog: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid local conversation catalog JSON: {error}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(VERSION)
        {
            return Err("unsupported local conversation catalog schema".to_owned());
        }
        let active = value
            .get("active_conversation_id")
            .and_then(Value::as_str)
            .map(LocalConversationId::from_str)
            .transpose()
            .map_err(|error| format!("invalid active local conversation id: {error}"))?;
        let items = value
            .get("conversations")
            .and_then(Value::as_array)
            .ok_or_else(|| "local conversation catalog items missing".to_owned())?;
        let mut entries = BTreeMap::new();
        for (index, raw) in items.iter().enumerate() {
            let object = raw
                .as_object()
                .ok_or_else(|| format!("local conversation item {index} is not an object"))?;
            let id_text = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("local conversation item {index} id missing"))?;
            let id = LocalConversationId::from_str(id_text)
                .map_err(|error| format!("invalid local conversation id: {error}"))?;
            let title = object
                .get("title")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if title
                .as_ref()
                .is_some_and(|title| title.len() > MAX_TITLE_BYTES)
            {
                return Err("local conversation title is too large".to_owned());
            }
            let archived = object
                .get("archived")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let created_at_unix_ms = object
                .get("created_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let updated_at_unix_ms = object
                .get("updated_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or(created_at_unix_ms);
            if entries
                .insert(
                    id.to_string(),
                    LocalConversationEntry {
                        id,
                        title,
                        archived,
                        created_at_unix_ms,
                        updated_at_unix_ms,
                    },
                )
                .is_some()
            {
                return Err("duplicate local conversation id".to_owned());
            }
        }
        if active.is_some_and(|id| !entries.contains_key(&id.to_string())) {
            return Err("active local conversation is absent from catalog".to_owned());
        }
        Ok(Self { active, entries })
    }

    pub fn save_atomic(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create local catalog directory: {error}"))?;
        }
        let conversations = self
            .entries()
            .into_iter()
            .map(|entry| {
                json!({
                    "id": entry.id.to_string(),
                    "title": entry.title,
                    "archived": entry.archived,
                    "created_at_unix_ms": entry.created_at_unix_ms,
                    "updated_at_unix_ms": entry.updated_at_unix_ms,
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": SCHEMA,
            "version": VERSION,
            "active_conversation_id": self.active.map(|id| id.to_string()),
            "conversations": conversations,
        }))
        .map_err(|error| format!("failed to encode local conversation catalog: {error}"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|error| {
            format!("failed to write temporary local conversation catalog: {error}")
        })?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to replace local conversation catalog: {error}"))
    }

    #[must_use]
    pub fn active(&self) -> Option<LocalConversationId> {
        self.active
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ensure(
        &mut self,
        id: LocalConversationId,
        derived_title: Option<String>,
        now_ms: u64,
    ) -> bool {
        let key = id.to_string();
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.title.is_none()
                && derived_title
                    .as_ref()
                    .is_some_and(|title| !title.trim().is_empty())
            {
                entry.title = derived_title;
                return true;
            }
            return false;
        }
        self.entries.insert(
            key,
            LocalConversationEntry {
                id,
                title: derived_title.filter(|title| !title.trim().is_empty()),
                archived: false,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            },
        );
        true
    }

    pub fn create(&mut self, id: LocalConversationId, now_ms: u64) {
        self.entries.insert(
            id.to_string(),
            LocalConversationEntry {
                id,
                title: None,
                archived: false,
                created_at_unix_ms: now_ms,
                updated_at_unix_ms: now_ms,
            },
        );
        self.active = Some(id);
    }

    pub fn set_active(&mut self, id: LocalConversationId, now_ms: u64) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.updated_at_unix_ms = now_ms;
        self.active = Some(id);
        Ok(())
    }

    pub fn rename(
        &mut self,
        id: LocalConversationId,
        title: Option<String>,
        now_ms: u64,
    ) -> Result<(), String> {
        if title
            .as_ref()
            .is_some_and(|title| title.len() > MAX_TITLE_BYTES)
        {
            return Err("local conversation title is too large".to_owned());
        }
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.title = title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty());
        entry.updated_at_unix_ms = now_ms;
        Ok(())
    }

    pub fn set_archived(
        &mut self,
        id: LocalConversationId,
        archived: bool,
        now_ms: u64,
    ) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(&id.to_string())
            .ok_or_else(|| "local conversation is absent from catalog".to_owned())?;
        entry.archived = archived;
        entry.updated_at_unix_ms = now_ms;
        if archived && self.active == Some(id) {
            self.active = None;
        }
        Ok(())
    }

    #[must_use]
    pub fn entry(&self, id: LocalConversationId) -> Option<&LocalConversationEntry> {
        self.entries.get(&id.to_string())
    }

    #[must_use]
    pub fn entries(&self) -> Vec<LocalConversationEntry> {
        let mut entries = self.entries.values().cloned().collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| left.id.to_string().cmp(&right.id.to_string()))
        });
        entries
    }

    #[must_use]
    pub fn first_unarchived(&self) -> Option<LocalConversationId> {
        self.entries()
            .into_iter()
            .find(|entry| !entry.archived)
            .map(|entry| entry.id)
    }
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
            "chatarium-local-conversations-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn create_switch_rename_archive_and_restart() {
        let path = temp_path();
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        catalog.rename(first, Some("First".to_owned()), 3).unwrap();
        catalog.set_active(first, 4).unwrap();
        catalog.set_archived(second, true, 5).unwrap();
        catalog.save_atomic(&path).unwrap();

        let loaded = LocalConversationCatalog::load(&path).unwrap();
        assert_eq!(loaded.active(), Some(first));
        assert_eq!(
            loaded.entry(first).and_then(|entry| entry.title.as_deref()),
            Some("First")
        );
        assert!(loaded.entry(second).is_some_and(|entry| entry.archived));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn conversations_remain_isolated_by_identity() {
        let first = LocalConversationId::new();
        let second = LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        catalog.rename(first, Some("Alpha".to_owned()), 3).unwrap();

        assert_eq!(
            catalog
                .entry(first)
                .and_then(|entry| entry.title.as_deref()),
            Some("Alpha")
        );
        assert_eq!(
            catalog
                .entry(second)
                .and_then(|entry| entry.title.as_deref()),
            None
        );
    }
}
