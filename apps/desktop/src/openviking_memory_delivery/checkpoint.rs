//! Single-owner at-most-once checkpoint delivery and checked read-back.
use super::*;

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

// Asynchronous OpenViking create returns before the checkpoint is guaranteed
// readable. Only read-only verification may be repeated: a second create could
// duplicate an uncertain external side effect.
const VERIFY_READ_ATTEMPTS: usize = 5;

pub fn deliver_create_once(conversation: &str, turn: &str, content: &str) -> Result<(), String> {
    deliver_create_once_with(
        conversation,
        turn,
        content,
        |owner, key, provider, operation, args| {
            if provider == local_http_mcp::OPENVIKING && operation == "write" {
                // The sole production write route is host-owned, not a
                // model-approved generic MCP invocation.
                local_http_mcp::invoke_checkpoint_write(owner, turn, content)
            } else {
                local_http_mcp::invoke(owner, key, provider, operation, args)
            }
        },
        std::thread::sleep,
    )
}

/// Injectable transport and pause make the real at-most-once dispatch rule
/// testable without contacting or mutating the user's OpenViking instance.
pub(super) fn deliver_create_once_with(
    conversation: &str,
    turn: &str,
    content: &str,
    mut invoke: impl FnMut(&str, &str, &str, &str, &Value) -> Result<Value, String>,
    mut pause: impl FnMut(std::time::Duration),
) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_CHECKPOINT_BYTES {
        return Err("OpenViking checkpoint content is empty or too large".to_owned());
    }
    let uri = checkpoint_uri(conversation, turn)?;
    let identity = format!("memory-create:{conversation}:{turn}");
    let write_result = invoke(
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
    if write_result.get("isError") == Some(&Value::Bool(true)) {
        return Err("OpenViking create returned a tool error; outcome uncertain".to_owned());
    }
    // The write was attempted exactly once. Delayed materialization, transport
    // errors and temporary read mismatches may be resolved by safe read-only
    // checks; none can authorize a write retry or an unverified confirmation.
    for attempt in 0..VERIFY_READ_ATTEMPTS {
        let readback = invoke(
            conversation,
            &format!("memory-verify:{conversation}:{turn}:{attempt}"),
            local_http_mcp::OPENVIKING,
            "read",
            &json!({"uris": uri, "limit": 100}),
        );
        if readback
            .as_ref()
            .is_ok_and(|response| readback_matches(response, content))
        {
            return Ok(());
        }
        if attempt + 1 < VERIFY_READ_ATTEMPTS {
            pause(std::time::Duration::from_millis(250_u64 << attempt));
        }
    }
    Err("OpenViking create was attempted once but exact read-back remains unconfirmed".to_owned())
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
    let expected = crate::openviking_checkpoint_outbox::Outbox::read_summary_evidence(
        outbox_path,
        conversation,
        turn,
        MAX_CHECKPOINT_BYTES,
    )?;
    if !expected.starts_with(&format!(
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

pub(super) const MAX_RETRIEVAL_ITEMS: usize = 3;
pub(super) const MAX_RETRIEVAL_BYTES: usize = 12 * 1024;
pub(super) const MAX_HISTORY_SCAN_ITEMS: usize = 64;
const MAX_QUERY_CHARS: usize = 4096;

/// Bounded local vocabulary, never a permission check or a remote search.
pub(crate) fn relevance_terms(text: &str) -> std::collections::BTreeSet<String> {
    const STOP: &[&str] = &[
        "about", "after", "also", "and", "are", "como", "con", "del", "does", "for", "from",
        "have", "how", "las", "los", "para", "por", "que", "the", "this", "una", "was", "were",
        "what", "when", "where", "which", "who", "will", "with", "you", "your",
    ];
    text.chars()
        .take(MAX_QUERY_CHARS)
        .collect::<String>()
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_lowercase)
        .filter(|word| !STOP.contains(&word.as_str()))
        .take(96)
        .collect()
}

/// Never score durable metadata (owner ID, turn ID, journal sequence): only
/// the lower-trust checkpoint body may match the local query.
pub(crate) fn checkpoint_relevance(
    checkpoint: &str,
    query: &std::collections::BTreeSet<String>,
) -> usize {
    let body = checkpoint.splitn(3, "\n\n").nth(2).unwrap_or("");
    query.intersection(&relevance_terms(body)).count()
}

/// An immutable confirmed summary must match the outbox's exact source
/// identity *before* any source is considered for retrieval or ranking.
pub(super) fn read_confirmed_evidence(
    outbox_path: &std::path::Path,
    entry: &crate::openviking_checkpoint_outbox::Entry,
) -> Result<String, String> {
    let expected = crate::openviking_checkpoint_outbox::Outbox::read_summary_evidence(
        outbox_path,
        &entry.conversation_id,
        &entry.turn_id,
        MAX_CHECKPOINT_BYTES,
    )?;
    if !expected.starts_with(&format!(
            "# Chatarium memory checkpoint\n\nConversation: {}\nTurn: {}\nSource journal sequence: {}\n\n",
            entry.conversation_id, entry.turn_id, entry.completed_sequence,
        ))
    {
        return Err("confirmed checkpoint summary identity mismatch".to_owned());
    }
    Ok(expected)
}

/// Keep the newest rolling checkpoint and, when the query has real matches,
/// allow up to two additional historical checkpoints to win over newer
/// irrelevant entries. Scan only the latest 64 confirmed entries for this
/// exact owner. The caller still verifies the selected immutable bytes against
/// OpenViking before model-context admission.
fn select_confirmed_history<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    outbox_path: &std::path::Path,
    conversation: &str,
    query: &str,
) -> Result<Vec<(&'a crate::openviking_checkpoint_outbox::Entry, String)>, String> {
    let candidates = confirmed_sources_limit(outbox, conversation, MAX_HISTORY_SCAN_ITEMS)?;
    let terms = relevance_terms(query);
    let mut scanned = Vec::with_capacity(candidates.len());
    for entry in candidates {
        let evidence = read_confirmed_evidence(outbox_path, entry)?;
        let score = checkpoint_relevance(&evidence, &terms);
        scanned.push((entry, evidence, score));
    }
    if !terms.is_empty() && scanned.iter().skip(1).any(|(_, _, score)| *score > 0) {
        // A latest rolling summary retains the authoritative continuity
        // chain even when the current question is about an older topic.
        let newest = scanned.remove(0);
        scanned.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| b.0.completed_sequence.cmp(&a.0.completed_sequence))
                .then_with(|| b.0.turn_id.cmp(&a.0.turn_id))
        });
        scanned.truncate(MAX_RETRIEVAL_ITEMS - 1);
        let mut selected = vec![newest];
        selected.extend(scanned);
        Ok(selected.into_iter().map(|(e, text, _)| (e, text)).collect())
    } else {
        scanned.truncate(MAX_RETRIEVAL_ITEMS);
        Ok(scanned.into_iter().map(|(e, text, _)| (e, text)).collect())
    }
}

