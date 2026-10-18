//! Host-owned at-most-once OpenViking writes and strict read-back checks.
//! Never call this on the egui thread or journal worker. The caller must
//! persist an outbound "dispatched" claim before invoking this function.
use crate::local_http_mcp;
use serde_json::{Value, json};

const MAX_CHECKPOINT_BYTES: usize = 16384;

pub fn checkpoint_uri(conversation: &str, turn: &str) -> Result<String, String> {
    fn valid(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
    if !valid(conversation) || !valid(turn) {
        return Err("invalid OpenViking checkpoint identity".to_owned());
    }
    Ok(format!(
        "viking://~/chatarium/conversations/{conversation}/checkpoints/{turn}.md"
    ))
}

/// OpenViking v0.5.0 read(uris=<one>, limit=<lines>) returns one text block.
/// An MCP write tool saying "Wrote X bytes" is not sufficient confirmation.
pub fn readback_matches(value: &Value, expected: &str) -> bool {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return false;
    };
    content.len() == 1
        && content[0].get("type").and_then(Value::as_str) == Some("text")
        && content[0].get("text").and_then(Value::as_str) == Some(expected)
        && value.get("isError") != Some(&Value::Bool(true))
}

pub fn deliver_create_once(conversation: &str, turn: &str, content: &str) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_CHECKPOINT_BYTES {
        return Err("OpenViking checkpoint content is empty or too large".to_owned());
    }
    let uri = checkpoint_uri(conversation, turn)?;
    let identity = format!("memory-create:{conversation}:{turn}");
    local_http_mcp::invoke(
        conversation,
        &identity,
        local_http_mcp::OPENVIKING,
        "write",
        &json!({
            "uri": uri,
            "content": content,
            "mode": "create",
            "wait": false,
        }),
    )?;
    // Verification is a read, never a second write. Failed or unconfirmed
    // verification makes the outbound operation uncertain.
    let readback = local_http_mcp::invoke(
        conversation,
        &format!("memory-verify:{conversation}:{turn}"),
        local_http_mcp::OPENVIKING,
        "read",
        &json!({"uris": uri, "limit": 100}),
    )?;
    if !readback_matches(&readback, content) {
        return Err("OpenViking write was attempted but exact read-back is unconfirmed".to_owned());
    }
    Ok(())
}

/// A crash-uncertain write may already exist remotely. Read, compare with
/// the immutable local source, and NEVER retry its create operation.
pub fn verify_uncertain_read_only(
    outbox_path: &std::path::Path,
    conversation: &str,
    turn: &str,
    completed_sequence: u64,
) -> Result<bool, String> {
    verify_uncertain_with_read(outbox_path, conversation, turn, completed_sequence, |uri| {
        local_http_mcp::invoke(
            conversation,
            &format!("memory-recover-read:{conversation}:{turn}"),
            local_http_mcp::OPENVIKING,
            "read",
            &json!({"uris": uri, "limit": 100}),
        )
    })
}

fn verify_uncertain_with_read(
    outbox_path: &std::path::Path,
    conversation: &str,
    turn: &str,
    completed_sequence: u64,
    read: impl FnOnce(&str) -> Result<Value, String>,
) -> Result<bool, String> {
    let uri = checkpoint_uri(conversation, turn)?;
    if completed_sequence == 0 {
        return Err("invalid uncertain checkpoint source sequence".to_owned());
    }
    let file =
        crate::openviking_checkpoint_outbox::Outbox::summary_path(outbox_path, conversation, turn)?;
    let size = std::fs::metadata(&file)
        .map_err(|e| format!("uncertain checkpoint summary metadata: {e}"))?
        .len();
    if size == 0 || size > MAX_CHECKPOINT_BYTES as u64 {
        return Err("uncertain checkpoint summary size invalid".to_owned());
    }
    let expected = std::fs::read_to_string(&file)
        .map_err(|e| format!("uncertain checkpoint source unreadable: {e}"))?;
    if expected.len() != size as usize
        || !expected.starts_with(&format!(
            "# Chatarium memory checkpoint\n\nConversation: {conversation}\nTurn: {turn}\nSource journal sequence: {completed_sequence}\n\n"
        ))
    {
        return Err("uncertain checkpoint source identity mismatch".to_owned());
    }
    read(&uri).map(|remote| readback_matches(&remote, &expected))
}

