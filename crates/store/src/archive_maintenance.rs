//! Local archive integrity, backup, verification, and fail-closed restore.
//!
//! The journal is authoritative. The history cache is copied as a convenience projection and
//! is validated structurally; neither backup manifests nor reports contain private identities,
//! titles, message text, or raw response bodies.

use crate::remote_health::RemoteHealthController;
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
const INFERENCE_SETTINGS_FILE: &str = "local-inference-settings.json";
const BEHAVIOR_PROFILES_FILE: &str = "behavior-profiles.json";
const LOCAL_CONVERSATION_CATALOG_FILE: &str = "local-conversations.json";
const CAPABILITY_PROBE_REPORT_FILE: &str = "siwc-capability-probes.json";
const LOCAL_INFERENCE_CONTRACT_FILE: &str = "local-inference-contract.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const MANIFEST_FILE: &str = "manifest.json";
const MANIFEST_SCHEMA: &str = "chatarium-local-archive-backup";
const MANIFEST_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveIntegrityReport {
    pub status: &'static str,
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
    RemoteHealthController::from_events(&events, unix_ms() as u64).map_err(err)?;
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
    let warnings = usize::from(tail > 0);
    Ok(ArchiveIntegrityReport {
        status: if warnings == 0 { "HEALTHY" } else { "WARNING" },
        healthy: warnings == 0,
        warnings,
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

fn copy_consistent(
    source: &Path,
    destination: &Path,
) -> Result<(u64, String), ArchiveMaintenanceError> {
    let before = sha256(source)?;
    fs::copy(source, destination)?;
    let after = sha256(source)?;
    let copied = sha256(destination)?;
    if before != after || after != copied {
        let _ = fs::remove_file(destination);
        return Err(err("archive source changed during backup"));
    }
    Ok(after)
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
    for name in [
        JOURNAL_FILE,
        CACHE_FILE,
        INFERENCE_SETTINGS_FILE,
        BEHAVIOR_PROFILES_FILE,
        LOCAL_CONVERSATION_CATALOG_FILE,
        CAPABILITY_PROBE_REPORT_FILE,
        LOCAL_INFERENCE_CONTRACT_FILE,
    ] {
        let source = data_dir.join(name);
        if source.exists() {
            let (size, hash) = copy_consistent(&source, &temporary.join(name))?;
            files.push(json!({"path": name, "size_bytes": size, "sha256": hash}));
        }
    }
    if !temporary.join(JOURNAL_FILE).exists() {
        fs::write(temporary.join(JOURNAL_FILE), [])?;
        let (size, hash) = sha256(&temporary.join(JOURNAL_FILE))?;
        files.push(json!({"path": JOURNAL_FILE, "size_bytes": size, "sha256": hash}));
    }
    let manifest = json!({
        "schema": MANIFEST_SCHEMA,
        "version": MANIFEST_VERSION,
        "created_at_unix_ms": unix_ms(),
        "data_schema_versions": {
            "journal": 2,
            "history_cache": 1,
            "local_inference_settings": 1,
            "behavior_profiles": 1,
            "local_conversation_catalog": 1,
            "siwc_capability_probe": 1,
            "local_inference_contract": 1
        },
        "files": files,
        "archive": {
            "journal_event_count": archive.journal_event_count,
            "highest_sequence": archive.highest_sequence,
            "catalog_count": archive.catalog_count,
            "snapshot_count": archive.snapshot_count,
            "mirror_count": archive.queue_full_count + archive.queue_partial_count
        }
    });
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
    if manifest
        .get("created_at_unix_ms")
        .and_then(Value::as_u64)
        .is_none()
        || manifest
            .get("data_schema_versions")
            .and_then(Value::as_object)
            .is_none()
    {
        return Err(err("backup manifest metadata missing"));
    }
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| err("manifest files missing"))?;
    let mut names = BTreeSet::new();
    for file in files {
        let name = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| err("manifest path missing"))?;
        let relative = safe_relative(name)?;
        if !names.insert(name) {
            return Err(err("backup manifest contains duplicate file"));
        }
        let path = backup.join(relative);
        let (size, hash) = sha256(&path)?;
        if file.get("size_bytes").and_then(Value::as_u64) != Some(size)
            || file.get("sha256").and_then(Value::as_str) != Some(hash.as_str())
        {
            return Err(err("backup file hash or size mismatch"));
        }
    }
    let archive = check_archive(backup)?;
    if !names.contains(JOURNAL_FILE) {
        return Err(err("backup manifest omits journal"));
    }
    let declared = manifest
        .get("archive")
        .ok_or_else(|| err("backup archive metadata missing"))?;
    if declared.get("journal_event_count").and_then(Value::as_u64)
        != Some(archive.journal_event_count as u64)
        || declared.get("highest_sequence").and_then(Value::as_u64)
            != Some(archive.highest_sequence)
        || declared.get("catalog_count").and_then(Value::as_u64)
            != Some(archive.catalog_count as u64)
        || declared.get("snapshot_count").and_then(Value::as_u64)
            != Some(archive.snapshot_count as u64)
    {
        return Err(err("backup manifest archive metadata mismatch"));
    }
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
    for name in [
        JOURNAL_FILE,
        CACHE_FILE,
        INFERENCE_SETTINGS_FILE,
        BEHAVIOR_PROFILES_FILE,
        LOCAL_CONVERSATION_CATALOG_FILE,
        CAPABILITY_PROBE_REPORT_FILE,
        LOCAL_INFERENCE_CONTRACT_FILE,
    ] {
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
    use crate::{EventStore, JsonlEventStore, inspect_jsonl_journal};
    use chatarium_core::EventKind;
    use std::io::Write;

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
    fn backup_and_restore_preserve_local_inference_settings() {
        let root = temp("inference-settings");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let _store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        fs::write(
            source.join(INFERENCE_SETTINGS_FILE),
            br#"{"schema":"chatarium-local-inference-settings","version":1,"conversations":{}}"#,
        )
        .unwrap();

        let created = create_backup(&source, &backup).unwrap();
        assert_eq!(created.manifest_files, 2);
        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(source.join(INFERENCE_SETTINGS_FILE)).unwrap(),
            fs::read(target.join(INFERENCE_SETTINGS_FILE)).unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_and_restore_preserve_behavior_profiles() {
        let root = temp("behavior-profiles");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let _store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        fs::write(
            source.join(BEHAVIOR_PROFILES_FILE),
            br#"{"schema":"chatarium-behavior-profiles","version":1,"conversations":{}}"#,
        )
        .unwrap();

        let created = create_backup(&source, &backup).unwrap();
        assert_eq!(created.manifest_files, 2);
        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(source.join(BEHAVIOR_PROFILES_FILE)).unwrap(),
            fs::read(target.join(BEHAVIOR_PROFILES_FILE)).unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_and_restore_preserve_local_conversation_catalog() {
        let root = temp("local-conversation-catalog");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let _store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        fs::write(
            source.join(LOCAL_CONVERSATION_CATALOG_FILE),
            br#"{"schema":"chatarium-local-conversation-catalog","version":1,"active_conversation_id":null,"conversations":[]}"#,
        )
        .unwrap();

        let created = create_backup(&source, &backup).unwrap();
        assert_eq!(created.manifest_files, 2);
        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(source.join(LOCAL_CONVERSATION_CATALOG_FILE)).unwrap(),
            fs::read(target.join(LOCAL_CONVERSATION_CATALOG_FILE)).unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_and_restore_preserve_capability_probe_report() {
        let root = temp("capability-probe-report");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let _store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        fs::write(
            source.join(CAPABILITY_PROBE_REPORT_FILE),
            br#"{"schema":"chatarium-siwc-capability-probe","version":1,"generated_unix_ms":1234,"model":"gpt-example","probes":[]}"#,
        )
        .unwrap();
        fs::write(
            source.join(LOCAL_INFERENCE_CONTRACT_FILE),
            br#"{"schema":"chatarium-local-inference-contract","version":1,"state":"incomplete"}"#,
        )
        .unwrap();

        let created = create_backup(&source, &backup).unwrap();
        assert_eq!(created.manifest_files, 3);
        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(source.join(CAPABILITY_PROBE_REPORT_FILE)).unwrap(),
            fs::read(target.join(CAPABILITY_PROBE_REPORT_FILE)).unwrap()
        );
        assert_eq!(
            fs::read(source.join(LOCAL_INFERENCE_CONTRACT_FILE)).unwrap(),
            fs::read(target.join(LOCAL_INFERENCE_CONTRACT_FILE)).unwrap()
        );
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

    #[test]
    fn read_only_inspection_reports_torn_tail_without_repairing() {
        let root = temp("torn");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let journal = root.join(JOURNAL_FILE);
        let mut store = JsonlEventStore::open(&journal).unwrap();
        store
            .append(EventKind::DraftChanged, "local".into())
            .unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"torn")
            .unwrap();
        let before = fs::metadata(&journal).unwrap().len();
        let inspection = inspect_jsonl_journal(&journal).unwrap();
        assert_eq!(inspection.events.len(), 1);
        assert!(inspection.unterminated_tail_bytes > 0);
        assert_eq!(fs::metadata(&journal).unwrap().len(), before);
        assert_eq!(check_archive(&root).unwrap().status, "WARNING");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_gap_and_unknown_records_fail_closed() {
        for (label, body) in [
            ("malformed", b"not-json\n".as_slice()),
            (
                "gap",
                br#"{"v":2,"sequence":2,"at_unix_ms":1,"kind":"draft_changed","payload":"x"}
"#
                .as_slice(),
            ),
            (
                "unknown",
                br#"{"v":2,"sequence":1,"at_unix_ms":1,"kind":"future_event","payload":"x"}
"#
                .as_slice(),
            ),
        ] {
            let root = temp(label);
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join(JOURNAL_FILE), body).unwrap();
            assert!(check_archive(&root).is_err());
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn restore_validates_in_isolation_and_preserves_target_on_failure() {
        let root = temp("restore");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        let mut source_store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        source_store
            .append(EventKind::DraftChanged, "source".into())
            .unwrap();
        let mut target_store = JsonlEventStore::open(target.join(JOURNAL_FILE)).unwrap();
        target_store
            .append(EventKind::DraftChanged, "target".into())
            .unwrap();
        drop(source_store);
        drop(target_store);
        let source_report = check_archive(&source).unwrap();
        create_backup(&source, &backup).unwrap();
        let mut corrupt = fs::read(backup.join(JOURNAL_FILE)).unwrap();
        corrupt.push(b'!');
        fs::write(backup.join(JOURNAL_FILE), corrupt).unwrap();
        assert!(restore_backup(&backup, &target).is_err());
        assert_eq!(check_archive(&target).unwrap().journal_event_count, 1);
        create_backup(&source, root.join("good-backup")).unwrap();
        let restored = restore_backup(root.join("good-backup"), &target).unwrap();
        assert_eq!(restored.archive, source_report);
        assert_eq!(check_archive(&target).unwrap(), source_report);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_manifest_and_missing_file_are_rejected() {
        let root = temp("manifest");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut store = JsonlEventStore::open(root.join(JOURNAL_FILE)).unwrap();
        store
            .append(EventKind::DraftChanged, "local".into())
            .unwrap();
        let backup = root.join("backup");
        create_backup(&root, &backup).unwrap();
        let manifest = backup.join(MANIFEST_FILE);
        let original = fs::read(&manifest).unwrap();
        fs::write(&manifest, b"{}\n").unwrap();
        assert!(verify_backup(&backup).is_err());
        fs::write(&manifest, original).unwrap();
        fs::write(backup.join(JOURNAL_FILE), b"").unwrap();
        assert!(verify_backup(&backup).is_err());
        let _ = fs::remove_dir_all(root);
    }
}