/// Select recent confirmed checkpoints from exactly one conversation. No
/// implicit global search, shared namespace, or cross-conversation access.
fn confirmed_sources<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    conversation: &str,
) -> Result<Vec<&'a crate::openviking_checkpoint_outbox::Entry>, String> {
    confirmed_sources_limit(outbox, conversation, MAX_RETRIEVAL_ITEMS)
}

fn confirmed_sources_limit<'a>(
    outbox: &'a crate::openviking_checkpoint_outbox::Outbox,
    conversation: &str,
    limit: usize,
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
    entries.truncate(limit);
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
    retrieve_confirmed_for_conversation_query(outbox_path, conversation, "")
}

/// A bounded historical lookup within one authenticated owner. This is a
/// local lexical preselection, not a semantic index or a trust substitution.
pub fn retrieve_confirmed_for_conversation_query(
    outbox_path: &std::path::Path,
    conversation: &str,
    query: &str,
) -> Result<Vec<RetrievedCheckpoint>, String> {
    let outbox = crate::openviking_checkpoint_outbox::Outbox::load(outbox_path)?;
    let sources = select_confirmed_history(&outbox, outbox_path, conversation, query)?;
    let mut used_bytes = 0usize;
    let mut retrieved = Vec::with_capacity(sources.len());

    for (entry, expected) in sources {
        if expected.len() > MAX_RETRIEVAL_BYTES - used_bytes {
            continue;
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
mod tests;