/// A verified, in-scope checkpoint. This is evidence for later context admission,
/// not a system/developer message or a replacement for the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievedCheckpoint {
    pub source_conversation_id: String,
    pub source_turn_id: String,
    pub source_journal_sequence: u64,
    pub uri: String,
    pub text: String,
}

const MAX_RETRIEVAL_ITEMS: usize = 3;
const MAX_RETRIEVAL_BYTES: usize = 12 * 1024;

/// Select recent confirmed checkpoints from exactly one conversation. No
/// implicit global search, shared namespace, or cross-conversation access.
fn confirmed_sources<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    conversation: &str,
) -> Result<Vec<&'a crate::openviking_checkpoint_outbox::Entry>, String> {
    // Reuse the same strict path identity validation as the writer.
    checkpoint_uri(conversation, "scope-check")?;
    let mut entries: Vec<_> = outbox
        .entries()
        .iter()
        .filter(|entry| {
            entry.conversation_id == conversation
                && entry.state == crate::openviking_checkpoint_outbox::State::Confirmed
        })
        .collect();
    entries.sort_by(|a, b| {
        b.completed_sequence
            .cmp(&a.completed_sequence)
            .then_with(|| b.turn_id.cmp(&a.turn_id))
    });
    entries.truncate(MAX_RETRIEVAL_ITEMS);
    Ok(entries)
}

