//! Import ChatGPT account-export conversation arrays as archival snapshots.
//!
//! The account export is treated as historical evidence. Remote conversation identifiers are
//! preserved for future correlation, but import alone never proves live read/write authority.

use chatarium_core::LocalConversationId;
use chatarium_store::historical_conversation_audit::{
    HistoricalConversationSnapshot, record_historical_conversation_snapshot,
    replay_historical_conversation_snapshots,
};
use chatarium_store::{EventStore, JsonlEventStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Result of one account-export import.
#[derive(Debug)]
pub struct ImportSummary {
    pub source_sha256: String,
    pub source_archive: PathBuf,
    pub journal_path: PathBuf,
    pub conversations_seen: usize,
    pub appended_snapshots: usize,
    pub unchanged_snapshots: usize,
}

/// Import one extracted ChatGPT conversation JSON array.
pub fn import_file(export_path: &Path, data_dir: &Path) -> Result<ImportSummary, String> {
    let bytes = fs::read(export_path)
        .map_err(|error| format!("read {}: {error}", export_path.display()))?;
    import_bytes(&bytes, data_dir)
}

fn import_bytes(bytes: &[u8], data_dir: &Path) -> Result<ImportSummary, String> {
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse account export JSON: {error}"))?;
    let conversations = root.as_array().ok_or_else(|| {
        "account export conversation file must be a top-level JSON array".to_owned()
    })?;

    let source_sha256 = sha256_hex(bytes);
    let source_archive = archive_bytes(
        &data_dir
            .join("imports")
            .join("openai-account-export")
            .join("sources"),
        &source_sha256,
        bytes,
    )?;
    let journal_path = data_dir.join("journal.jsonl");
    let mut store = JsonlEventStore::open(&journal_path)
        .map_err(|error| format!("open {}: {error}", journal_path.display()))?;
    let existing = replay_historical_conversation_snapshots(store.events())
        .map_err(|error| format!("replay historical conversation archive: {error}"))?;

    let mut local_by_remote = BTreeMap::<String, LocalConversationId>::new();
    let mut known_snapshots = BTreeSet::<(String, String)>::new();
    for record in &existing {
        local_by_remote.insert(
            record.snapshot.remote_conversation_id.clone(),
            record.snapshot.local_conversation_id,
        );
        known_snapshots.insert((
            record.snapshot.remote_conversation_id.clone(),
            record.snapshot.conversation_sha256.clone(),
        ));
    }

    let mut seen_remote = BTreeSet::new();
    let mut appended_snapshots = 0_usize;
    let mut unchanged_snapshots = 0_usize;

    for (index, conversation) in conversations.iter().enumerate() {
        let object = conversation
            .as_object()
            .ok_or_else(|| format!("conversation at index {index} must be an object"))?;
        let remote_conversation_id = conversation_identity(conversation, index)?;
        if !seen_remote.insert(remote_conversation_id.clone()) {
            return Err(format!(
                "account export contains duplicate conversation identity {:?}",
                remote_conversation_id
            ));
        }

        let canonical = serde_json::to_vec(conversation)
            .map_err(|error| format!("serialize conversation {index}: {error}"))?;
        let conversation_sha256 = sha256_hex(&canonical);
        let conversation_archive = archive_bytes(
            &data_dir
                .join("imports")
                .join("openai-account-export")
                .join("conversations"),
            &conversation_sha256,
            &canonical,
        )?;

        if known_snapshots.contains(&(remote_conversation_id.clone(), conversation_sha256.clone()))
        {
            unchanged_snapshots += 1;
            continue;
        }

        let local_conversation_id = local_by_remote
            .get(&remote_conversation_id)
            .copied()
            .unwrap_or_else(|| {
                let local = LocalConversationId::new();
                local_by_remote.insert(remote_conversation_id.clone(), local);
                local
            });

        let source_index = u64::try_from(index)
            .map_err(|_| format!("conversation index {index} exceeds durable integer range"))?;
        let snapshot = HistoricalConversationSnapshot {
            local_conversation_id,
            remote_conversation_id: remote_conversation_id.clone(),
            source_sha256: source_sha256.clone(),
            source_archive: relative_archive_path(data_dir, &source_archive),
            conversation_sha256: conversation_sha256.clone(),
            conversation_archive: relative_archive_path(data_dir, &conversation_archive),
            source_index,
            title: optional_string(object.get("title"), index, "title")?,
            create_time: optional_number(object.get("create_time"), index, "create_time")?,
            update_time: optional_number(object.get("update_time"), index, "update_time")?,
            current_node: optional_string(object.get("current_node"), index, "current_node")?,
        };
        record_historical_conversation_snapshot(&mut store, &snapshot).map_err(|error| {
            format!(
                "append historical conversation snapshot {:?}: {error}",
                remote_conversation_id
            )
        })?;
        known_snapshots.insert((remote_conversation_id, conversation_sha256));
        appended_snapshots += 1;
    }

    Ok(ImportSummary {
        source_sha256,
        source_archive,
        journal_path,
        conversations_seen: conversations.len(),
        appended_snapshots,
        unchanged_snapshots,
    })
}

fn conversation_identity(conversation: &Value, index: usize) -> Result<String, String> {
    let conversation_id = conversation.get("conversation_id").and_then(Value::as_str);
    let id = conversation.get("id").and_then(Value::as_str);

    let selected = match (conversation_id, id) {
        (Some(left), Some(right)) if left != right => {
            return Err(format!(
                "conversation at index {index} has conflicting conversation_id {left:?} and id {right:?}"
            ));
        }
        (Some(value), _) | (_, Some(value)) => value,
        (None, None) => {
            return Err(format!(
                "conversation at index {index} has neither string conversation_id nor string id"
            ));
        }
    };
    if selected.is_empty() {
        return Err(format!(
            "conversation at index {index} has an empty identity"
        ));
    }
    Ok(selected.to_owned())
}

fn optional_string(
    value: Option<&Value>,
    index: usize,
    field: &str,
) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!(
            "conversation at index {index} field {field:?} must be string or null"
        )),
    }
}

