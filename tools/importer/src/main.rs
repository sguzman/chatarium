//! Import browser flight-recorder evidence into Chatarium's durable native journal.

use chatarium_core::EventKind;
use chatarium_store::{EventStore, JsonlEventStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const SOURCE_FORMAT: &str = "chatarium-flight-recorder-export";
const SOURCE_VERSION: u64 = 3;
const IMPORT_PAYLOAD_SCHEMA: &str = "chatarium-imported-observation";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chatarium-importer: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, export, data_dir] if command == "flight-recorder" => {
            import_file(Path::new(export), Path::new(data_dir))
        }
        [command, _export] if command == "flight-recorder" => Err(
            "refusing to import without an explicit data directory; usage: chatarium-importer flight-recorder <export.json> <data-dir>"
                .to_owned(),
        ),
        _ => {
            eprintln!("Usage:\n  chatarium-importer flight-recorder <export.json> <data-dir>");
            Err("invalid arguments".to_owned())
        }
    }
}

fn import_file(export_path: &Path, data_dir: &Path) -> Result<(), String> {
    let bytes = fs::read(export_path)
        .map_err(|error| format!("read {}: {error}", export_path.display()))?;
    let summary = import_bytes(&bytes, data_dir)?;
    println!("source sha256: {}", summary.sha256);
    println!("archive: {}", summary.archive_path.display());
    println!("journal: {}", summary.journal_path.display());
    println!("planned semantic events: {}", summary.planned_events);
    println!("appended this run: {}", summary.appended_events);
    println!("already durable: {}", summary.skipped_events);
    Ok(())
}

#[derive(Debug)]
struct ImportSummary {
    sha256: String,
    archive_path: PathBuf,
    journal_path: PathBuf,
    planned_events: usize,
    appended_events: usize,
    skipped_events: usize,
}

#[derive(Debug)]
struct PlannedEvent {
    scope: Option<String>,
    kind: EventKind,
    event_key: String,
    payload: String,
}

#[derive(Debug, Clone)]
struct SelectedMessage {
    source_index: usize,
    value: Value,
}

fn import_bytes(bytes: &[u8], data_dir: &Path) -> Result<ImportSummary, String> {
    let export: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse flight-recorder export JSON: {error}"))?;
    validate_export(&export)?;

    let sha256 = sha256_hex(bytes);
    let archive_path = archive_source(data_dir, &sha256, bytes)?;
    let journal_path = data_dir.join("journal.jsonl");
    let mut store = JsonlEventStore::open(&journal_path)
        .map_err(|error| format!("open {}: {error}", journal_path.display()))?;
    let mut existing = existing_import_keys(store.events(), &sha256);
    let plan = build_plan(&export, &sha256, &archive_path, data_dir)?;

    let mut appended_events = 0_usize;
    let mut skipped_events = 0_usize;
    for event in &plan {
        if existing.contains(&event.event_key) {
            skipped_events += 1;
            continue;
        }

        store
            .append_scoped(event.scope.clone(), event.kind, event.payload.clone())
            .map_err(|error| {
                format!(
                    "append import event '{}' to {}: {error}",
                    event.event_key,
                    journal_path.display()
                )
            })?;
        existing.insert(event.event_key.clone());
        appended_events += 1;
    }

    Ok(ImportSummary {
        sha256,
        archive_path,
        journal_path,
        planned_events: plan.len(),
        appended_events,
        skipped_events,
    })
}

fn validate_export(export: &Value) -> Result<(), String> {
    let format = export
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| "export is missing string field 'format'".to_owned())?;
    if format != SOURCE_FORMAT {
        return Err(format!("unsupported export format '{format}'"));
    }

    let version = export
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "export is missing integer field 'version'".to_owned())?;
    if version != SOURCE_VERSION {
        return Err(format!(
            "unsupported flight-recorder export version {version}; expected {SOURCE_VERSION}"
        ));
    }
    Ok(())
}

