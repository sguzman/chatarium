//! Directed project memory grants and verified cross-conversation retrieval.
use super::*;

/// Explicit, directed local permission to share a source conversation's
/// verified checkpoints with one other conversation bound to the same project.
/// A project-folder binding alone NEVER enables cross-conversation recall.
pub const PROJECT_MEMORY_GRANTS_FILE_NAME: &str = "openviking-project-memory-grants.json";
const PROJECT_GRANTS_SCHEMA: &str = "chatarium-openviking-project-memory-grants";
const MAX_PROJECT_GRANTS: usize = 256;
const MAX_PROJECT_GRANTS_FILE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMemoryGrant {
    source: chatarium_core::LocalConversationId,
    destination: chatarium_core::LocalConversationId,
    project_directory: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectMemoryGrants {
    grants: Vec<ProjectMemoryGrant>,
}

fn canonical_bound_project(
    catalog: &crate::local_conversations::LocalConversationCatalog,
    conversation: chatarium_core::LocalConversationId,
) -> Option<String> {
    let directory = catalog.entry(conversation)?.project_directory.as_deref()?;
    let canonical = std::fs::canonicalize(directory).ok()?;
    if !canonical.is_dir() || canonical.to_str()? != directory {
        return None;
    }
    Some(directory.to_owned())
}

impl ProjectMemoryGrants {
    /// No missing or malformed grant file ever widens access.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        use std::str::FromStr;
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path)
            .map_err(|error| format!("project memory permissions read: {error}"))?;
        if bytes.len() > MAX_PROJECT_GRANTS_FILE_BYTES {
            return Err("project memory permissions exceed file limit".to_owned());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid project memory permissions: {error}"))?;
        if value.get("schema").and_then(Value::as_str) != Some(PROJECT_GRANTS_SCHEMA)
            || value.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Err("unsupported project memory permission schema".to_owned());
        }
        let raw_grants = value
            .get("grants")
            .and_then(Value::as_array)
            .ok_or("project memory permission list missing")?;
        if raw_grants.len() > MAX_PROJECT_GRANTS {
            return Err("too many project memory grants".to_owned());
        }
        let mut grants = Vec::with_capacity(raw_grants.len());
        let mut seen = std::collections::BTreeSet::new();
        for raw in raw_grants {
            let source = chatarium_core::LocalConversationId::from_str(
                raw.get("source")
                    .and_then(Value::as_str)
                    .ok_or("grant source missing")?,
            )
            .map_err(|error| format!("invalid grant source: {error}"))?;
            let destination = chatarium_core::LocalConversationId::from_str(
                raw.get("destination")
                    .and_then(Value::as_str)
                    .ok_or("grant destination missing")?,
            )
            .map_err(|error| format!("invalid grant destination: {error}"))?;
            let project_directory = raw
                .get("project_directory")
                .and_then(Value::as_str)
                .ok_or("grant project directory missing")?;
            if source == destination
                || project_directory.is_empty()
                || project_directory.len() > 4096
                || project_directory.contains('\0')
                || !std::path::Path::new(project_directory).is_absolute()
                || !seen.insert((source.to_string(), destination.to_string()))
            {
                return Err("invalid or duplicate project memory grant".to_owned());
            }
            grants.push(ProjectMemoryGrant {
                source,
                destination,
                project_directory: project_directory.to_owned(),
            });
        }
        Ok(Self { grants })
    }

    pub fn save_atomic(&self, path: &std::path::Path) -> Result<(), String> {
        let grants = self
            .grants
            .iter()
            .map(|grant| {
                json!({
                    "source": grant.source.to_string(),
                    "destination": grant.destination.to_string(),
                    "project_directory": grant.project_directory,
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": PROJECT_GRANTS_SCHEMA,
            "version": 1,
            "grants": grants,
        }))
        .map_err(|error| format!("project memory permission serialization: {error}"))?;
        if bytes.len() > MAX_PROJECT_GRANTS_FILE_BYTES {
            return Err("project memory permissions exceed file limit".to_owned());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("project memory permission directory: {error}"))?;
        }
        // Never open the predictable .json.tmp path: a preexisting symlink
        // could redirect a permission write to an unrelated local file.
        // Unique create-new staging also prevents following a stale crash
        // artifact from an earlier save. Only the final rename replaces grants.
        static STAGING_ORDINAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ordinal = STAGING_ORDINAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = path.with_extension(format!(
            "json.tmp.{}.{}.{ordinal}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| format!("project memory permission timestamp: {error}"))?
                .as_nanos(),
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("project memory permission staging: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                drop(file);
                let _ = std::fs::remove_file(&temporary);
                return Err(format!("project memory permission file mode: {error}"));
            }
        }
        use std::io::Write;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&temporary);
            return Err(format!(
                "project memory permission durable staging: {error}"
            ));
        }
        drop(file);
        if let Err(error) = std::fs::rename(&temporary, path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("project memory permission replacement: {error}"));
        }
        // A synced temporary file does not make its renamed directory entry
        // durable. This matters especially for a revoked cross-chat grant:
        // never acknowledge persistence without syncing the parent on Unix.
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .ok_or("project memory permission parent missing")?;
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("project memory permission directory sync: {error}"))?;
        }
        Ok(())
    }

    /// Explicit one-way opt-in. A caller must surface this as a user-reviewed
    /// authorization; discovering a matching folder must not call grant().
    pub fn grant(
        &mut self,
        catalog: &crate::local_conversations::LocalConversationCatalog,
        source: chatarium_core::LocalConversationId,
        destination: chatarium_core::LocalConversationId,
    ) -> Result<(), String> {
        if source == destination {
            return Err("project memory sharing requires distinct conversations".to_owned());
        }
        let project = canonical_bound_project(catalog, source)
            .ok_or("source conversation has no verified project binding")?;
        if canonical_bound_project(catalog, destination).as_deref() != Some(&project) {
            return Err("destination conversation belongs to a different project".to_owned());
        }
        self.grants
            .retain(|grant| !(grant.source == source && grant.destination == destination));
        if self.grants.len() >= MAX_PROJECT_GRANTS {
            return Err("project memory grant limit reached".to_owned());
        }
        self.grants.push(ProjectMemoryGrant {
            source,
            destination,
            project_directory: project,
        });
        Ok(())
    }

    /// Revocation is immediate in memory; caller persists it before further reads.
    pub fn revoke(
        &mut self,
        source: chatarium_core::LocalConversationId,
        destination: chatarium_core::LocalConversationId,
    ) -> bool {
        let old_len = self.grants.len();
        self.grants
            .retain(|grant| !(grant.source == source && grant.destination == destination));
        self.grants.len() != old_len
    }

    pub fn allows(
        &self,
        catalog: &crate::local_conversations::LocalConversationCatalog,
        source: chatarium_core::LocalConversationId,
        destination: chatarium_core::LocalConversationId,
    ) -> bool {
        let Some(source_project) = canonical_bound_project(catalog, source) else {
            return false;
        };
        canonical_bound_project(catalog, destination).as_deref() == Some(&source_project)
            && self.grants.iter().any(|grant| {
                grant.source == source
                    && grant.destination == destination
                    && grant.project_directory == source_project
            })
    }

    /// Does this destination have any currently valid, explicitly granted
    /// source? Project-folder equality alone is insufficient.
    pub fn has_sources(
        &self,
        catalog: &crate::local_conversations::LocalConversationCatalog,
        destination: chatarium_core::LocalConversationId,
    ) -> bool {
        self.grants.iter().any(|grant| {
            grant.destination == destination && self.allows(catalog, grant.source, destination)
        })
    }
}

