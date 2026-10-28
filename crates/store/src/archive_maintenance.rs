//! Local archive integrity, backup, verification, and fail-closed restore.
//!
//! The journal is authoritative. The history cache is copied as a convenience projection and
//! is validated structurally; neither backup manifests nor reports contain private identities,
//! titles, message text, or raw response bodies.

use crate::continuation_execution_audit::replay_worker_continuation_execution_audit;
use crate::control_ack_audit::replay_worker_control_acknowledgement_audit;
use crate::control_action_audit::replay_worker_control_action_audit;
use crate::control_admission_audit::replay_validated_control_admissions;
use crate::control_delivery_audit::replay_worker_control_delivery_audit;
use crate::control_dispatch_audit::replay_validated_control_dispatches;
use crate::control_inbox::replay_worker_control_inbox;
use crate::control_provenance_audit::replay_control_provenance_audit;
use crate::control_result_audit::replay_worker_control_status_results;
use crate::control_route_audit::replay_control_route_audit;
use crate::controller_coordination_audit::replay_controller_coordination_audit;
use crate::controller_coordination_context_audit::replay_controller_coordination_result_context_audit;
use crate::controller_coordination_suggestion_audit::replay_controller_coordination_suggestion_audit;
use crate::controller_result_context_audit::replay_controller_worker_result_context_audit;
use crate::controller_result_inbox::replay_controller_worker_results;
use crate::import_source_quarantine_audit::replay_import_source_quarantine_audit;
use crate::local_conversation_chat_container_audit::replay_local_conversation_topologies;
use crate::local_memory_audit::replay_local_memory_audit;
use crate::local_memory_context_audit::{
    replay_admitted_local_memory_context, replay_local_memory_context_audit,
};
use crate::local_memory_label_audit::replay_local_memory_label_audit;
use crate::local_memory_search::{
    local_memory_label_facets, local_memory_source_facets, search_local_memory,
};
use crate::local_memory_supersession_audit::replay_local_memory_supersession_audit;
use crate::local_memory_turn_selection_audit::replay_local_memory_turn_selection_audit;
use crate::local_route_context_audit::replay_local_route_context_audit;
use crate::local_route_delivery_audit::replay_local_route_delivery_audit;
use crate::local_route_payload_audit::replay_local_route_payload_audit;
use crate::local_routed_inbox::replay_local_routed_inbox;
use crate::local_routing_directory::replay_local_routing_directory;
use crate::orchestration_route_audit::replay_validated_orchestration_routes;
use crate::remote_health::RemoteHealthController;
use crate::remote_identity_audit::replay_remote_identity_audit;
use crate::remote_mirror_queue::{RemoteMirrorQueueStatus, derive_remote_mirror_queue};
use crate::remote_mirror_selection_audit::replay_remote_mirror_selection_audit;
use crate::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use crate::remote_mirror_transcript::project_remote_active_transcript;
use crate::remote_read_audit::replay_remote_read_audit;
use crate::siwc_agent_activity_audit::replay_siwc_agent_activity_audit;
use crate::supervision_audit::replay_supervision_audit;
use crate::tool_call_audit::replay_tool_call_audit;
use crate::tool_outcome_audit::replay_tool_call_outcome_audit;
use crate::tool_provider_activation_audit::replay_tool_provider_activation_audit;
use crate::tool_provider_audit::replay_tool_provider_audit;
use crate::tool_result_context_audit::replay_tool_result_context_audit;
use crate::tool_stdio_dispatch::replay_unresolved_external_tool_dispatches;
use crate::tool_transport_config_audit::replay_tool_transport_config_audit;
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
const MCP_AGENT_DEFAULTS_FILE: &str = "mcp-agent-defaults.json";
const LOCAL_CONVERSATION_CATALOG_FILE: &str = "local-conversations.json";
const CAPABILITY_PROBE_REPORT_FILE: &str = "siwc-capability-probes.json";
const LOCAL_INFERENCE_CONTRACT_FILE: &str = "local-inference-contract.json";
const JOURNAL_FILE: &str = "journal.jsonl";
const FLIGHT_ARCHIVE_DIR: &str = "imports/flight-recorder";
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
    replay_local_conversation_topologies(&events).map_err(err)?;
    let quarantine = replay_import_source_quarantine_audit(&events).map_err(err)?;
    for decision in quarantine {
        let relative = format!("{FLIGHT_ARCHIVE_DIR}/{}.json", decision.source_sha256);
        let source = data_dir.join(relative);
        let (_, digest) = sha256(&source)
            .map_err(|_| err("quarantine source archive is missing or unreadable"))?;
        if digest != decision.source_sha256 {
            return Err(err("quarantine source archive SHA-256 mismatch"));
        }
        let source_bytes = fs::read(&source)?;
        let parsed: Value = serde_json::from_slice(&source_bytes)
            .map_err(|_| err("quarantine source archive is not valid JSON"))?;
        if parsed.get("format").and_then(Value::as_str) != Some("chatarium-flight-recorder-export")
            || parsed.get("version").and_then(Value::as_u64) != Some(3)
        {
            return Err(err(
                "quarantine source archive has unsupported format or version",
            ));
        }
    }
    replay_supervision_audit(&events).map_err(err)?;
    replay_tool_provider_audit(&events).map_err(err)?;
    replay_tool_call_audit(&events).map_err(err)?;
    replay_tool_call_outcome_audit(&events).map_err(err)?;
    replay_tool_result_context_audit(&events).map_err(err)?;
    replay_tool_transport_config_audit(&events).map_err(err)?;
    replay_tool_provider_activation_audit(&events).map_err(err)?;
    replay_unresolved_external_tool_dispatches(&events).map_err(err)?;
    replay_validated_control_admissions(&events).map_err(err)?;
    replay_control_provenance_audit(&events).map_err(err)?;
    replay_control_route_audit(&events).map_err(err)?;
    replay_validated_orchestration_routes(&events).map_err(err)?;
    replay_validated_control_dispatches(&events).map_err(err)?;
    replay_worker_control_delivery_audit(&events).map_err(err)?;
    replay_worker_control_inbox(&events).map_err(err)?;
    replay_worker_control_acknowledgement_audit(&events).map_err(err)?;
    replay_worker_control_action_audit(&events).map_err(err)?;
    replay_worker_continuation_execution_audit(&events).map_err(err)?;
    replay_controller_worker_results(&events).map_err(err)?;
    replay_controller_worker_result_context_audit(&events).map_err(err)?;
    replay_controller_coordination_audit(&events).map_err(err)?;
    replay_controller_coordination_result_context_audit(&events).map_err(err)?;
    replay_controller_coordination_suggestion_audit(&events).map_err(err)?;
    replay_worker_control_status_results(&events).map_err(err)?;
    replay_local_routing_directory(&events).map_err(err)?;
    replay_local_route_payload_audit(&events).map_err(err)?;
    replay_local_route_delivery_audit(&events).map_err(err)?;
    replay_local_routed_inbox(&events).map_err(err)?;
    replay_local_route_context_audit(&events).map_err(err)?;
    replay_local_memory_audit(&events).map_err(err)?;
    replay_local_memory_supersession_audit(&events).map_err(err)?;
    replay_local_memory_label_audit(&events).map_err(err)?;
    search_local_memory(&events, "", true).map_err(err)?;
    local_memory_label_facets(&events, true).map_err(err)?;
    local_memory_source_facets(&events, true).map_err(err)?;
    replay_local_memory_turn_selection_audit(&events).map_err(err)?;
    // Tool and agent activity are context-excluded, but they are still
    // authoritative local journal facts and must pass archive integrity.
    replay_siwc_agent_activity_audit(&events).map_err(err)?;
    let memory_context = replay_local_memory_context_audit(&events).map_err(err)?;
    let memory_destinations = memory_context
        .iter()
        .map(|record| record.destination_conversation_id)
        .collect::<BTreeSet<_>>();
    for destination_conversation_id in memory_destinations {
        replay_admitted_local_memory_context(&events, destination_conversation_id).map_err(err)?;
    }
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

