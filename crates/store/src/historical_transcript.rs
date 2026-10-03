//! Read-only projection of historical ChatGPT account-export snapshots.
//!
//! Historical snapshots preserve remote identity for correlation, but they are not live remote
//! bindings. This module verifies the content-addressed archive and projects only the exported
//! active branch selected by `current_node`.

use crate::EventEnvelope;
use crate::historical_conversation_audit::{
    HistoricalConversationSnapshot, replay_historical_conversation_snapshots,
};
use chatarium_core::LocalConversationId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

/// One latest historical conversation snapshot available for read-only browsing.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalConversationCatalogEntry {
    /// Stable Chatarium-local identity for this imported lineage.
    pub local_conversation_id: LocalConversationId,
    /// Exported ChatGPT conversation identity.
    pub remote_conversation_id: String,
    /// Exported title when present.
    pub title: Option<String>,
    /// Exported creation timestamp when present.
    pub create_time: Option<f64>,
    /// Exported update timestamp when present.
    pub update_time: Option<f64>,
    /// Exported active leaf.
    pub current_node: Option<String>,
    /// Content hash of the archived raw conversation object.
    pub conversation_sha256: String,
    /// Data-directory-relative path to the raw archived conversation object.
    pub conversation_archive: String,
    /// Durable journal sequence of the selected latest snapshot.
    pub imported_sequence: u64,
}

/// User-visible role in a historical transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoricalTranscriptRole {
    /// User-authored message.
    User,
    /// Assistant-authored message.
    Assistant,
}

/// One user-visible message on the exported active branch.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalTranscriptMessage {
    /// Exported message identity, falling back to the mapping-node identity.
    pub remote_message_id: String,
    /// Visible user/assistant role.
    pub role: HistoricalTranscriptRole,
    /// Best-effort textual representation of the preserved message content.
    pub text: String,
    /// Exported creation timestamp when present.
    pub create_time: Option<f64>,
}

/// Return the newest imported snapshot for every historical conversation lineage.
///
/// Newness is the durable import sequence, not an inferred remote timestamp.
pub fn latest_historical_conversation_catalog(
    events: &[EventEnvelope],
) -> Result<Vec<HistoricalConversationCatalogEntry>, String> {
    let records = replay_historical_conversation_snapshots(events)?;
    let mut latest = BTreeMap::<String, HistoricalConversationCatalogEntry>::new();

    for record in records {
        let snapshot = record.snapshot;
        latest.insert(
            snapshot.remote_conversation_id.clone(),
            catalog_entry(snapshot, record.imported_sequence),
        );
    }

    let mut entries = latest.into_values().collect::<Vec<_>>();
    entries.sort_by(|left, right| right.imported_sequence.cmp(&left.imported_sequence));
    Ok(entries)
}

/// Verify and load the archived raw conversation object, then project its active branch.
pub fn load_historical_active_transcript(
    data_dir: &Path,
    entry: &HistoricalConversationCatalogEntry,
) -> Result<Vec<HistoricalTranscriptMessage>, String> {
    let relative = checked_relative_archive_path(&entry.conversation_archive)?;
    let path = data_dir.join(relative);
    let bytes = fs::read(&path)
        .map_err(|error| format!("read historical archive {}: {error}", path.display()))?;
    let observed_sha256 = sha256_hex(&bytes);
    if observed_sha256 != entry.conversation_sha256 {
        return Err(format!(
            "historical archive hash mismatch for {}: journal records {}, file hashes to {}",
            path.display(),
            entry.conversation_sha256,
            observed_sha256
        ));
    }

    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse historical archive {}: {error}", path.display()))?;
    project_historical_active_transcript(&raw)
}