fn optional_number(
    value: Option<&Value>,
    index: usize,
    field: &str,
) -> Result<Option<f64>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_f64().map(Some).ok_or_else(|| {
            format!("conversation at index {index} field {field:?} must be number or null")
        }),
    }
}

fn archive_bytes(directory: &Path, sha256: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let path = directory.join(format!("{sha256}.json"));

    if path.exists() {
        let existing = fs::read(&path)
            .map_err(|error| format!("read existing archive {}: {error}", path.display()))?;
        if sha256_hex(&existing) != sha256 {
            return Err(format!(
                "existing archive {} does not match its content-addressed filename",
                path.display()
            ));
        }
        return Ok(path);
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("create archive {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write archive {}: {error}", path.display()))?;
    file.flush()
        .map_err(|error| format!("flush archive {}: {error}", path.display()))?;
    file.sync_data()
        .map_err(|error| format!("sync archive {}: {error}", path.display()))?;
    Ok(path)
}

fn relative_archive_path(data_dir: &Path, path: &Path) -> String {
    path.strip_prefix(data_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_store::historical_conversation_audit::replay_historical_conversation_snapshots;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-account-export-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn sample_export(first_title: &str) -> Vec<u8> {
        serde_json::to_vec(&json!([
            {
                "id": "remote-a",
                "conversation_id": "remote-a",
                "title": first_title,
                "create_time": 10.0,
                "update_time": 20.0,
                "current_node": "assistant-b",
                "mapping": {
                    "root": {"id": "root", "message": null, "parent": null, "children": ["user-a"]},
                    "user-a": {"id": "user-a", "message": {"id": "user-a", "author": {"role": "user"}, "content": {"content_type": "text", "parts": ["hello"]}}, "parent": "root", "children": ["assistant-a", "assistant-b"]},
                    "assistant-a": {"id": "assistant-a", "message": {"id": "assistant-a", "author": {"role": "assistant"}, "content": {"content_type": "text", "parts": ["branch a"]}}, "parent": "user-a", "children": []},
                    "assistant-b": {"id": "assistant-b", "message": {"id": "assistant-b", "author": {"role": "assistant"}, "content": {"content_type": "text", "parts": ["branch b"]}}, "parent": "user-a", "children": []}
                }
            },
            {
                "id": "legacy-remote-b",
                "title": "legacy",
                "mapping": {},
                "current_node": null
            }
        ]))
        .expect("sample export")
    }

    #[test]
    fn import_preserves_raw_branching_and_is_idempotent() {
        let dir = temp_dir("idempotent");
        let bytes = sample_export("branched");
        let first = import_bytes(&bytes, &dir).expect("first import");
        assert_eq!(first.conversations_seen, 2);
        assert_eq!(first.appended_snapshots, 2);
        assert_eq!(first.unchanged_snapshots, 0);

        let second = import_bytes(&bytes, &dir).expect("second import");
        assert_eq!(second.appended_snapshots, 0);
        assert_eq!(second.unchanged_snapshots, 2);

        let store = JsonlEventStore::open(dir.join("journal.jsonl")).expect("journal");
        let records = replay_historical_conversation_snapshots(store.events()).expect("replay");
        assert_eq!(records.len(), 2);
        let first_record = records
            .iter()
            .find(|record| record.snapshot.remote_conversation_id == "remote-a")
            .expect("remote-a");
        let raw = fs::read_to_string(dir.join(&first_record.snapshot.conversation_archive))
            .expect("snapshot archive");
        let value: Value = serde_json::from_str(&raw).expect("snapshot json");
        assert_eq!(
            value
                .pointer("/mapping/user-a/children")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn changed_export_keeps_local_identity_and_appends_new_snapshot() {
        let dir = temp_dir("changed");
        import_bytes(&sample_export("first"), &dir).expect("first");
        let first_store = JsonlEventStore::open(dir.join("journal.jsonl")).expect("first journal");
        let first_records =
            replay_historical_conversation_snapshots(first_store.events()).expect("first replay");
        let original_local = first_records
            .iter()
            .find(|record| record.snapshot.remote_conversation_id == "remote-a")
            .expect("remote-a first")
            .snapshot
            .local_conversation_id;
        drop(first_store);

        let second = import_bytes(&sample_export("changed"), &dir).expect("second");
        assert_eq!(second.appended_snapshots, 1);
        assert_eq!(second.unchanged_snapshots, 1);

        let store = JsonlEventStore::open(dir.join("journal.jsonl")).expect("journal");
        let records = replay_historical_conversation_snapshots(store.events()).expect("replay");
        let remote_a = records
            .iter()
            .filter(|record| record.snapshot.remote_conversation_id == "remote-a")
            .collect::<Vec<_>>();
        assert_eq!(remote_a.len(), 2);
        assert!(
            remote_a
                .iter()
                .all(|record| record.snapshot.local_conversation_id == original_local)
        );
        assert_eq!(
            remote_a
                .last()
                .and_then(|record| record.snapshot.title.as_deref()),
            Some("changed")
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn conflicting_export_identities_are_rejected() {
        let dir = temp_dir("conflict");
        let bytes = serde_json::to_vec(&json!([{
            "id": "one",
            "conversation_id": "two",
            "mapping": {}
        }]))
        .unwrap();
        let error = import_bytes(&bytes, &dir).expect_err("must reject ambiguity");
        assert!(error.contains("conflicting conversation_id"));
        let _ = fs::remove_dir_all(dir);
    }
}
