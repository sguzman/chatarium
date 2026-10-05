//! Local archive integrity, backup, verification, and fail-closed restore.
//!
//! The journal is authoritative. The history cache is copied as a convenience projection and
//! is validated structurally; neither backup manifests nor reports contain private identities,
//! titles, message text, or raw response bodies.

use crate::remote_identity_audit::replay_remote_identity_audit;
use crate::remote_mirror_queue::{RemoteMirrorQueueStatus, derive_remote_mirror_queue};
use crate::remote_mirror_selection_audit::replay_remote_mirror_selection_audit;
use crate::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use crate::remote_mirror_transcript::project_remote_active_transcript;
use crate::remote_read_audit::replay_remote_read_audit;
use crate::{EventEnvelope, inspect_jsonl_journal};
use chatarium_protocol::conversation_list::ConversationListItem;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FILE: &str = "remote-history-cache.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_SCHEMA: &str = "chatarium-local-archive-backup";
const MANIFEST_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIntegrityReport {
    pub healthy: bool,
    pub warnings: usize,
    pub errors: usize,
    pub journal_event_count: usize,
    pub highest_sequence: u64,
    pub catalog_count: usize,
    pub snapshot_count: usize,
    pub queue_full_count: usize,
    pub queue_partial_count: usize,
    pub queue_pending_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupVerification {
    pub valid: bool,
    pub manifest_files: usize,
    pub archive: ArchiveIntegrityReport,
}

#[derive(Debug)]
pub struct ArchiveMaintenanceError(String);

impl std::fmt::Display for ArchiveMaintenanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ArchiveMaintenanceError {}
impl From<io::Error> for ArchiveMaintenanceError {
    fn from(value: io::Error) -> Self {
        Self(value.to_string())
    }
}

fn err(message: impl Into<String>) -> ArchiveMaintenanceError {
    ArchiveMaintenanceError(message.into())
}

fn read_catalog(path: &Path) -> Result<Vec<ConversationListItem>, ArchiveMaintenanceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_slice(&fs::read(path)?).map_err(|e| err(format!("cache JSON: {e}")))?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-remote-history-cache")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(err("unsupported history cache schema"));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| err("history cache items missing"))?;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| err(format!("history cache item {index} is not an object")))?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| err(format!("history cache item {index} identity missing")))?;
            Ok(ConversationListItem {
                id: id.to_owned(),
                title: None,
                create_time: object.get("create_time").cloned(),
                update_time: object.get("update_time").cloned(),
            })
        })
        .collect()
}

fn audit_paths(
    data_dir: &Path,
) -> Result<(Vec<EventEnvelope>, Vec<ConversationListItem>, u64), ArchiveMaintenanceError> {
    let journal = data_dir.join(JOURNAL_FILE);
    let inspection = if journal.exists() {
        inspect_jsonl_journal(&journal)?
    } else {
        crate::JournalInspection {
            events: Vec::new(),
            byte_len: 0,
            complete_byte_len: 0,
            unterminated_tail_bytes: 0,
        }
    };
    let catalog = read_catalog(&data_dir.join(CACHE_FILE))?;
    Ok((
        inspection.events,
        catalog,
        inspection.unterminated_tail_bytes,
    ))
}

/// Audit one local data directory, without writing to it.
pub fn check_archive(
    data_dir: impl AsRef<Path>,
) -> Result<ArchiveIntegrityReport, ArchiveMaintenanceError> {
    let data_dir = data_dir.as_ref();
    let (events, catalog, tail) = audit_paths(data_dir)?;
    let catalog_ids = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    let identities = replay_remote_identity_audit(&events).map_err(err)?;
    replay_remote_mirror_selection_audit(&events).map_err(err)?;
    replay_remote_read_audit(&events).map_err(err)?;
    let snapshots = replay_remote_conversation_snapshot_audit(&events).map_err(err)?;
    for snapshot in &snapshots {
        project_remote_active_transcript(&snapshot.envelope).map_err(err)?;
    }
    let queue = derive_remote_mirror_queue(&catalog_ids, &events).map_err(err)?;
    let catalog_id_set = catalog_ids
        .iter()
        .map(|(_, id)| id.as_str())
        .collect::<BTreeSet<_>>();
    if identities.iter().any(|identity| {
        !catalog_id_set.contains(identity.binding.remote_conversation_id().as_str())
    }) {
        return Err(err(
            "durable remote identity is absent from the cached catalog",
        ));
    }
    if snapshots
        .iter()
        .any(|snapshot| !catalog_id_set.contains(snapshot.remote_conversation_id.as_str()))
    {
        return Err(err("durable snapshot is absent from the cached catalog"));
    }
    Ok(ArchiveIntegrityReport {
        healthy: true,
        warnings: usize::from(tail > 0),
        errors: 0,
        journal_event_count: events.len(),
        highest_sequence: events.last().map_or(0, |event| event.sequence),
        catalog_count: catalog.len(),
        snapshot_count: snapshots.len(),
        queue_full_count: queue.full_count(),
        queue_partial_count: queue.partial_count(),
        queue_pending_count: queue.pending_count(),
    })
}

fn sha256(path: &Path) -> Result<(u64, String), ArchiveMaintenanceError> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok((bytes.len() as u64, format!("{:x}", hasher.finalize())))
}