fn archive_source(data_dir: &Path, sha256: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let archive_dir = data_dir.join("imports").join("flight-recorder");
    fs::create_dir_all(&archive_dir)
        .map_err(|error| format!("create {}: {error}", archive_dir.display()))?;
    let archive_path = archive_dir.join(format!("{sha256}.json"));

    if archive_path.exists() {
        let existing = fs::read(&archive_path).map_err(|error| {
            format!("read existing archive {}: {error}", archive_path.display())
        })?;
        if sha256_hex(&existing) != sha256 {
            return Err(format!(
                "existing archive {} does not match its content-addressed filename",
                archive_path.display()
            ));
        }
        return Ok(archive_path);
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive_path)
        .map_err(|error| format!("create archive {}: {error}", archive_path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write archive {}: {error}", archive_path.display()))?;
    file.flush()
        .map_err(|error| format!("flush archive {}: {error}", archive_path.display()))?;
    file.sync_data()
        .map_err(|error| format!("sync archive {}: {error}", archive_path.display()))?;
    Ok(archive_path)
}

fn existing_import_keys(
    events: &[chatarium_store::EventEnvelope],
    sha256: &str,
) -> HashSet<String> {
    events
        .iter()
        .filter_map(|event| serde_json::from_str::<Value>(&event.payload).ok())
        .filter(|payload| payload.pointer("/source/sha256").and_then(Value::as_str) == Some(sha256))
        .filter_map(|payload| {
            payload
                .pointer("/source/event_key")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect()
}

fn build_plan(
    export: &Value,
    sha256: &str,
    archive_path: &Path,
    data_dir: &Path,
) -> Result<Vec<PlannedEvent>, String> {
    let mut body = Vec::new();
    let archive_relative = archive_path
        .strip_prefix(data_dir)
        .unwrap_or(archive_path)
        .to_string_lossy()
        .replace('\\', "/");

    let canonical_scopes = canonical_scope_by_observed_id(export);

    let mut draft_pairs = HashSet::new();
    if let Some(draft) = export.get("draftWal").filter(|value| !value.is_null()) {
        if let Some(event) = draft_event(draft, sha256, "draft-wal")? {
            if let Some(text) = payload_text(&event.payload) {
                draft_pairs.insert((event.scope.clone(), text));
            }
            body.push(event);
        }
    }
    if let Some(drafts) = export.get("drafts").and_then(Value::as_array) {
        for (index, draft) in drafts.iter().enumerate() {
            if let Some(event) = draft_event(draft, sha256, &format!("draft-store:{index}"))? {
                let pair = payload_text(&event.payload).map(|text| (event.scope.clone(), text));
                if pair.as_ref().is_some_and(|pair| draft_pairs.contains(pair)) {
                    continue;
                }
                if let Some(pair) = pair {
                    draft_pairs.insert(pair);
                }
                body.push(event);
            }
        }
    }

    if let Some(intents) = export.get("sendIntents").and_then(Value::as_array) {
        for (index, intent) in intents.iter().enumerate() {
            body.extend(send_intent_events(
                intent,
                index,
                sha256,
                &canonical_scopes,
            )?);
        }
    }

    let selected_messages = selected_transcript_messages(export);
    let mut assistant_message_fingerprints = HashSet::new();
    for selected in selected_messages {
        if let Some(event) = transcript_event(
            &selected.value,
            selected.source_index,
            sha256,
            &canonical_scopes,
        )? {
            if event.kind == EventKind::AssistantSnapshotObserved {
                assistant_message_fingerprints.insert(assistant_fingerprint(&selected.value));
            }
            body.push(event);
        }
    }

    if let Some(assistant) = export.get("assistantWal").filter(|value| !value.is_null()) {
        if !assistant_message_fingerprints.contains(&assistant_fingerprint(assistant)) {
            if let Some(event) = assistant_wal_event(assistant, sha256, &canonical_scopes)? {
                body.push(event);
            }
        }
    }

    if let Some(error) = export
        .get("lastVisibleError")
        .filter(|value| !value.is_null())
    {
        if let Some(event) = visible_error_event(error, sha256)? {
            body.push(event);
        }
    }

    let source_counts = json!({
        "events": array_len(export, "events"),
        "drafts": array_len(export, "drafts"),
        "messages": array_len(export, "messages"),
        "send_intents": array_len(export, "sendIntents"),
    });
    let start = PlannedEvent {
        scope: None,
        kind: EventKind::ImportStarted,
        event_key: "import:start".to_owned(),
        payload: import_payload(
            sha256,
            "import:start",
            None,
            None,
            export.get("exportedAt").and_then(Value::as_str),
            export.get("href").and_then(Value::as_str),
            json!({
                "recorder_version": export.get("recorderVersion").cloned().unwrap_or(Value::Null),
                "archive": archive_relative.clone(),
                "source_counts": source_counts,
            }),
        )?,
    };

    let semantic_count = body.len();
    let complete = PlannedEvent {
        scope: None,
        kind: EventKind::ImportCompleted,
        event_key: "import:complete".to_owned(),
        payload: import_payload(
            sha256,
            "import:complete",
            None,
            None,
            export.get("exportedAt").and_then(Value::as_str),
            export.get("href").and_then(Value::as_str),
            json!({
                "semantic_event_count": semantic_count,
                "archive": archive_relative,
            }),
        )?,
    };

    let mut plan = Vec::with_capacity(body.len() + 2);
    plan.push(start);
    plan.extend(body);
    plan.push(complete);
    Ok(plan)
}

fn canonical_scope_by_observed_id(export: &Value) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let Some(messages) = export.get("messages").and_then(Value::as_array) else {
        return result;
    };

    for message in messages {
        let Some(observed_id) = message.get("observedId").and_then(Value::as_str) else {
            continue;
        };
        let Some(scope) = message.get("conversation").and_then(Value::as_str) else {
            continue;
        };
        if observed_id.starts_with("request-placeholder-") {
            continue;
        }

        match result.get(observed_id) {
            Some(existing) if scope_rank(existing) >= scope_rank(scope) => {}
            _ => {
                result.insert(observed_id.to_owned(), scope.to_owned());
            }
        }
    }

    result
}

fn selected_transcript_messages(export: &Value) -> Vec<SelectedMessage> {
    let Some(messages) = export.get("messages").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut selected: HashMap<String, SelectedMessage> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = message
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let observed_id = message
            .get("observedId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key = message
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let identity = if observed_id.is_empty() {
            key
        } else {
            observed_id
        };
        let fingerprint = format!("{role}:{identity}:{}", sha256_hex(text.as_bytes()));
        let scope = message
            .get("conversation")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let candidate = SelectedMessage {
            source_index: index,
            value: message.clone(),
        };

        match selected.get(&fingerprint) {
            Some(existing) => {
                let existing_scope = existing
                    .value
                    .get("conversation")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if scope_rank(scope) > scope_rank(existing_scope) {
                    selected.insert(fingerprint, candidate);
                }
            }
            None => {
                selected.insert(fingerprint, candidate);
            }
        }
    }

    let mut values = selected.into_values().collect::<Vec<_>>();
    values.sort_by_key(|message| message.source_index);
    values
}

fn scope_rank(scope: &str) -> u8 {
    if scope.starts_with("conversation:WEB:") {
        1
    } else if scope.starts_with("conversation:") {
        2
    } else {
        0
    }
}

fn canonical_scope_for(
    observed_id: Option<&str>,
    fallback: Option<&str>,
    canonical_scopes: &HashMap<String, String>,
) -> Option<String> {
    observed_id
        .and_then(|id| canonical_scopes.get(id).cloned())
        .or_else(|| fallback.map(ToOwned::to_owned))
}

fn draft_event(
    draft: &Value,
    sha256: &str,
    key_prefix: &str,
) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = draft.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let scope = draft
        .get("conversation")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let scope_key = scope.as_deref().unwrap_or("unscoped");
    let event_key = format!("{key_prefix}:{scope_key}");
    Ok(Some(PlannedEvent {
        scope: scope.clone(),
        kind: EventKind::DraftChanged,
        event_key: event_key.clone(),
        payload: import_payload(
            sha256,
            &event_key,
            Some(text),
            scope.as_deref(),
            draft.get("at").and_then(Value::as_str),
            draft.get("href").and_then(Value::as_str),
            json!({
                "source_kind": draft.get("kind").cloned().unwrap_or(Value::Null),
                "reason": draft.get("reason").cloned().unwrap_or(Value::Null),
            }),
        )?,
    }))
}

fn send_intent_events(
    intent: &Value,
    index: usize,
    sha256: &str,
    canonical_scopes: &HashMap<String, String>,
) -> Result<Vec<PlannedEvent>, String> {
    let Some(text) = intent.get("text").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    let id = intent
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("index-{index}"), ToOwned::to_owned);
    let observed_id = intent.get("observedMessageId").and_then(Value::as_str);
    let fallback_scope = intent
        .get("confirmedConversation")
        .or_else(|| intent.get("conversation"))
        .and_then(Value::as_str);
    let scope = canonical_scope_for(observed_id, fallback_scope, canonical_scopes);
    let at = intent.get("at").and_then(Value::as_str);
    let href = intent.get("href").and_then(Value::as_str);
    let state = intent
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let details = json!({
        "send_id": id.clone(),
        "state": state,
        "reason": intent.get("reason").cloned().unwrap_or(Value::Null),
        "origin_conversation": intent.get("conversation").cloned().unwrap_or(Value::Null),
        "confirmed_conversation": intent.get("confirmedConversation").cloned().unwrap_or(Value::Null),
        "canonical_conversation": scope.clone(),
        "confirmed_at": intent.get("confirmedAt").cloned().unwrap_or(Value::Null),
        "observed_message_id": intent.get("observedMessageId").cloned().unwrap_or(Value::Null),
    });

    let commit_key = format!("send:{id}:commit");
    let dispatch_key = format!("send:{id}:dispatch");
    let mut events = vec![
        PlannedEvent {
            scope: scope.clone(),
            kind: EventKind::UserMessageCommitted,
            event_key: commit_key.clone(),
            payload: import_payload(
                sha256,
                &commit_key,
                Some(text),
                scope.as_deref(),
                at,
                href,
                details.clone(),
            )?,
        },
        PlannedEvent {
            scope: scope.clone(),
            kind: EventKind::DispatchAttempted,
            event_key: dispatch_key.clone(),
            payload: import_payload(
                sha256,
                &dispatch_key,
                Some(text),
                scope.as_deref(),
                at,
                href,
                details.clone(),
            )?,
        },
    ];

    if state == "confirmed" {
        let acceptance_key = format!("send:{id}:acceptance");
        events.push(PlannedEvent {
            scope: scope.clone(),
            kind: EventKind::RemoteAcceptanceObserved,
            event_key: acceptance_key.clone(),
            payload: import_payload(
                sha256,
                &acceptance_key,
                Some(text),
                scope.as_deref(),
                intent
                    .get("confirmedAt")
                    .or_else(|| intent.get("at"))
                    .and_then(Value::as_str),
                href,
                details,
            )?,
        });
    }

    Ok(events)
}

fn transcript_event(
    message: &Value,
    index: usize,
    sha256: &str,
    canonical_scopes: &HashMap<String, String>,
) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = message.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(None);
    };
    let observed_id = message.get("observedId").and_then(Value::as_str);
    let is_placeholder =
        role == "assistant" && observed_id.is_some_and(|id| id.starts_with("request-placeholder-"));
    let kind = match role {
        "user" => EventKind::TranscriptUserMessageObserved,
        "assistant" if is_placeholder => EventKind::AssistantStatusObserved,
        "assistant" => EventKind::AssistantSnapshotObserved,
        _ => return Ok(None),
    };
    let source_key = observed_id
        .map(ToOwned::to_owned)
        .or_else(|| {
            message
                .get("key")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| format!("index-{index}"));
    let event_key = format!(
        "message:{}:{}:{}",
        role,
        source_key,
        sha256_hex(text.as_bytes())
    );
    let fallback_scope = message.get("conversation").and_then(Value::as_str);
    let scope = canonical_scope_for(observed_id, fallback_scope, canonical_scopes);

    Ok(Some(PlannedEvent {
        scope: scope.clone(),
        kind,
        event_key: event_key.clone(),
        payload: import_payload(
            sha256,
            &event_key,
            Some(text),
            scope.as_deref(),
            message.get("observedAt").and_then(Value::as_str),
            message.get("href").and_then(Value::as_str),
            json!({
                "role": role,
                "observed_id": observed_id,
                "content_hash": message.get("contentHash").cloned().unwrap_or(Value::Null),
                "index": message.get("index").cloned().unwrap_or(Value::Null),
                "source_conversation": fallback_scope,
                "transient_placeholder": is_placeholder,
            }),
        )?,
    }))
}