/// Enumerate all content-addressed Flight Recorder archives safely. No
/// symlinks, nested paths, partial files, or unverifiable names are admitted.
fn flight_source_archives(data_dir: &Path) -> Result<Vec<String>, ArchiveMaintenanceError> {
    let dir = data_dir.join(FLIGHT_ARCHIVE_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !fs::symlink_metadata(&dir)?.file_type().is_dir()
        || !fs::symlink_metadata(data_dir.join("imports"))?
            .file_type()
            .is_dir()
    {
        return Err(err(
            "import source archive directory is not a real directory",
        ));
    }
    let mut archives = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Err(err("import source archive entry is not a regular file"));
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| err("import source archive has a non-UTF8 filename"))?;
        let Some(hash) = name.strip_suffix(".json") else {
            return Err(err("import source archive has an unexpected filename"));
        };
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(err(
                "import source archive filename is not lowercase SHA-256",
            ));
        }
        let (_, digest) = sha256(&entry.path())?;
        if digest != hash {
            return Err(err(
                "import source archive bytes do not match filename SHA-256",
            ));
        }
        archives.push(format!("{FLIGHT_ARCHIVE_DIR}/{name}"));
    }
    archives.sort();
    Ok(archives)
}

fn allowed_backup_path(name: &str) -> bool {
    if [
        JOURNAL_FILE,
        CACHE_FILE,
        INFERENCE_SETTINGS_FILE,
        BEHAVIOR_PROFILES_FILE,
        MCP_AGENT_DEFAULTS_FILE,
        LOCAL_CONVERSATION_CATALOG_FILE,
        CAPABILITY_PROBE_REPORT_FILE,
        LOCAL_INFERENCE_CONTRACT_FILE,
    ]
    .contains(&name)
    {
        return true;
    }
    let Some(hash_name) = name.strip_prefix("imports/flight-recorder/") else {
        return false;
    };
    let Some(hash) = hash_name.strip_suffix(".json") else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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
        MCP_AGENT_DEFAULTS_FILE,
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
    for name in flight_source_archives(data_dir)? {
        let destination = temporary.join(&name);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| err("invalid archive path"))?,
        )?;
        let (size, hash) = copy_consistent(&data_dir.join(&name), &destination)?;
        files.push(json!({"path": name, "size_bytes": size, "sha256": hash}));
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
        if !allowed_backup_path(name) {
            return Err(err("backup manifest contains unsupported path"));
        }
        if !names.insert(name) {
            return Err(err("backup manifest contains duplicate file"));
        }
        let path = backup.join(relative);
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(err("backup manifest file is not a regular file"));
        }
        if name.starts_with("imports/") {
            for directory in ["imports", FLIGHT_ARCHIVE_DIR] {
                if !fs::symlink_metadata(backup.join(directory))?
                    .file_type()
                    .is_dir()
                {
                    return Err(err("backup import directory must not be a symlink"));
                }
            }
        }
        let (size, hash) = sha256(&path)?;
        if file.get("size_bytes").and_then(Value::as_u64) != Some(size)
            || file.get("sha256").and_then(Value::as_str) != Some(hash.as_str())
        {
            return Err(err("backup file hash or size mismatch"));
        }
    }
    for name in flight_source_archives(backup)? {
        if !names.contains(name.as_str()) {
            return Err(err("backup manifest omits an archived import source"));
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

/// Clean failed restore staging without touching the pre-existing target.
struct RestoreStagingCleanup(PathBuf);

impl Drop for RestoreStagingCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn verify_staged_file(manifest_file: &Value, copied: &Path) -> Result<(), ArchiveMaintenanceError> {
    let (size, hash) = sha256(copied)?;
    if manifest_file.get("size_bytes").and_then(Value::as_u64) != Some(size)
        || manifest_file.get("sha256").and_then(Value::as_str) != Some(hash.as_str())
    {
        return Err(err(
            "restored staged file differs from verified backup manifest",
        ));
    }
    Ok(())
}

/// Publish a validated staging tree, rolling back the original directory if
/// the second rename fails. The simulation flag is only enabled by tests.
fn publish_staged_restore(
    staging: &Path,
    target: &Path,
    parent: &Path,
    simulate_failure: bool,
) -> Result<(), ArchiveMaintenanceError> {
    let safety = if target.exists() {
        let safety = parent.join(format!(".chatarium-pre-restore-{}", unix_ms()));
        if safety.exists() {
            return Err(err("restore safety snapshot path already exists"));
        }
        fs::rename(target, &safety)?;
        Some(safety)
    } else {
        None
    };
    let published = if simulate_failure {
        Err(io::Error::other("synthetic restore publication failure"))
    } else {
        fs::rename(staging, target)
    };
    if let Err(error) = published {
        if let Some(safety) = &safety {
            if let Err(rollback_error) = fs::rename(safety, target) {
                return Err(err(format!(
                    "restore activation failed ({error}); rollback also failed ({rollback_error}); original preserved at {}",
                    safety.display()
                )));
            }
        }
        return Err(err(format!(
            "restore activation failed; original target preserved: {error}"
        )));
    }
    Ok(())
}

/// Verify a package in isolation, then atomically replace the target while retaining a safety copy.
pub fn restore_backup(
    backup: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<BackupVerification, ArchiveMaintenanceError> {
    let backup = backup.as_ref();
    let target = target.as_ref();
    let verification = verify_backup(backup)?;
    // Never rename the active data directory out from under a running desktop.
    // Use the same exclusive OS-backed lease as JsonlEventStore, held through
    // staged verification and publication. No journal bytes are opened for
    // append or recovery by the restore path.
    if let Ok(metadata) = fs::symlink_metadata(target) {
        if !metadata.file_type().is_dir() {
            return Err(err(
                "restore target must be a real directory, not a file or symlink",
            ));
        }
    }
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    // Keep this lock in the target's PARENT throughout publication. A lock
    // file inside target would move with the directory (or prevent rename
    // entirely on Windows) and allow split-brain journal ownership.
    let _restore_writer_guard = crate::acquire_archive_restore_lease(target)
        .map_err(|error| err(format!("restore refuses active journal writer: {error}")))?;
    if target.is_dir() {
        // Older app versions did not hold the parent lease. This detects an
        // already-running legacy writer; a true mixed-version race still
        // requires stopping every older app before publication.
        crate::reject_active_legacy_journal_writer(target)
            .map_err(|error| err(format!("restore refuses legacy journal writer: {error}")))?;
    }
    let staging = parent.join(format!(".chatarium-restore-{}", std::process::id()));
    if staging.exists() {
        return Err(err("restore staging path already exists"));
    }
    fs::create_dir_all(&staging)?;
    let _staging_cleanup = RestoreStagingCleanup(staging.clone());
    let manifest: Value = serde_json::from_slice(&fs::read(backup.join(MANIFEST_FILE))?)
        .map_err(|error| err(format!("manifest JSON: {error}")))?;
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| err("verified manifest has no files"))?;
    for file in files {
        let name = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| err("verified manifest path missing"))?;
        let relative = safe_relative(name)?;
        if !allowed_backup_path(name) {
            return Err(err("verified manifest has unsupported path"));
        }
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(backup.join(relative), &destination)?;
        // Re-hash staged bytes; a source changed after verify_backup
        // must not be published.
        verify_staged_file(file, &destination)?;
    }
    check_archive(&staging)?;
    publish_staged_restore(&staging, target, parent, false)?;
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
    fn siwc_activity_backup_replays_and_archive_rejects_forged_provenance() {
        use crate::siwc_agent_activity_audit::{
            append_siwc_agent_activity_checked, replay_siwc_agent_activity_audit,
        };
        use chatarium_core::{LocalConversationId, LocalTurnId};

        let root = temp("siwc-activity");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let conversation = LocalConversationId::new();
        let turn = LocalTurnId::new();
        let mut store = JsonlEventStore::open(root.join(JOURNAL_FILE)).unwrap();
        let frame = json!({
            "type": "agent_activity",
            "source": "siwc_bridge_observation",
            "request_id": "request-1",
            "sequence": 1,
            "stage": "started",
            "observed_at_utc": "2026-10-13T18:30:00.000Z",
            "elapsed_ms": 0.0
        })
        .to_string();
        append_siwc_agent_activity_checked(&mut store, conversation, turn, &frame).unwrap();

        check_archive(&root).unwrap();
        let backup = root.join("backup");
        let target = root.join("restored");
        create_backup(&root, &backup).unwrap();
        verify_backup(&backup).unwrap();
        restore_backup(&backup, &target).unwrap();
        let restored = JsonlEventStore::open(target.join(JOURNAL_FILE)).unwrap();
        let rows = replay_siwc_agent_activity_audit(restored.events()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conversation_id, conversation);
        assert_eq!(rows[0].turn_id, turn);
        drop(restored);

        // A malformed complete activity event cannot masquerade as a valid
        // backup or be normalized into success during archive inspection.
        let previous: Value = serde_json::from_str(&store.events()[0].payload).unwrap();
        let mut corrupted = previous;
        corrupted["observation"]["source"] = json!("untrusted");
        store
            .append_scoped(
                store.events()[0].scope.clone(),
                EventKind::SiwcAgentActivityObserved,
                corrupted.to_string(),
            )
            .unwrap();
        assert!(check_archive(&root).is_err());
        let _ = fs::remove_dir_all(root);
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
    fn quarantined_source_archives_survive_backup_and_restore_with_verified_bytes() {
        let root = temp("quarantine-source");
        let _ = fs::remove_dir_all(&root);
        let data_dir = root.join("original");
        let backup = root.join("backup");
        let restored = root.join("restored");
        let archive_dir = data_dir.join(FLIGHT_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).unwrap();

        let source = json!({
            "format": "chatarium-flight-recorder-export",
            "version": 3,
            "events": []
        })
        .to_string();
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        let archive_name = format!("{FLIGHT_ARCHIVE_DIR}/{hash}.json");
        fs::write(data_dir.join(&archive_name), &source).unwrap();
        let mut store = JsonlEventStore::open(data_dir.join(JOURNAL_FILE)).unwrap();
        let imported_payload = |key: &str, details: Value| {
            json!({
                "schema": "chatarium-imported-observation",
                "source": {
                    "format": "chatarium-flight-recorder-export",
                    "sha256": hash,
                    "event_key": key,
                },
                "details": details,
            })
            .to_string()
        };
        store
            .append(
                EventKind::ImportStarted,
                imported_payload("import:start", json!({"archive": archive_name})),
            )
            .unwrap();
        store
            .append_scoped(
                Some("remote:source".to_owned()),
                EventKind::DraftChanged,
                imported_payload("draft:one", json!({})),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportCompleted,
                imported_payload("import:complete", json!({"semantic_event_count": 1})),
            )
            .unwrap();
        store
            .append(
                EventKind::ImportSourceQuarantineRecorded,
                json!({
                    "schema": "chatarium-source-quarantine-audit",
                    "version": 1,
                    "record": "source_quarantine_recorded",
                    "source_format": "chatarium-flight-recorder-export",
                    "source_sha256": hash,
                    "preflight_journal_sha256": "b".repeat(64),
                    "matching_event_count": 3,
                    "first_sequence": 1,
                    "last_sequence": 3,
                })
                .to_string(),
            )
            .unwrap();
        drop(store);

        check_archive(&data_dir).unwrap();
        let created = create_backup(&data_dir, &backup).unwrap();
        assert_eq!(created.manifest_files, 2);
        assert_eq!(
            fs::read(data_dir.join(&archive_name)).unwrap(),
            fs::read(backup.join(&archive_name)).unwrap()
        );
        restore_backup(&backup, &restored).unwrap();
        assert_eq!(
            fs::read(data_dir.join(&archive_name)).unwrap(),
            fs::read(restored.join(&archive_name)).unwrap()
        );
        check_archive(&restored).unwrap();

        // Source evidence tampering is a hard error, not a warning.
        fs::write(restored.join(&archive_name), b"tampered").unwrap();
        assert!(check_archive(&restored).is_err());
        fs::write(backup.join(&archive_name), b"tampered").unwrap();
        assert!(verify_backup(&backup).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn backup_manifest_must_list_every_content_addressed_source_archive() {
        let root = temp("quarantine-manifest");
        let _ = fs::remove_dir_all(&root);
        let original = root.join("original");
        let backup = root.join("backup");
        let archive_dir = original.join(FLIGHT_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir).unwrap();
        let source = b"{\"version\":3}";
        let hash = format!("{:x}", Sha256::digest(source));
        let archive_name = format!("{FLIGHT_ARCHIVE_DIR}/{hash}.json");
        fs::write(original.join(&archive_name), source).unwrap();
        fs::write(original.join(JOURNAL_FILE), []).unwrap();
        assert_eq!(create_backup(&original, &backup).unwrap().manifest_files, 2);
        let manifest = backup.join(MANIFEST_FILE);
        let mut data: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        data["files"]
            .as_array_mut()
            .unwrap()
            .retain(|file| file["path"].as_str() != Some(archive_name.as_str()));
        fs::write(&manifest, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(verify_backup(&backup).is_err());
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
    fn backup_and_restore_preserve_global_mcp_agent_defaults() {
        let root = temp("mcp-agent-defaults");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let backup = root.join("backup");
        let target = root.join("restored");
        fs::create_dir_all(&source).unwrap();
        let _store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        let original = br#"{"schema":"chatarium-mcp-agent-defaults","version":1,"bootstrap":"local tools","policy_rules":"chatarium.builtin::hello = ask","max_calls":8,"seed_new_conversations":true}"#;
        fs::write(source.join(MCP_AGENT_DEFAULTS_FILE), original).unwrap();

        let created = create_backup(&source, &backup).unwrap();
        assert_eq!(created.manifest_files, 2);
        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(target.join(MCP_AGENT_DEFAULTS_FILE)).unwrap(),
            original
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
    fn failed_restore_publish_rolls_back_original_directory() {
        let root = temp("publish-rollback");
        let _ = fs::remove_dir_all(&root);
        let target = root.join("target");
        let staging = root.join("staging");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(target.join("original.txt"), b"original").unwrap();
        fs::write(staging.join("new.txt"), b"new").unwrap();

        let error = publish_staged_restore(&staging, &target, &root, true)
            .expect_err("simulated failed publication must roll back");
        assert!(error.to_string().contains("original target preserved"));
        assert_eq!(fs::read(target.join("original.txt")).unwrap(), b"original");
        assert!(!target.join("new.txt").exists());
        assert_eq!(fs::read(staging.join("new.txt")).unwrap(), b"new");

        publish_staged_restore(&staging, &target, &root, false).unwrap();
        assert_eq!(fs::read(target.join("new.txt")).unwrap(), b"new");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staged_copy_is_reverified_against_manifest() {
        let root = temp("staged-hash");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.json");
        fs::write(&path, b"verified").unwrap();
        let (size, digest) = sha256(&path).unwrap();
        let manifest_entry = json!({
            "path": "source.json",
            "size_bytes": size,
            "sha256": digest,
        });
        verify_staged_file(&manifest_entry, &path).unwrap();
        fs::write(&path, b"mutated!").unwrap();
        assert!(verify_staged_file(&manifest_entry, &path).is_err());
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
    fn restore_refuses_active_journal_writer_and_preserves_original_bytes() {
        let root = temp("restore-live-writer");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();

        let mut source_store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        source_store
            .append(EventKind::DraftChanged, "restored source".into())
            .unwrap();
        drop(source_store);
        create_backup(&source, &backup).unwrap();

        let mut active = JsonlEventStore::open(target.join(JOURNAL_FILE)).unwrap();
        active
            .append(EventKind::DraftChanged, "live user data".into())
            .unwrap();
        let original = fs::read(target.join(JOURNAL_FILE)).unwrap();
        let error = restore_backup(&backup, &target).expect_err("live writer must block restore");
        assert!(error.to_string().contains("active journal writer"));
        assert_eq!(fs::read(target.join(JOURNAL_FILE)).unwrap(), original);
        active
            .append(EventKind::DraftChanged, "still alive".into())
            .unwrap();
        assert_eq!(active.events().len(), 2);
        drop(active);

        let outcome = restore_backup(&backup, &target).unwrap();
        assert!(outcome.valid);
        assert_eq!(
            fs::read(target.join(JOURNAL_FILE)).unwrap(),
            fs::read(backup.join(JOURNAL_FILE)).unwrap()
        );
        let reopened = JsonlEventStore::open(target.join(JOURNAL_FILE)).unwrap();
        assert_eq!(reopened.events().len(), 1);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_detects_writer_that_only_holds_legacy_in_directory_lock() {
        let root = temp("legacy-restore-writer");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        let mut writer = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        writer.append(EventKind::DraftChanged, "source".into()).unwrap();
        drop(writer);
        create_backup(&source, &backup).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();

        // Simulate a previous app version holding only the historical
        // journal.jsonl.writer-lock.sqlite3 lease.
        let legacy = rusqlite::Connection::open(
            target.join("journal.jsonl.writer-lock.sqlite3")
        )
        .unwrap();
        legacy.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let error = restore_backup(&backup, &target).unwrap_err();
        assert!(error.to_string().contains("legacy journal writer"));
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        drop(legacy);

        restore_backup(&backup, &target).unwrap();
        assert_eq!(
            fs::read(target.join(JOURNAL_FILE)).unwrap(),
            fs::read(backup.join(JOURNAL_FILE)).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_lease_blocks_concurrent_publication_even_for_absent_target() {
        let root = temp("restore-parent-lease");
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let mut source_store = JsonlEventStore::open(source.join(JOURNAL_FILE)).unwrap();
        source_store
            .append(EventKind::DraftChanged, "good".into())
            .unwrap();
        drop(source_store);
        create_backup(&source, &backup).unwrap();

        let guard = crate::acquire_archive_restore_lease(&target).unwrap();
        assert!(restore_backup(&backup, &target).is_err());
        assert!(!target.exists(), "refused restore must not publish target");
        drop(guard);
        assert!(restore_backup(&backup, &target).unwrap().valid);
        assert!(target.join(JOURNAL_FILE).exists());
        fs::remove_dir_all(root).unwrap();
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
