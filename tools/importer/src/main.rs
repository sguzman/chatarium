use chatarium_core::EventKind;
use chatarium_store::{EventStore, JsonlEventStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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
        [command, export] if command == "flight-recorder" => {
            import_file(Path::new(export), &default_data_dir())
        }
        [command, export, data_dir] if command == "flight-recorder" => {
            import_file(Path::new(export), Path::new(data_dir))
        }
        _ => {
            eprintln!("Usage:\n  chatarium-importer flight-recorder <export.json> [data-dir]");
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
            body.extend(send_intent_events(intent, index, sha256)?);
        }
    }

    let mut assistant_message_fingerprints = HashSet::new();
    if let Some(messages) = export.get("messages").and_then(Value::as_array) {
        for (index, message) in messages.iter().enumerate() {
            if let Some(event) = transcript_event(message, index, sha256)? {
                if event.kind == EventKind::AssistantSnapshotObserved {
                    assistant_message_fingerprints.insert(assistant_fingerprint(message));
                }
                body.push(event);
            }
        }
    }

    if let Some(assistant) = export.get("assistantWal").filter(|value| !value.is_null()) {
        if !assistant_message_fingerprints.contains(&assistant_fingerprint(assistant)) {
            if let Some(event) = assistant_wal_event(assistant, sha256)? {
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
) -> Result<Vec<PlannedEvent>, String> {
    let Some(text) = intent.get("text").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    let id = intent
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("index-{index}"), ToOwned::to_owned);
    let scope = intent
        .get("confirmedConversation")
        .or_else(|| intent.get("conversation"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
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
            scope,
            kind: EventKind::RemoteAcceptanceObserved,
            event_key: acceptance_key.clone(),
            payload: import_payload(
                sha256,
                &acceptance_key,
                Some(text),
                intent
                    .get("confirmedConversation")
                    .or_else(|| intent.get("conversation"))
                    .and_then(Value::as_str),
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
) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = message.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(role) = message.get("role").and_then(Value::as_str) else {
        return Ok(None);
    };
    let kind = match role {
        "user" => EventKind::TranscriptUserMessageObserved,
        "assistant" => EventKind::AssistantSnapshotObserved,
        _ => return Ok(None),
    };
    let source_key = message
        .get("key")
        .and_then(Value::as_str)
        .map_or_else(|| format!("index-{index}"), ToOwned::to_owned);
    let event_key = format!("message:{role}:{source_key}");
    let scope = message
        .get("conversation")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

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
                "observed_id": message.get("observedId").cloned().unwrap_or(Value::Null),
                "content_hash": message.get("contentHash").cloned().unwrap_or(Value::Null),
                "index": message.get("index").cloned().unwrap_or(Value::Null),
            }),
        )?,
    }))
}

fn assistant_wal_event(assistant: &Value, sha256: &str) -> Result<Option<PlannedEvent>, String> {
    let Some(text) = assistant.get("text").and_then(Value::as_str) else {
        return Ok(None);
    };
    let scope = assistant
        .get("conversation")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let observed_id = assistant.get("observedId").and_then(Value::as_str);
    let event_key = format!(
        "assistant-wal:{}:{}",
        scope.as_deref().unwrap_or("unscoped"),
        observed_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| sha256_hex(text.as_bytes()))
    );
    Ok(Some(PlannedEvent {
        scope: scope.clone(),
        kind: EventKind::AssistantSnapshotObserved,
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

fn default_data_dir() -> PathBuf {
    if let Some(override_dir) = env::var_os("CHATARIUM_DATA_DIR") {
        return PathBuf::from(override_dir);
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join("Chatarium");
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".chatarium")
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
            "href": "https://chatgpt.com/c/test-conversation",
            "draftWal": {
                "at": "2026-09-17T17:59:00.000Z",
                "href": "https://chatgpt.com/c/test-conversation",
                "conversation": "conversation:test-conversation",
                "kind": "draft",
                "text": "unsent draft"
            },
            "sendIntents": [{
                "id": "send:test",
                "at": "2026-09-17T17:58:00.000Z",
                "href": "https://chatgpt.com/",
                "conversation": "route:/",
                "confirmedConversation": "conversation:test-conversation",
                "reason": "composer-enter",
                "text": "hello",
                "state": "confirmed",
                "confirmedAt": "2026-09-17T17:58:01.000Z",
                "observedMessageId": "user-message-1"
            }],
            "assistantWal": {
                "at": "2026-09-17T17:58:02.000Z",
                "href": "https://chatgpt.com/c/test-conversation",
                "conversation": "conversation:test-conversation",
                "observedId": "assistant-message-1",
                "text": "world",
                "originalChars": 5,
                "truncatedPrefix": false
            },
            "lastVisibleError": {
                "at": "2026-09-17T17:58:03.000Z",
                "href": "https://chatgpt.com/c/test-conversation",
                "conversation": "conversation:test-conversation",
                "text": "Message delivery timed out",
                "truncated": false
            },
            "events": [],
            "drafts": [],
            "messages": [
                {
                    "key": "conversation:test-conversation:id:user-message-1",
                    "conversation": "conversation:test-conversation",
                    "href": "https://chatgpt.com/c/test-conversation",
                    "observedAt": "2026-09-17T17:58:01.000Z",
                    "observedId": "user-message-1",
                    "role": "user",
                    "index": 0,
                    "contentHash": "a",
                    "text": "hello"
                },
                {
                    "key": "conversation:test-conversation:id:assistant-message-1",
                    "conversation": "conversation:test-conversation",
                    "href": "https://chatgpt.com/c/test-conversation",
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
                && event.scope.as_deref() == Some("conversation:test-conversation")
        }));
        assert!(
            store
                .events()
                .iter()
                .any(|event| event.kind == EventKind::ClientErrorObserved)
        );
        assert_eq!(
            store
                .events()
                .iter()
                .filter(|event| event.kind == EventKind::AssistantSnapshotObserved)
                .count(),
            1
        );

        let _ = fs::remove_dir_all(dir);
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