fn assistant_wal_event(
    assistant: &Value,
    sha256: &str,
    canonical_scopes: &HashMap<String, String>,
) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = assistant.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let observed_id = assistant.get("observedId").and_then(Value::as_str);
    let fallback_scope = assistant.get("conversation").and_then(Value::as_str);
    let scope = canonical_scope_for(observed_id, fallback_scope, canonical_scopes);
    let is_placeholder = observed_id.is_some_and(|id| id.starts_with("request-placeholder-"));
    let event_key = format!(
        "assistant-wal:{}:{}:{}",
        scope.as_deref().unwrap_or("unscoped"),
        observed_id.unwrap_or("no-id"),
        sha256_hex(text.as_bytes())
    );
    Ok(Some(PlannedEvent {
        scope: scope.clone(),
        kind: if is_placeholder {
            EventKind::AssistantStatusObserved
        } else {
            EventKind::AssistantSnapshotObserved
        },
        event_key: event_key.clone(),
        payload: import_payload(
            sha256,
            &event_key,
            Some(text),
            scope.as_deref(),
            assistant.get("at").and_then(Value::as_str),
            assistant.get("href").and_then(Value::as_str),
            json!({
                "observed_id": observed_id,
                "original_chars": assistant.get("originalChars").cloned().unwrap_or(Value::Null),
                "truncated_prefix": assistant.get("truncatedPrefix").cloned().unwrap_or(Value::Null),
                "source": "assistant-wal",
                "transient_placeholder": is_placeholder,
            }),
        )?,
    }))
}