/// Select at most one newest confirmed checkpoint per explicitly authorized
/// source, globally capped to three results. No project-wide URI scan occurs.
fn confirmed_project_sources<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
) -> Vec<&'a crate::openviking_checkpoint_outbox::Entry> {
    let mut by_source = std::collections::BTreeMap::new();
    for entry in outbox.entries() {
        if entry.state != crate::openviking_checkpoint_outbox::State::Confirmed
            || entry.conversation_id == destination.to_string()
        {
            continue;
        }
        let Ok(source) = entry
            .conversation_id
            .parse::<chatarium_core::LocalConversationId>()
        else {
            continue;
        };
        if !permissions.allows(catalog, source, destination) {
            continue;
        }
        let previous: Option<&&crate::openviking_checkpoint_outbox::Entry> =
            by_source.get(&entry.conversation_id);
        if previous.is_none_or(|prior| entry.completed_sequence > prior.completed_sequence) {
            by_source.insert(entry.conversation_id.clone(), entry);
        }
    }
    let mut results: Vec<_> = by_source.into_values().collect();
    results.sort_by(|left, right| right.completed_sequence.cmp(&left.completed_sequence));
    results.truncate(MAX_RETRIEVAL_ITEMS);
    results
}

/// Select newest rolling evidence for each explicitly authorized source,
/// then spend unused result slots on relevant older checkpoints. No grant
/// permits a global OpenViking namespace scan: only locally confirmed entries
/// whose exact source/destination grant is valid can be considered.
/// The historical scan is capped at 64 additional entries, and remote reads
/// are still limited to three exact, byte-for-byte verified source URIs.
fn select_confirmed_project_history<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
    query: &str,
) -> Result<Vec<(&'a crate::openviking_checkpoint_outbox::Entry, String)>, String> {
    // Preserve the previously supported one-newest-per-source behavior.
    let newest = confirmed_project_sources(outbox, catalog, permissions, destination);
    let mut selected = Vec::with_capacity(MAX_RETRIEVAL_ITEMS);
    for entry in &newest {
        selected.push((*entry, read_confirmed_evidence(outbox_path, entry)?));
    }
    if selected.len() == MAX_RETRIEVAL_ITEMS {
        return Ok(selected);
    }
    let terms = relevance_terms(query);
    if terms.is_empty() || selected.is_empty() {
        return Ok(selected);
    }

    // Search only among the exact sources already admitted by directed grants.
    // Never even inspect local memory bodies for an ungranted conversation.
    let authorized_owners: std::collections::BTreeSet<&str> = newest
        .iter()
        .map(|entry| entry.conversation_id.as_str())
        .collect();
    let mut history: Vec<_> = outbox
        .entries()
        .iter()
        .filter(|entry| {
            entry.state == crate::openviking_checkpoint_outbox::State::Confirmed
                && authorized_owners.contains(entry.conversation_id.as_str())
                && !newest.iter().any(|head| {
                    head.conversation_id == entry.conversation_id
                        && head.turn_id == entry.turn_id
                        && head.completed_sequence == entry.completed_sequence
                })
        })
        .collect();
    history.sort_by(|a, b| {
        b.completed_sequence
            .cmp(&a.completed_sequence)
            .then_with(|| b.turn_id.cmp(&a.turn_id))
    });
    history.truncate(MAX_HISTORY_SCAN_ITEMS);

    let mut matches = Vec::new();
    for entry in history {
        let body = read_confirmed_evidence(outbox_path, entry)?;
        let score = checkpoint_relevance(&body, &terms);
        if score > 0 {
            matches.push((entry, body, score));
        }
    }
    matches.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| b.0.completed_sequence.cmp(&a.0.completed_sequence))
            .then_with(|| b.0.turn_id.cmp(&a.0.turn_id))
    });
    let free_slots = MAX_RETRIEVAL_ITEMS - selected.len();
    selected.extend(
        matches
            .into_iter()
            .take(free_slots)
            .map(|(entry, body, _)| (entry, body)),
    );
    Ok(selected)
}