/// Project the exported active branch by walking `current_node -> parent -> ... -> root`.
///
/// Sibling branches remain preserved in the raw archive but are not mixed into this projection.
pub fn project_historical_active_transcript(
    conversation: &Value,
) -> Result<Vec<HistoricalTranscriptMessage>, String> {
    let mapping = conversation
        .get("mapping")
        .and_then(Value::as_object)
        .ok_or_else(|| "historical conversation has no object mapping".to_owned())?;
    let mut current = conversation
        .get("current_node")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            "historical conversation has no current_node; refusing to guess an active branch"
                .to_owned()
        })?;

    let mut seen = BTreeSet::<String>::new();
    let mut path = Vec::<(&str, &Value)>::new();

    loop {
        if !seen.insert(current.clone()) {
            return Err(format!(
                "historical conversation active branch contains a parent cycle at {current:?}"
            ));
        }
        let node = mapping.get(&current).ok_or_else(|| {
            format!("historical conversation active branch references missing node {current:?}")
        })?;
        path.push((current.as_str(), node));

        match node.get("parent") {
            None | Some(Value::Null) => break,
            Some(Value::String(parent)) if !parent.is_empty() => {
                current = parent.clone();
            }
            Some(Value::String(_)) => {
                return Err("historical conversation contains an empty parent identity".to_owned());
            }
            Some(_) => {
                return Err(format!(
                    "historical conversation node {:?} has non-string parent",
                    path.last().map(|(id, _)| *id).unwrap_or_default()
                ));
            }
        }
    }

    path.reverse();
    let mut transcript = Vec::new();
    for (node_id, node) in path {
        let Some(message) = node.get("message") else {
            continue;
        };
        if message.is_null() {
            continue;
        }
        let Some(message) = message.as_object() else {
            return Err(format!(
                "historical conversation node {node_id:?} has non-object message"
            ));
        };

        if message.get("weight").and_then(Value::as_f64) == Some(0.0)
            || message
                .get("metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("is_visually_hidden_from_conversation"))
                .and_then(Value::as_bool)
                == Some(true)
        {
            continue;
        }

        let role = match message
            .get("author")
            .and_then(Value::as_object)
            .and_then(|author| author.get("role"))
            .and_then(Value::as_str)
        {
            Some("user") => HistoricalTranscriptRole::User,
            Some("assistant") => HistoricalTranscriptRole::Assistant,
            _ => continue,
        };

        let text = message
            .get("content")
            .map(project_content_text)
            .transpose()?
            .unwrap_or_default();
        if text.trim().is_empty() {
            continue;
        }

        let remote_message_id = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(node_id)
            .to_owned();
        let create_time = optional_number(message.get("create_time"), "message.create_time")?;

        transcript.push(HistoricalTranscriptMessage {
            remote_message_id,
            role,
            text,
            create_time,
        });
    }

    Ok(transcript)
}

fn catalog_entry(
    snapshot: HistoricalConversationSnapshot,
    imported_sequence: u64,
) -> HistoricalConversationCatalogEntry {
    HistoricalConversationCatalogEntry {
        local_conversation_id: snapshot.local_conversation_id,
        remote_conversation_id: snapshot.remote_conversation_id,
        title: snapshot.title,
        create_time: snapshot.create_time,
        update_time: snapshot.update_time,
        current_node: snapshot.current_node,
        conversation_sha256: snapshot.conversation_sha256,
        conversation_archive: snapshot.conversation_archive,
        imported_sequence,
    }
}

fn project_content_text(content: &Value) -> Result<String, String> {
    let Some(content) = content.as_object() else {
        return Err("historical message content must be an object".to_owned());
    };

    if let Some(parts) = content.get("parts") {
        let parts = parts
            .as_array()
            .ok_or_else(|| "historical message content.parts must be an array".to_owned())?;
        let mut projected = Vec::new();
        for part in parts {
            match part {
                Value::String(text) => {
                    if !text.is_empty() {
                        projected.push(text.clone());
                    }
                }
                Value::Object(object) => {
                    if let Some(text) = object
                        .get("text")
                        .and_then(Value::as_str)
                        .or_else(|| object.get("content").and_then(Value::as_str))
                    {
                        if !text.is_empty() {
                            projected.push(text.to_owned());
                        }
                    } else {
                        let content_type = object
                            .get("content_type")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        projected.push(format!("[non-text content: {content_type}]"));
                    }
                }
                Value::Null => {}
                _ => projected.push("[unsupported content part]".to_owned()),
            }
        }
        return Ok(projected.join("\n"));
    }

    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Ok(text.to_owned());
    }

    let content_type = content
        .get("content_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(format!("[non-text content: {content_type}]"))
}

fn checked_relative_archive_path(raw: &str) -> Result<&Path, String> {
    let path = Path::new(raw);
    if raw.is_empty() || path.is_absolute() {
        return Err(format!(
            "historical archive path must be non-empty and relative: {raw:?}"
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "historical archive path escapes the data directory: {raw:?}"
        ));
    }
    Ok(path)
}