fn visible_error_event(error: &Value, sha256: &str) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = error.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let scope = error
        .get("conversation")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let event_key = format!(
        "visible-error:{}:{}",
        scope.as_deref().unwrap_or("unscoped"),
        sha256_hex(text.as_bytes())
    );
    Ok(Some(PlannedEvent {
        scope: scope.clone(),
        kind: EventKind::ClientErrorObserved,
        event_key: event_key.clone(),
        payload: import_payload(
            sha256,
            &event_key,
            Some(text),
            scope.as_deref(),
            error.get("at").and_then(Value::as_str),
            error.get("href").and_then(Value::as_str),
            json!({
                "truncated": error.get("truncated").cloned().unwrap_or(Value::Null),
            }),
        )?,
    }))
}

fn import_payload(
    sha256: &str,
    event_key: &str,
    text: Option<&str>,
    conversation: Option<&str>,
    at: Option<&str>,
    href: Option<&str>,
    details: Value,
) -> Result<String, String> {
    serde_json::to_string(&json!({
        "schema": IMPORT_PAYLOAD_SCHEMA,
        "version": 1,
        "text": text,
        "source": {
            "format": SOURCE_FORMAT,
            "version": SOURCE_VERSION,
            "sha256": sha256,
            "event_key": event_key,
            "at": at,
            "conversation": conversation,
            "href": href,
        },
        "details": details,
    }))
    .map_err(|error| format!("serialize imported event payload: {error}"))
}