fn safe_relative(name: &str) -> Result<&Path, ArchiveMaintenanceError> {
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(err("manifest contains unsafe path"));
    }
    Ok(path)
}

/// Create an inspectable directory backup. Existing output is never overwritten.
pub fn create_backup(
    data_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<BackupVerification, ArchiveMaintenanceError> {
    let data_dir = data_dir.as_ref();
    let output = output.as_ref();
    if output.exists() {
        return Err(err("backup output already exists"));
    }
    let archive = check_archive(data_dir)?;
    let temporary = output.with_extension(format!("tmp-{}", std::process::id()));
    if temporary.exists() {
        return Err(err("temporary backup path already exists"));
    }
    fs::create_dir_all(&temporary)?;
    let mut files = Vec::new();
    for name in [JOURNAL_FILE, CACHE_FILE] {
        let source = data_dir.join(name);
        if source.exists() {
            fs::copy(&source, temporary.join(name))?;
            let (size, hash) = sha256(&source)?;
            files.push(json!({"path": name, "size_bytes": size, "sha256": hash}));
        }
    }
    if !temporary.join(JOURNAL_FILE).exists() {
        fs::write(temporary.join(JOURNAL_FILE), [])?;
        let (size, hash) = sha256(&temporary.join(JOURNAL_FILE))?;
        files.push(json!({"path": JOURNAL_FILE, "size_bytes": size, "sha256": hash}));
    }
    let manifest = json!({"schema": MANIFEST_SCHEMA, "version": MANIFEST_VERSION, "files": files, "archive": {"journal_event_count": archive.journal_event_count, "highest_sequence": archive.highest_sequence, "catalog_count": archive.catalog_count, "snapshot_count": archive.snapshot_count}});
    fs::write(
        temporary.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest).map_err(|e| err(e.to_string()))?,
    )?;
    fs::rename(&temporary, output)?;
    verify_backup(output)
}

/// Verify manifest hashes and then run the same archive audit against the package contents.
pub fn verify_backup(
    backup: impl AsRef<Path>,
) -> Result<BackupVerification, ArchiveMaintenanceError> {
    let backup = backup.as_ref();
    let manifest: Value = serde_json::from_slice(&fs::read(backup.join(MANIFEST_FILE))?)
        .map_err(|e| err(format!("manifest JSON: {e}")))?;
    if manifest.get("schema").and_then(Value::as_str) != Some(MANIFEST_SCHEMA)
        || manifest.get("version").and_then(Value::as_u64) != Some(MANIFEST_VERSION)
    {
        return Err(err("unsupported backup manifest"));
    }
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| err("manifest files missing"))?;
    for file in files {
        let name = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| err("manifest path missing"))?;
        let relative = safe_relative(name)?;
        let path = backup.join(relative);
        let (size, hash) = sha256(&path)?;
        if file.get("size_bytes").and_then(Value::as_u64) != Some(size)
            || file.get("sha256").and_then(Value::as_str) != Some(hash.as_str())
        {
            return Err(err("backup file hash or size mismatch"));
        }
    }
    let archive = check_archive(backup)?;
    Ok(BackupVerification {
        valid: true,
        manifest_files: files.len(),
        archive,
    })
}

/// Verify a package in isolation, then atomically replace the target while retaining a safety copy.
pub fn restore_backup(
    backup: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<BackupVerification, ArchiveMaintenanceError> {
    let backup = backup.as_ref();
    let target = target.as_ref();
    let verification = verify_backup(backup)?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".chatarium-restore-{}", std::process::id()));
    if staging.exists() {
        return Err(err("restore staging path already exists"));
    }
    fs::create_dir_all(&staging)?;
    for name in [JOURNAL_FILE, CACHE_FILE] {
        let source = backup.join(name);
        if source.exists() {
            fs::copy(source, staging.join(name))?;
        }
    }
    check_archive(&staging)?;
    if target.exists() {
        let safety = parent.join(format!(".chatarium-pre-restore-{}", unix_ms()));
        fs::rename(target, safety)?;
    }
    fs::rename(&staging, target)?;
    Ok(verification)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventStore, JsonlEventStore};
    use chatarium_core::EventKind;

    fn temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("chatarium-archive-{label}-{}", std::process::id()))
    }

    #[test]
    fn empty_archive_can_be_backed_up_and_verified() {
        let root = temp("empty");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut store = JsonlEventStore::open(root.join(JOURNAL_FILE)).unwrap();
        store
            .append(EventKind::DraftChanged, "local".into())
            .unwrap();
        let backup = root.join("backup");
        let result = create_backup(&root, &backup).unwrap();
        assert!(result.valid);
        assert_eq!(verify_backup(&backup).unwrap().manifest_files, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupted_backup_is_rejected_without_restore() {
        let root = temp("corrupt");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut store = JsonlEventStore::open(root.join(JOURNAL_FILE)).unwrap();
        store
            .append(EventKind::DraftChanged, "local".into())
            .unwrap();
        let backup = root.join("backup");
        create_backup(&root, &backup).unwrap();
        fs::write(backup.join(JOURNAL_FILE), b"bad\n").unwrap();
        assert!(verify_backup(&backup).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