fn optional_number(value: Option<&Value>, field: &str) -> Result<Option<f64>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("historical {field} must be number or null")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventStore;
    use crate::historical_conversation_audit::record_historical_conversation_snapshot;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn branched_conversation() -> Value {
        json!({
            "id": "remote-a",
            "title": "Branched",
            "current_node": "assistant-active",
            "mapping": {
                "root": {
                    "id": "root",
                    "message": null,
                    "parent": null,
                    "children": ["user"]
                },
                "user": {
                    "id": "user",
                    "message": {
                        "id": "user-message",
                        "author": {"role": "user"},
                        "content": {"content_type": "text", "parts": ["hello"]}
                    },
                    "parent": "root",
                    "children": ["assistant-abandoned", "assistant-active"]
                },
                "assistant-abandoned": {
                    "id": "assistant-abandoned",
                    "message": {
                        "id": "assistant-abandoned-message",
                        "author": {"role": "assistant"},
                        "content": {"content_type": "text", "parts": ["wrong branch"]}
                    },
                    "parent": "user",
                    "children": []
                },
                "assistant-active": {
                    "id": "assistant-active",
                    "message": {
                        "id": "assistant-active-message",
                        "author": {"role": "assistant"},
                        "content": {
                            "content_type": "multimodal_text",
                            "parts": [
                                "active answer",
                                {"content_type": "image_asset_pointer", "asset_pointer": "file-service://x"}
                            ]
                        }
                    },
                    "parent": "user",
                    "children": []
                }
            }
        })
    }

    #[test]
    fn active_branch_projection_excludes_abandoned_sibling() {
        let transcript = project_historical_active_transcript(&branched_conversation()).unwrap();
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].role, HistoricalTranscriptRole::User);
        assert_eq!(transcript[0].text, "hello");
        assert_eq!(transcript[1].role, HistoricalTranscriptRole::Assistant);
        assert!(transcript[1].text.contains("active answer"));
        assert!(transcript[1].text.contains("[non-text content: image_asset_pointer]"));
        assert!(!transcript.iter().any(|message| message.text.contains("wrong branch")));
    }

    #[test]
    fn active_branch_projection_rejects_missing_node_and_cycle() {
        let mut missing = branched_conversation();
        missing["current_node"] = Value::String("missing".to_owned());
        assert!(
            project_historical_active_transcript(&missing)
                .unwrap_err()
                .contains("missing node")
        );

        let cycle = json!({
            "current_node": "a",
            "mapping": {
                "a": {"message": null, "parent": "b", "children": []},
                "b": {"message": null, "parent": "a", "children": []}
            }
        });
        assert!(
            project_historical_active_transcript(&cycle)
                .unwrap_err()
                .contains("parent cycle")
        );
    }

    #[test]
    fn catalog_selects_latest_snapshot_for_remote_lineage() {
        let local = LocalConversationId::new();
        let mut store = MemoryEventStore::default();
        for (title, digest) in [("old", "one"), ("new", "two")] {
            record_historical_conversation_snapshot(
                &mut store,
                &HistoricalConversationSnapshot {
                    local_conversation_id: local,
                    remote_conversation_id: "remote-a".to_owned(),
                    source_sha256: "source".to_owned(),
                    source_archive: "imports/openai-account-export/sources/source.json".to_owned(),
                    conversation_sha256: digest.to_owned(),
                    conversation_archive: format!(
                        "imports/openai-account-export/conversations/{digest}.json"
                    ),
                    source_index: 0,
                    title: Some(title.to_owned()),
                    create_time: None,
                    update_time: None,
                    current_node: Some("leaf".to_owned()),
                },
            )
            .unwrap();
        }

        let catalog = latest_historical_conversation_catalog(store.events()).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].title.as_deref(), Some("new"));
        assert_eq!(catalog[0].local_conversation_id, local);
    }

    #[test]
    fn archive_load_verifies_hash_and_rejects_path_escape() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-historical-transcript-{}-{nonce}",
            std::process::id()
        ));
        let archive_dir = dir.join("imports/openai-account-export/conversations");
        fs::create_dir_all(&archive_dir).unwrap();
        let bytes = serde_json::to_vec(&branched_conversation()).unwrap();
        let digest = sha256_hex(&bytes);
        let relative = format!("imports/openai-account-export/conversations/{digest}.json");
        fs::write(dir.join(&relative), &bytes).unwrap();

        let entry = HistoricalConversationCatalogEntry {
            local_conversation_id: LocalConversationId::new(),
            remote_conversation_id: "remote-a".to_owned(),
            title: Some("Branched".to_owned()),
            create_time: None,
            update_time: None,
            current_node: Some("assistant-active".to_owned()),
            conversation_sha256: digest,
            conversation_archive: relative,
            imported_sequence: 1,
        };
        let transcript = load_historical_active_transcript(&dir, &entry).unwrap();
        assert_eq!(transcript.len(), 2);

        fs::write(
            dir.join(&entry.conversation_archive),
            br#"{"tampered":true}"#,
        )
        .unwrap();
        assert!(
            load_historical_active_transcript(&dir, &entry)
                .unwrap_err()
                .contains("hash mismatch")
        );

        let mut escaped = entry;
        escaped.conversation_archive = "../outside.json".to_owned();
        assert!(
            load_historical_active_transcript(&dir, &escaped)
                .unwrap_err()
                .contains("escapes the data directory")
        );

        let _ = fs::remove_dir_all(dir);
    }
}