fn payload_text(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn assistant_fingerprint(value: &Value) -> String {
    let observed_id = value
        .get("observedId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("{observed_id}:{}", sha256_hex(text.as_bytes()))
}

fn array_len(value: &Value, key: &str) -> usize {
    value.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        env::temp_dir().join(format!(
            "chatarium-importer-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn sample_export() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": SOURCE_FORMAT,
            "version": 3,
            "recorderVersion": "0.3.0",
            "exportedAt": "2026-09-17T18:00:00.000Z",
            "href": "https://chatgpt.com/c/final-conversation",
            "draftWal": {
                "at": "2026-09-17T17:59:00.000Z",
                "href": "https://chatgpt.com/c/final-conversation",
                "conversation": "conversation:final-conversation",
                "kind": "draft",
                "text": "unsent draft"
            },
            "sendIntents": [{
                "id": "send:test",
                "at": "2026-09-17T17:58:00.000Z",
                "href": "https://chatgpt.com/",
                "conversation": "route:/",
                "confirmedConversation": "conversation:WEB:temporary",
                "reason": "composer-enter",
                "text": "hello",
                "state": "confirmed",
                "confirmedAt": "2026-09-17T17:58:01.000Z",
                "observedMessageId": "user-message-1"
            }],
            "assistantWal": {
                "at": "2026-09-17T17:58:02.000Z",
                "href": "https://chatgpt.com/c/final-conversation",
                "conversation": "conversation:final-conversation",
                "observedId": "assistant-message-1",
                "text": "world",
                "originalChars": 5,
                "truncatedPrefix": false
            },
            "lastVisibleError": null,
            "events": [],
            "drafts": [],
            "messages": [
                {
                    "key": "conversation:WEB:temporary:id:user-message-1",
                    "conversation": "conversation:WEB:temporary",
                    "href": "https://chatgpt.com/c/WEB:temporary",
                    "observedAt": "2026-09-17T17:58:01.000Z",
                    "observedId": "user-message-1",
                    "role": "user",
                    "index": 0,
                    "contentHash": "a",
                    "text": "hello"
                },
                {
                    "key": "conversation:WEB:temporary:id:request-placeholder-request-WEB:temporary-0",
                    "conversation": "conversation:WEB:temporary",
                    "href": "https://chatgpt.com/c/WEB:temporary",
                    "observedAt": "2026-09-17T17:58:01.100Z",
                    "observedId": "request-placeholder-request-WEB:temporary-0",
                    "role": "assistant",
                    "index": 1,
                    "contentHash": "p",
                    "text": "Thinking"
                },
                {
                    "key": "conversation:final-conversation:id:user-message-1",
                    "conversation": "conversation:final-conversation",
                    "href": "https://chatgpt.com/c/final-conversation",
                    "observedAt": "2026-09-17T17:58:01.500Z",
                    "observedId": "user-message-1",
                    "role": "user",
                    "index": 0,
                    "contentHash": "a",
                    "text": "hello"
                },
                {
                    "key": "conversation:final-conversation:id:assistant-message-1",
                    "conversation": "conversation:final-conversation",
                    "href": "https://chatgpt.com/c/final-conversation",
                    "observedAt": "2026-09-17T17:58:02.000Z",
                    "observedId": "assistant-message-1",
                    "role": "assistant",
                    "index": 1,
                    "contentHash": "b",
                    "text": "world"
                }
            ]
        }))
        .expect("serialize sample")
    }

    #[test]
    fn import_is_idempotent_and_preserves_scopes() {
        let dir = temp_dir("idempotent");
        let bytes = sample_export();
        let first = import_bytes(&bytes, &dir).expect("first import");
        assert!(first.appended_events > 0);
        assert_eq!(first.skipped_events, 0);

        let second = import_bytes(&bytes, &dir).expect("second import");
        assert_eq!(second.appended_events, 0);
        assert_eq!(second.skipped_events, second.planned_events);

        let store = JsonlEventStore::open(dir.join("journal.jsonl")).expect("reopen journal");
        assert!(store.events().iter().any(|event| {
            event.kind == EventKind::UserMessageCommitted
                && event.scope.as_deref() == Some("conversation:final-conversation")
        }));
        assert_eq!(
            store
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::TranscriptUserMessageObserved)
                .count(),
            1
        );
        assert_eq!(
            store
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::AssistantSnapshotObserved)
                .count(),
            1
        );
        assert_eq!(
            store
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::AssistantStatusObserved)
                .count(),
            1
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn transient_web_scope_is_replaced_by_canonical_scope() {
        let export: Value = serde_json::from_slice(&sample_export()).expect("parse sample");
        let scopes = canonical_scope_by_observed_id(&export);
        assert_eq!(
            scopes.get("user-message-1").map(String::as_str),
            Some("conversation:final-conversation")
        );
    }

    #[test]
    fn unsupported_export_version_is_rejected() {
        let export = serde_json::to_vec(&json!({
            "format": SOURCE_FORMAT,
            "version": 99
        }))
        .expect("serialize");
        let dir = temp_dir("version");
        let error = import_bytes(&export, &dir).expect_err("must reject version");
        assert!(error.contains("unsupported flight-recorder export version"));
        let _ = fs::remove_dir_all(dir);
    }
}