/// Retrieve recent, locally confirmed memories for a single owning conversation.
/// Must run off the UI thread. On an offline service or ambiguous read, returns
/// an error; the caller must proceed without external memory, not invent it.
///
/// The persisted local summary is the expected immutable evidence. A remote
/// response is admitted only on an exact match, with no truncation. Cross-chat
/// or cross-project discovery requires a separately authorized scope mechanism.
pub fn retrieve_confirmed_for_conversation(
    outbox_path: &std::path::Path,
    conversation: &str,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    use crate::openviking_checkpoint_outbox::Outbox;
    let outbox = Outbox::load(outbox_path)?;
    let sources = confirmed_sources(&outbox, conversation)?;
    let mut used_bytes = 0usize;
    let mut retrieved = Vec::with_capacity(sources.len());

    for entry in sources {
        let file = Outbox::summary_path(outbox_path, conversation, &entry.turn_id)?;
        let size = std::fs::metadata(&file)
            .map_err(|e| format!("checkpoint summary metadata unavailable: {e}"))?
            .len();
        if size == 0 || size > MAX_CHECKPOINT_BYTES as u64 {
            return Err("confirmed checkpoint has invalid local summary size".to_owned());
        }
        // Never partially inject a checkpoint that does not fit the budget.
        if size > (MAX_RETRIEVAL_BYTES - used_bytes) as u64 {
            break;
        }
        let expected = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read confirmed checkpoint summary: {e}"))?;
        if expected.is_empty() || expected.len() != size as usize {
            return Err("confirmed checkpoint summary changed while reading".to_owned());
        }
        let uri = checkpoint_uri(conversation, &entry.turn_id)?;
        let response = local_http_mcp::invoke(
            conversation,
            &format!("memory-retrieve:{conversation}:{}", entry.turn_id),
            local_http_mcp::OPENVIKING,
            "read",
            &json!({"uris": uri, "limit": 100}),
        )?;
        if !readback_matches(&response, &expected) {
            return Err(
                "confirmed OpenViking memory did not match its local durable source".to_owned(),
            );
        }
        used_bytes += expected.len();
        retrieved.push(RetrievedCheckpoint {
            source_conversation_id: conversation.to_owned(),
            source_turn_id: entry.turn_id.clone(),
            source_journal_sequence: entry.completed_sequence,
            uri,
            text: expected,
        });
    }
    Ok(retrieved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_scopes_to_owner_and_confirmed_entries_only() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-memory-retrieval-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        for (owner, turn, seq, confirm) in [
            ("owner", "older", 2, true),
            ("foreign", "secret", 3, true),
            ("owner", "unconfirmed", 4, false),
            ("owner", "newer", 5, true),
        ] {
            outbox.enqueue_durable(&path, owner, turn, seq).unwrap();
            outbox
                .save_summary_durable(&path, owner, turn, seq, "checkpoint")
                .unwrap();
            if confirm {
                outbox
                    .transition_durable(&path, owner, turn, seq, State::Ready, State::Dispatched)
                    .unwrap();
                outbox
                    .transition_durable(
                        &path,
                        owner,
                        turn,
                        seq,
                        State::Dispatched,
                        State::Confirmed,
                    )
                    .unwrap();
            }
        }
        let selected = confirmed_sources(&outbox, "owner").unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|item| item.turn_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
        assert!(
            confirmed_sources(&outbox, "foreign")
                .unwrap()
                .iter()
                .all(|item| item.conversation_id == "foreign")
        );
        assert!(confirmed_sources(&outbox, "../escape").is_err());
        assert!(confirmed_sources(&outbox, "").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selection_is_bounded_to_latest_three_confirmed_sources() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-retrieval-cap-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        for n in 1..=5 {
            let turn = format!("turn-{n}");
            outbox.enqueue_durable(&path, "owner", &turn, n).unwrap();
            outbox
                .save_summary_durable(&path, "owner", &turn, n, "short checkpoint")
                .unwrap();
            outbox
                .transition_durable(&path, "owner", &turn, n, State::Ready, State::Dispatched)
                .unwrap();
            outbox
                .transition_durable(
                    &path,
                    "owner",
                    &turn,
                    n,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
        }
        assert_eq!(
            confirmed_sources(&outbox, "owner")
                .unwrap()
                .iter()
                .map(|item| item.completed_sequence)
                .collect::<Vec<_>>(),
            vec![5, 4, 3]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn uncertain_checkpoint_recovery_reads_only_and_confirms_exact_bytes() {
        use crate::openviking_checkpoint_outbox::{Outbox, State};
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "chatarium-readonly-memory-recovery-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        outbox
            .enqueue_durable(&path, "owner", "turn-1", 12)
            .unwrap();
        outbox
            .save_summary_durable(&path, "owner", "turn-1", 12, "Durable fact")
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                "turn-1",
                12,
                State::Ready,
                State::Dispatched,
            )
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                "turn-1",
                12,
                State::Dispatched,
                State::Uncertain,
            )
            .unwrap();
        let source = Outbox::summary_path(&path, "owner", "turn-1").unwrap();
        let content = std::fs::read_to_string(&source).unwrap();
        assert!(
            verify_uncertain_with_read(&path, "owner", "turn-1", 12, |uri| {
                assert_eq!(
                    uri,
                    "viking://~/chatarium/conversations/owner/checkpoints/turn-1.md"
                );
                Ok(json!({"content":[{"type":"text","text":content}]}))
            })
            .unwrap()
        );
        assert!(
            !verify_uncertain_with_read(&path, "owner", "turn-1", 12, |_| {
                Ok(json!({"content":[{"type":"text","text":"different"}]}))
            })
            .unwrap()
        );
        assert!(
            verify_uncertain_with_read(&path, "foreign", "turn-1", 12, |_| {
                panic!("remote read must not run for foreign source")
            })
            .is_err()
        );
        std::fs::write(&source, "tampered body").unwrap();
        assert!(
            verify_uncertain_with_read(&path, "owner", "turn-1", 12, |_| {
                panic!("remote read must not run for corrupted source")
            })
            .is_err()
        );
        assert!(
            outbox
                .transition_durable(
                    &path,
                    "owner",
                    "turn-1",
                    12,
                    State::Uncertain,
                    State::Confirmed
                )
                .unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exact_readback_only_verifies_same_content() {
        let ok = json!({"isError": false, "content": [{"type": "text", "text": "stored"}]});
        assert!(readback_matches(&ok, "stored"));
        assert!(!readback_matches(&ok, "other"));
        assert!(!readback_matches(
            &json!({"isError": true, "content": [{"type":"text","text":"stored"}]}),
            "stored"
        ));
        assert!(!readback_matches(&json!({"content":[]}), "stored"));
        assert!(!readback_matches(
            &json!({"content":[{"type":"text","text":"stored"},{"type":"text","text":"stored"}]}),
            "stored"
        ));
    }

    #[test]
    fn memory_paths_are_private_and_no_traversal_or_foreign_identity_is_permitted() {
        assert_eq!(
            checkpoint_uri("conversation-1", "turn-1").unwrap(),
            "viking://~/chatarium/conversations/conversation-1/checkpoints/turn-1.md"
        );
        assert!(checkpoint_uri("../escape", "turn-1").is_err());
        assert!(checkpoint_uri("conversation-1", "evil/turn").is_err());
    }
}

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
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)
            .map_err(|error| format!("project memory permission staging: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("project memory permission replacement: {error}"))
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

/// A bounded, explicit cross-conversation read path. Never call on the UI
/// thread. Existing single-conversation retrieval remains the default.
pub fn retrieve_confirmed_for_project(
    outbox_path: &std::path::Path,
    catalog: &crate::local_conversations::LocalConversationCatalog,
    permissions: &ProjectMemoryGrants,
    destination: chatarium_core::LocalConversationId,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    retrieve_confirmed_for_project_with_read(
        outbox_path,
        catalog,
        permissions,
        destination,
        |uri, owner, turn| {
            // The fixed MCP adapter enforces its caller's exact conversation
            // namespace. The grant was verified before this callback; read
            // with the source owner, NEVER the destination transport identity.
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
    mut read: impl FnMut(&str, &str, &str) -> Result<Value, String>,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    let outbox = crate::openviking_checkpoint_outbox::Outbox::load(outbox_path)?;
    let sources = confirmed_project_sources(&outbox, catalog, permissions, destination);
    let mut used_bytes = 0usize;
    let mut result = Vec::with_capacity(sources.len());
    for entry in sources {
        let owner = entry.conversation_id.as_str();
        let path = crate::openviking_checkpoint_outbox::Outbox::summary_path(
            outbox_path,
            owner,
            &entry.turn_id,
        )?;
        let size = std::fs::metadata(&path)
            .map_err(|error| format!("project checkpoint metadata: {error}"))?
            .len();
        if size == 0 || size > MAX_CHECKPOINT_BYTES as u64 {
            return Err("invalid project checkpoint summary size".to_owned());
        }
        if size > (MAX_RETRIEVAL_BYTES - used_bytes) as u64 {
            continue;
        }
        let expected = std::fs::read_to_string(&path)
            .map_err(|error| format!("project checkpoint source unavailable: {error}"))?;
        if expected.len() != size as usize
            || !expected.starts_with(&format!(
                "# Chatarium memory checkpoint\n\nConversation: {owner}\nTurn: {}\nSource journal sequence: {}\n\n",
                entry.turn_id, entry.completed_sequence,
            ))
        {
            return Err("project checkpoint source identity mismatch".to_owned());
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
mod project_scope_tests {
    use super::*;
    use crate::local_conversations::LocalConversationCatalog;
    use crate::openviking_checkpoint_outbox::{Outbox, State};

    #[test]
    fn project_binding_does_not_implicitly_grant_memory_sharing() {
        let first = chatarium_core::LocalConversationId::new();
        let second = chatarium_core::LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        let directory = std::env::current_dir().unwrap().display().to_string();
        catalog
            .set_project_directory(first, Some(directory.clone()), 3)
            .unwrap();
        catalog
            .set_project_directory(second, Some(directory), 4)
            .unwrap();
        let mut permissions = ProjectMemoryGrants::default();
        assert!(!permissions.allows(&catalog, first, second));
        permissions.grant(&catalog, first, second).unwrap();
        assert!(permissions.allows(&catalog, first, second));
        assert!(!permissions.allows(&catalog, second, first));
        assert!(permissions.revoke(first, second));
        assert!(!permissions.allows(&catalog, first, second));
    }

    #[test]
    fn project_changes_fail_closed_and_cannot_create_cross_project_grants() {
        let first = chatarium_core::LocalConversationId::new();
        let second = chatarium_core::LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        let directory = std::env::current_dir().unwrap().display().to_string();
        catalog
            .set_project_directory(first, Some(directory.clone()), 3)
            .unwrap();
        catalog
            .set_project_directory(second, Some(directory), 4)
            .unwrap();
        let mut permissions = ProjectMemoryGrants::default();
        permissions.grant(&catalog, first, second).unwrap();
        catalog
            .set_project_directory(second, Some(std::env::temp_dir().display().to_string()), 5)
            .unwrap();
        assert!(!permissions.allows(&catalog, first, second));
        assert!(permissions.grant(&catalog, first, second).is_err());
    }

    #[test]
    fn only_latest_confirmed_per_authorized_source_is_selected() {
        let first = chatarium_core::LocalConversationId::new();
        let second = chatarium_core::LocalConversationId::new();
        let third = chatarium_core::LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        for (id, n) in [(first, 1), (second, 2), (third, 3)] {
            catalog.create(id, n);
            catalog
                .set_project_directory(
                    id,
                    Some(std::env::current_dir().unwrap().display().to_string()),
                    n + 10,
                )
                .unwrap();
        }
        let mut permissions = ProjectMemoryGrants::default();
        permissions.grant(&catalog, first, third).unwrap();
        let mut outbox = Outbox::default();
        // Selection checks only durable state; remote calls are not needed.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-scope-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        outbox.activate_durable(&path, 0).unwrap();
        for (owner, turn, seq, confirmed) in [
            (first, "old", 1, true),
            (second, "foreign", 2, true),
            (first, "uncertain", 3, false),
            (first, "new", 4, true),
        ] {
            let owner = owner.to_string();
            outbox.enqueue_durable(&path, &owner, turn, seq).unwrap();
            outbox
                .save_summary_durable(&path, &owner, turn, seq, "summary")
                .unwrap();
            if confirmed {
                outbox
                    .transition_durable(&path, &owner, turn, seq, State::Ready, State::Dispatched)
                    .unwrap();
                outbox
                    .transition_durable(
                        &path,
                        &owner,
                        turn,
                        seq,
                        State::Dispatched,
                        State::Confirmed,
                    )
                    .unwrap();
            }
        }
        let chosen = confirmed_project_sources(&outbox, &catalog, &permissions, third);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].turn_id, "new");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_scoped_transport_read_cannot_impersonate_destination() {
        let source = chatarium_core::LocalConversationId::new().to_string();
        let destination = chatarium_core::LocalConversationId::new().to_string();
        let turn = "verified-turn";
        let uri = checkpoint_uri(&source, turn).unwrap();
        let mut recorded = Vec::new();
        let value = read_project_checkpoint_as_source(
            &source,
            turn,
            &uri,
            |identity, call_key, provider, operation, args| {
                recorded.push((
                    identity.to_owned(),
                    call_key.to_owned(),
                    provider.to_owned(),
                    operation.to_owned(),
                    args.clone(),
                ));
                Ok(json!({"content": [{"type":"text", "text":"exact"}]}))
            },
        )
        .unwrap();
        assert!(readback_matches(&value, "exact"));
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, source);
        assert_ne!(recorded[0].0, destination);
        assert_eq!(
            recorded[0].1,
            format!("memory-project-read:{source}:{turn}")
        );
        assert_eq!(recorded[0].2, local_http_mcp::OPENVIKING);
        assert_eq!(recorded[0].3, "read");
        assert_eq!(recorded[0].4, json!({"uris":uri,"limit":100}));

        // Forged owner/URI pairs must be rejected before any transport call.
        let mut invoked = false;
        assert!(
            read_project_checkpoint_as_source(&destination, turn, &uri, |_, _, _, _, _| {
                invoked = true;
                Ok(Value::Null)
            },)
            .is_err()
        );
        assert!(!invoked);
    }

    /// Exercises durable checkpoint creation, exact URI retrieval, explicit
    /// cross-chat permission, model context admission, revocation and restart
    /// with a fake OpenViking read backend. Never contacts a real service.
    #[test]
    fn project_checkpoint_fake_backend_roundtrip_and_revocation() {
        use std::collections::BTreeMap;
        let source = chatarium_core::LocalConversationId::new();
        let destination = chatarium_core::LocalConversationId::new();
        let unrelated = chatarium_core::LocalConversationId::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-roundtrip-{}-{nonce}",
            std::process::id()
        ));
        let shared = root.join("shared");
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        let mut catalog = LocalConversationCatalog::default();
        for (id, dir) in [
            (source, &shared),
            (destination, &shared),
            (unrelated, &foreign),
        ] {
            catalog.create(id, 1);
            catalog
                .set_project_directory(id, Some(dir.display().to_string()), 2)
                .unwrap();
        }
        let outbox_path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let grant_path = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&outbox_path, 0).unwrap();

        let mut fake_server = BTreeMap::<String, String>::new();
        for (owner, turn, sequence, confirmed) in [
            (source, "older", 10, true),
            (source, "latest", 12, true),
            (source, "uncertain", 14, false),
            (unrelated, "foreign-secret", 99, true),
        ] {
            let identity = owner.to_string();
            outbox
                .enqueue_durable(&outbox_path, &identity, turn, sequence)
                .unwrap();
            outbox
                .save_summary_durable(
                    &outbox_path,
                    &identity,
                    turn,
                    sequence,
                    "Durable project fact: Rust client",
                )
                .unwrap();
            if confirmed {
                outbox
                    .transition_durable(
                        &outbox_path,
                        &identity,
                        turn,
                        sequence,
                        State::Ready,
                        State::Dispatched,
                    )
                    .unwrap();
                outbox
                    .transition_durable(
                        &outbox_path,
                        &identity,
                        turn,
                        sequence,
                        State::Dispatched,
                        State::Confirmed,
                    )
                    .unwrap();
                let path = Outbox::summary_path(&outbox_path, &identity, turn).unwrap();
                let uri = checkpoint_uri(&identity, turn).unwrap();
                fake_server.insert(uri, std::fs::read_to_string(path).unwrap());
            }
        }
        let mut grants = ProjectMemoryGrants::default();
        let mut attempted = Vec::new();
        let before_grant = retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &grants,
            destination,
            |uri, _, _| {
                attempted.push(uri.to_owned());
                Err("a read must never occur without a grant".to_owned())
            },
        )
        .unwrap();
        assert!(before_grant.is_empty());
        assert!(attempted.is_empty());

        grants.grant(&catalog, source, destination).unwrap();
        grants.save_atomic(&grant_path).unwrap();
        let mut grants = ProjectMemoryGrants::load(&grant_path).unwrap();
        let mut observed = Vec::new();
        let retrieved = retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &grants,
            destination,
            |uri, owner, turn| {
                observed.push((uri.to_owned(), owner.to_owned(), turn.to_owned()));
                let remote = fake_server.get(uri).expect("fake OpenViking immutable URI");
                Ok(json!({"content": [{"type": "text", "text": remote}]}))
            },
        )
        .unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].source_conversation_id, source.to_string());
        assert_eq!(retrieved[0].source_turn_id, "latest");
        assert_eq!(retrieved[0].source_journal_sequence, 12);
        assert_eq!(
            observed,
            vec![(
                checkpoint_uri(&source.to_string(), "latest").unwrap(),
                source.to_string(),
                "latest".to_owned(),
            )]
        );
        assert!(!retrieved[0].text.contains("foreign-secret"));
        let admitted =
            crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
                &retrieved[0],
                destination,
                &catalog,
                &grants,
            )
            .unwrap();
        assert!(admitted.text.contains("Durable project fact: Rust client"));
        assert!(admitted.text.contains("explicitly granted project-shared"));
        assert!(
            crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
                &retrieved[0],
                unrelated,
                &catalog,
                &grants,
            )
            .is_err()
        );

        // A successful read may remain cached after explicit revocation.
        // Context assembly must re-check authorization rather than trust it.
        assert!(grants.revoke(source, destination));
        grants.save_atomic(&grant_path).unwrap();
        let revoked = ProjectMemoryGrants::load(&grant_path).unwrap();
        assert!(
            crate::context_composer::TranscriptMessage::verified_project_openviking_checkpoint(
                &retrieved[0],
                destination,
                &catalog,
                &revoked,
            )
            .is_err()
        );
        let mut calls = 0;
        let after_revocation = retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &revoked,
            destination,
            |_, _, _| {
                calls += 1;
                Err("revoked read".to_owned())
            },
        )
        .unwrap();
        assert!(after_revocation.is_empty());
        assert_eq!(calls, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fake_backend_rejects_remote_drift_and_offline_errors() {
        let source = chatarium_core::LocalConversationId::new();
        let destination = chatarium_core::LocalConversationId::new();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-drift-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut catalog = LocalConversationCatalog::default();
        for id in [source, destination] {
            catalog.create(id, 1);
            catalog
                .set_project_directory(id, Some(root.display().to_string()), 2)
                .unwrap();
        }
        let outbox_path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
        let mut outbox = Outbox::default();
        outbox.activate_durable(&outbox_path, 0).unwrap();
        let owner = source.to_string();
        outbox
            .enqueue_durable(&outbox_path, &owner, "first", 1)
            .unwrap();
        outbox
            .save_summary_durable(&outbox_path, &owner, "first", 1, "Correct memory")
            .unwrap();
        outbox
            .transition_durable(
                &outbox_path,
                &owner,
                "first",
                1,
                State::Ready,
                State::Dispatched,
            )
            .unwrap();
        outbox
            .transition_durable(
                &outbox_path,
                &owner,
                "first",
                1,
                State::Dispatched,
                State::Confirmed,
            )
            .unwrap();
        let mut grants = ProjectMemoryGrants::default();
        grants.grant(&catalog, source, destination).unwrap();

        let drift = retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &grants,
            destination,
            |_, _, _| Ok(json!({"content": [{"type": "text", "text": "Tampered"}]})),
        );
        assert!(drift.unwrap_err().contains("differs from local evidence"));

        let offline = retrieve_confirmed_for_project_with_read(
            &outbox_path,
            &catalog,
            &grants,
            destination,
            |_, _, _| Err("service is offline".to_owned()),
        );
        assert!(offline.unwrap_err().contains("offline"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn grants_roundtrip_and_reject_tampered_schema() {
        let first = chatarium_core::LocalConversationId::new();
        let second = chatarium_core::LocalConversationId::new();
        let mut catalog = LocalConversationCatalog::default();
        catalog.create(first, 1);
        catalog.create(second, 2);
        for id in [first, second] {
            catalog
                .set_project_directory(
                    id,
                    Some(std::env::current_dir().unwrap().display().to_string()),
                    3,
                )
                .unwrap();
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "chatarium-project-grants-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join(PROJECT_MEMORY_GRANTS_FILE_NAME);
        let mut grants = ProjectMemoryGrants::default();
        grants.grant(&catalog, first, second).unwrap();
        grants.save_atomic(&file).unwrap();
        let loaded = ProjectMemoryGrants::load(&file).unwrap();
        assert_eq!(grants, loaded);
        std::fs::write(&file, r#"{"schema":"other","version":1,"grants":[]}"#).unwrap();
        assert!(ProjectMemoryGrants::load(&file).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
