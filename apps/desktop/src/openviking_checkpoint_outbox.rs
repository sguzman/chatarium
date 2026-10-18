//! Durable, metadata-only requests for automatic OpenViking checkpoints.
//! No transcript or credentials are copied into this file. Generating a
//! checkpoint and sending it are deliberately separate, audited operations.
use serde_json::{Value, json};
use std::{fs, path::{Path, PathBuf}};

pub const FILE_NAME: &str = "openviking-checkpoint-outbox.json";
const SCHEMA: &str = "chatarium-openviking-checkpoint-outbox";
const MAX_ITEMS: usize = 4096;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub conversation_id: String,
    pub turn_id: String,
    pub completed_sequence: u64,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    PendingSummary,
    Ready,
    Dispatched,
    Confirmed,
    Uncertain,
    Skipped,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            Self::PendingSummary => "pending_summary",
            Self::Ready => "ready",
            Self::Dispatched => "dispatched",
            Self::Confirmed => "confirmed",
            Self::Uncertain => "uncertain",
            Self::Skipped => "skipped",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pending_summary" => Ok(Self::PendingSummary),
            "ready" => Ok(Self::Ready),
            "dispatched" => Ok(Self::Dispatched),
            "confirmed" => Ok(Self::Confirmed),
            "uncertain" => Ok(Self::Uncertain),
            "skipped" => Ok(Self::Skipped),
            _ => Err("invalid checkpoint state".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbox {
    schema: String,
    version: u32,
    // Baseline before any new completion can be appended. Older conversations
    // are not bulk exported on first installation.
    started_after_sequence: Option<u64>,
    entries: Vec<Entry>,
}

impl Default for Outbox {
    fn default() -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            version: 1,
            started_after_sequence: None,
            entries: Vec::new(),
        }
    }
}

fn identity_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