/// A bounded, explicit cross-conversation read path. Never call on the UI
/// thread. Existing single-conversation retrieval remains the default.
pub fn retrieve_confirmed_for_project(
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    retrieve_confirmed_for_project_query(outbox_path, catalog, permissions, destination, "")
}

/// Query-aware historical recall with the same explicit source/destination
/// grant and exact remote read-back as the original project retrieval path.
pub fn retrieve_confirmed_for_project_query(
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
    query: &str,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    retrieve_confirmed_for_project_with_query_and_read(
        outbox_path,
        catalog,
        permissions,
        destination,
        query,
        |uri, owner, turn| {
            // The MCP broker requires the source owner for an authorized read.
            read_project_checkpoint_as_source(owner, turn, uri, local_http_mcp::invoke)
        },
    )
}

/// Validate the exact selected source URI and authenticate the fixed,
/// read-only MCP operation as that owner. A destination-scoped transport call
/// is correctly denied by the bridge and would make granted sharing unusable.
/// This function may ONLY be called after the host verifies a directed grant.
fn read_project_checkpoint_as_source(
    source: &str,
    turn: &str,
    uri: &str,
    mut invoke: impl FnMut(&str, &str, &str, &str, &Value) -> Result<Value, String>,
) -> Result<Value, String> {
    if checkpoint_uri(source, turn)? != uri {
        return Err("project checkpoint URI does not belong to its verified source".to_owned());
    }
    invoke(
        source,
        &format!("memory-project-read:{source}:{turn}"),
        local_http_mcp::OPENVIKING,
        "read",
        &json!({"uris": uri, "limit": 100}),
    )
}

/// Shared selection, local evidence and exact read-back implementation.
/// An injected read permits deterministic end-to-end mock backend tests;
/// production always dispatches the fixed, allowlisted OpenViking MCP read.
fn retrieve_confirmed_for_project_with_read(
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
    read: impl FnMut(&str, &str, &str) -> Result<Value, String>,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    retrieve_confirmed_for_project_with_query_and_read(
        outbox_path,
        catalog,
        permissions,
        destination,
        "",
        read,
    )
}

fn retrieve_confirmed_for_project_with_query_and_read(
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
    query: &str,
    mut read: impl FnMut(&str, &str, &str) -> Result<Value, String>,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    let outbox = crate::openviking_checkpoint_outbox::Outbox::load(outbox_path)?;
    let sources = select_confirmed_project_history(
        &outbox,
        outbox_path,
        catalog,
        permissions,
        destination,
        query,
    )?;
    let mut used_bytes = 0usize;
    let mut result = Vec::with_capacity(sources.len());
    for (entry, expected) in sources {
        let owner = entry.conversation_id.as_str();
        if expected.len() > MAX_RETRIEVAL_BYTES - used_bytes {
            continue;
        }
        // Re-check authorization at dispatch, not just at earlier selection.
        let source = owner
            .parse::<chatarium_core::LocalConversationId>()
            .map_err(|error| format!("invalid project checkpoint owner: {error}"))?;
        if !permissions.allows(catalog, source, destination) {
            return Err("project memory sharing grant is no longer valid".to_owned());
        }
        let uri = checkpoint_uri(owner, &entry.turn_id)?;
        let remote = read(&uri, owner, &entry.turn_id)?;
        if !readback_matches(&remote, &expected) {
            return Err("project checkpoint remote content differs from local evidence".to_owned());
        }
        used_bytes += expected.len();
        result.push(RetrievedCheckpoint {
            source_conversation_id: owner.to_owned(),
            source_turn_id: entry.turn_id.clone(),
            source_journal_sequence: entry.completed_sequence,
            uri,
            text: expected,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod project_scope_tests;