impl Outbox {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).map_err(|e| format!("checkpoint outbox read: {e}"))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("checkpoint outbox exceeds file limit".to_owned());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("checkpoint outbox JSON: {e}"))?;
        let root = value
            .as_object()
            .ok_or("checkpoint outbox root is not an object")?;
        if !(3..=4).contains(&root.len())
            || (root.len() == 4 && !root.contains_key("started_after_sequence"))
            || root.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || root.get("version").and_then(Value::as_u64) != Some(1)
        {
            return Err("invalid checkpoint outbox schema".to_owned());
        }
        let items = root
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("checkpoint outbox entries are missing")?;
        if items.len() > MAX_ITEMS {
            return Err("checkpoint outbox exceeds entry limit".to_owned());
        }
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let obj = item
                .as_object()
                .ok_or("checkpoint entry is not an object")?;
            if obj.len() != 4 {
                return Err("checkpoint entry contains unexpected fields".to_owned());
            }
            let conversation_id = obj
                .get("conversation_id")
                .and_then(Value::as_str)
                .ok_or("checkpoint owner identity missing")?
                .to_owned();
            let turn_id = obj
                .get("turn_id")
                .and_then(Value::as_str)
                .ok_or("checkpoint turn identity missing")?
                .to_owned();
            let completed_sequence = obj
                .get("completed_sequence")
                .and_then(Value::as_u64)
                .ok_or("checkpoint event sequence missing")?;
            let state = State::parse(
                obj.get("state")
                    .and_then(Value::as_str)
                    .ok_or("checkpoint state missing")?,
            )?;
            entries.push(Entry {
                conversation_id,
                turn_id,
                completed_sequence,
                state,
            });
        }
        let started_after_sequence = match root.get("started_after_sequence") {
            Some(value) if !value.is_null() => Some(
                value
                    .as_u64()
                    .ok_or("checkpoint baseline must be an integer")?,
            ),
            _ => None,
        };
        let outbox = Self {
            schema: SCHEMA.to_owned(),
            version: 1,
            started_after_sequence,
            entries,
        };
        outbox.validate()?;
        Ok(outbox)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA || self.version != 1 || self.entries.len() > MAX_ITEMS {
            return Err("invalid checkpoint outbox schema or size".to_owned());
        }
        let mut keys = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if !identity_valid(&entry.conversation_id)
                || !identity_valid(&entry.turn_id)
                || entry.completed_sequence == 0
                || !keys.insert((
                    entry.conversation_id.as_str(),
                    entry.turn_id.as_str(),
                    entry.completed_sequence,
                ))
            {
                return Err("duplicate or invalid checkpoint outbox entry".to_owned());
            }
        }
        Ok(())
    }

    fn save_atomic(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "conversation_id": entry.conversation_id,
                    "turn_id": entry.turn_id,
                    "completed_sequence": entry.completed_sequence,
                    "state": entry.state.name(),
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": self.schema,
            "version": self.version,
            "started_after_sequence": self.started_after_sequence,
            "entries": entries,
        }))
        .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("checkpoint outbox exceeds file limit".to_owned());
        }
        let parent = path.parent().ok_or("checkpoint outbox parent missing")?;
        fs::create_dir_all(parent).map_err(|e| format!("checkpoint outbox directory: {e}"))?;
        let temporary = path.with_extension("json.tmp");
        let mut file = fs::File::create(&temporary)
            .map_err(|e| format!("checkpoint outbox temporary file: {e}"))?;
        use std::io::Write;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("checkpoint outbox durable write: {e}"))?;
        fs::rename(&temporary, path)
            .map_err(|e| format!("checkpoint outbox atomic rename: {e}"))?;
        Ok(())
    }

    /// Initialize a durable first-run boundary before consuming new model
    /// completions. An existing file's checkpoint records are preserved.
    pub fn activate_durable(&mut self, path: &Path, journal_high_water: u64) -> Result<(), String> {
        if self.started_after_sequence.is_some() {
            return Ok(());
        }
        let mut changed = self.clone();
        changed.started_after_sequence = Some(journal_high_water);
        changed.save_atomic(path)?;
        *self = changed;
        Ok(())
    }

    pub fn started_after_sequence(&self) -> Option<u64> {
        self.started_after_sequence
    }

    /// Persist a memory-processing intent, not a successful memory write.
    /// A repeated delivery or process restart cannot duplicate the request.
    pub fn enqueue_durable(
        &mut self,
        path: &Path,
        conversation_id: &str,
        turn_id: &str,
        completed_sequence: u64,
    ) -> Result<bool, String> {
        if !identity_valid(conversation_id) || !identity_valid(turn_id) || completed_sequence == 0 {
            return Err("invalid checkpoint source identity".to_owned());
        }
        if self
            .entries
            .iter()
            .any(|entry| entry.conversation_id == conversation_id && entry.turn_id == turn_id)
        {
            return Ok(false);
        }
        if self.entries.len() >= MAX_ITEMS {
            return Err("checkpoint outbox is full; no work dropped".to_owned());
        }
        let mut changed = self.clone();
        changed.entries.push(Entry {
            conversation_id: conversation_id.to_owned(),
            turn_id: turn_id.to_owned(),
            completed_sequence,
            state: State::PendingSummary,
        });
        changed.save_atomic(path)?;
        *self = changed;
        Ok(true)
    }

    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|item| item.state == State::PendingSummary)
            .count()
    }

    /// List pending requests; the scheduler may inspect this snapshot but
    /// only the persistence worker mutates the queue.
    pub fn pending_summaries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|item| item.state == State::PendingSummary)
    }

    pub fn summary_path(outbox_path: &Path, conversation: &str, turn: &str) -> Result<PathBuf, String> {
        if !identity_valid(conversation) || !identity_valid(turn) {
            return Err("invalid OpenViking checkpoint path identity".to_owned());
        }
        let parent = outbox_path.parent().ok_or("OpenViking outbox parent missing")?;
        Ok(parent
            .join("openviking-checkpoint-summaries")
            .join(conversation)
            .join(format!("{turn}.md")))
    }

    pub fn save_summary_durable(
        &mut self,
        outbox_path: &Path,
        conversation: &str,
        turn: &str,
        completed_sequence: u64,
        summary: &str,
    ) -> Result<bool, String> {
        let summary = summary.trim();
        if summary == "NO_MEMORY" {
            return self.transition_durable(
                outbox_path,
                conversation,
                turn,
                completed_sequence,
                State::PendingSummary,
                State::Skipped,
            );
        }
        if summary.is_empty()
            || summary.len() > 8192
            || summary.chars().any(|ch| ch == '\0' || (ch.is_control() && ch != '\n' && ch != '\t'))
        {
            return Err("checkpoint summary empty, too large or contains control characters".to_owned());
        }
        let exists = self.entries.iter().any(|entry| {
            entry.conversation_id == conversation
                && entry.turn_id == turn
                && entry.completed_sequence == completed_sequence
                && entry.state == State::PendingSummary
        });
        if !exists {
            return Err("checkpoint summary has no matching pending source".to_owned());
        }
        let file = Self::summary_path(outbox_path, conversation, turn)?;
        let body = format!(
            "# Chatarium memory checkpoint\n\nConversation: {conversation}\nTurn: {turn}\nSource journal sequence: {completed_sequence}\n\n{summary}\n"
        );
        let parent = file.parent().ok_or("checkpoint summary directory missing")?;
        fs::create_dir_all(parent).map_err(|e| format!("summary directory: {e}"))?;
        if file.exists() {
            let existing = fs::read_to_string(&file)
                .map_err(|e| format!("cannot inspect existing summary: {e}"))?;
            if existing != body {
                return Err("different summary already stored for this source; refusing overwrite".to_owned());
            }
        } else {
            use std::io::Write;
            let mut dest = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)
                .map_err(|e| format!("cannot create checkpoint summary: {e}"))?;
            dest.write_all(body.as_bytes())
                .and_then(|()| dest.sync_all())
                .map_err(|e| format!("cannot persist checkpoint summary: {e}"))?;
        }
        self.transition_durable(
            outbox_path,
            conversation,
            turn,
            completed_sequence,
            State::PendingSummary,
            State::Ready,
        )
    }

    pub fn transition_durable(
        &mut self,
        path: &Path,
        conversation: &str,
        turn: &str,
        completed_sequence: u64,
        expected: State,
        next: State,
    ) -> Result<bool, String> {
        let mut updated = self.clone();
        let entry = updated.entries.iter_mut().find(|entry| {
            entry.conversation_id == conversation
                && entry.turn_id == turn
                && entry.completed_sequence == completed_sequence
        })
        .ok_or("checkpoint transition has no matching source")?;
        if entry.state == next {
            return Ok(false);
        }
        if entry.state != expected {
            return Err("checkpoint state transition conflicts with persisted state".to_owned());
        }
        let allowed = matches!(
            (expected, next),
            (State::PendingSummary, State::Ready)
                | (State::PendingSummary, State::Skipped)
                | (State::Ready, State::Dispatched)
                | (State::Dispatched, State::Confirmed)
                | (State::Dispatched, State::Uncertain)
                | (State::Uncertain, State::Confirmed)
        );
        if !allowed {
            return Err("checkpoint state transition is not permitted".to_owned());
        }
        entry.state = next;
        updated.save_atomic(path)?;
        *self = updated;
        Ok(true)
    }

    /// A process crash during an outbound write leaves an unknown result.
    /// Recover conservatively; never reissue the same create automatically.
    pub fn recover_interrupted_dispatches_durable(
        &mut self,
        path: &Path,
    ) -> Result<usize, String> {
        let count = self
            .entries
            .iter()
            .filter(|entry| entry.state == State::Dispatched)
            .count();
        if count == 0 {
            return Ok(0);
        }
        let mut updated = self.clone();
        for entry in &mut updated.entries {
            if entry.state == State::Dispatched {
                entry.state = State::Uncertain;
            }
        }
        updated.save_atomic(path)?;
        *self = updated;
        Ok(count)
    }

    pub fn ready_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|entry| entry.state == State::Ready)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-checkpoint-{label}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn restart_recovers_one_exact_pending_source_without_transcript_body() {
        let path = test_path("restart");
        let mut queue = Outbox::default();
        assert!(
            queue
                .enqueue_durable(&path, "conversation-1", "turn-1", 51)
                .unwrap()
        );
        assert!(
            !queue
                .enqueue_durable(&path, "conversation-1", "turn-1", 52)
                .unwrap()
        );
        let reopened = Outbox::load(&path).unwrap();
        assert_eq!(reopened.pending_count(), 1);
        assert_eq!(reopened.entries()[0].completed_sequence, 51);
        let bytes = fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("assistant_response"));
        assert!(!bytes.contains("conversation_text"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failed_persistence_does_not_claim_a_durable_checkpoint() {
        let parent = test_path("invalid-parent");
        fs::write(&parent, "not a directory").unwrap();
        let path = parent.join(FILE_NAME);
        let mut queue = Outbox::default();
        assert!(
            queue
                .enqueue_durable(&path, "conversation-1", "turn-1", 51)
                .is_err()
        );
        assert_eq!(queue.pending_count(), 0);
        let _ = fs::remove_file(parent);
    }

    #[test]
    fn saved_checkpoint_body_is_separate_from_metadata_and_is_create_only() {
        let path = test_path("summary");
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        outbox.enqueue_durable(&path, "chat-abc", "turn-123", 6).unwrap();
        let summary = "Committed a fix for archive pagination, with the relevant source commit linked.";
        assert!(outbox
            .save_summary_durable(&path, "chat-abc", "turn-123", 6, summary)
            .unwrap());
        assert_eq!(outbox.entries()[0].state, State::Ready);
        assert_eq!(outbox.ready_entries().count(), 1);
        let file = Outbox::summary_path(&path, "chat-abc", "turn-123").unwrap();
        assert!(fs::read_to_string(&file).unwrap().contains(summary));
        assert!(!fs::read_to_string(&path).unwrap().contains(summary));
        assert!(outbox
            .save_summary_durable(&path, "chat-abc", "turn-123", 6, "other")
            .is_err());
        assert!(outbox
            .transition_durable(
                &path,
                "chat-abc",
                "turn-123",
                6,
                State::Ready,
                State::Dispatched,
            )
            .unwrap());
        let reloaded = Outbox::load(&path).unwrap();
        assert_eq!(reloaded.entries()[0].state, State::Dispatched);
        let mut recovered = reloaded;
        assert_eq!(recovered.recover_interrupted_dispatches_durable(&path), Ok(1));
        assert_eq!(recovered.entries()[0].state, State::Uncertain);
        assert_eq!(recovered.recover_interrupted_dispatches_durable(&path), Ok(0));
        assert!(outbox
            .transition_durable(
                &path,
                "chat-abc",
                "turn-123",
                6,
                State::Ready,
                State::Confirmed,
            )
            .is_err());
        let _ = fs::remove_file(file);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn no_memory_skips_without_creating_external_write_or_file() {
        let path = test_path("skip");
        let mut outbox = Outbox::default();
        outbox.activate_durable(&path, 0).unwrap();
        outbox.enqueue_durable(&path, "chat-abc", "turn-124", 7).unwrap();
        outbox
            .save_summary_durable(&path, "chat-abc", "turn-124", 7, "NO_MEMORY")
            .unwrap();
        assert_eq!(outbox.entries()[0].state, State::Skipped);
        assert_eq!(outbox.ready_entries().count(), 0);
        assert!(!Outbox::summary_path(&path, "chat-abc", "turn-124").unwrap().exists());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_or_duplicate_disk_entries_fail_closed() {
        let path = test_path("invalid");
        fs::write(&path, r#"{"schema":"bad","version":1,"entries":[]}"#).unwrap();
        assert!(Outbox::load(&path).is_err());
        let mut queue = Outbox::default();
        queue.entries.push(Entry {
            conversation_id: "a".to_owned(),
            turn_id: "t".to_owned(),
            completed_sequence: 1,
            state: State::PendingSummary,
        });
        queue.entries.push(queue.entries[0].clone());
        assert!(queue.save_atomic(&path).is_err());
        assert!(queue.enqueue_durable(&path, "../escape", "t", 2).is_err());
        let _ = fs::remove_file(path);
    }
}
