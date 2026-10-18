//! Durable, metadata-only requests for automatic OpenViking checkpoints.
//! No transcript or credentials are copied into this file. Generating a
//! checkpoint and sending it are deliberately separate, audited operations.
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const FILE_NAME: &str = "openviking-checkpoint-outbox.json";
const SCHEMA: &str = "chatarium-openviking-checkpoint-outbox";
const MAX_ITEMS: usize = 4096;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub conversation_id: String,
    pub turn_id: String,
    pub completed_sequence: u64,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    PendingSummary,
    Ready,
    Dispatched,
    Confirmed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outbox {
    schema: String,
    version: u32,
    entries: Vec<Entry>,
}

impl Default for Outbox {
    fn default() -> Self {
        Self {
            schema: SCHEMA.to_owned(),
            version: 1,
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
        let outbox: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("checkpoint outbox JSON: {e}"))?;
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
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("checkpoint outbox exceeds file limit".to_owned());
        }
        let parent = path.parent().ok_or("checkpoint outbox parent missing")?;
        fs::create_dir_all(parent)
            .map_err(|e| format!("checkpoint outbox directory: {e}"))?;
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

    /// Persist a memory-processing intent, not a successful memory write.
    /// A repeated delivery or process restart cannot duplicate the request.
    pub fn enqueue_durable(
        &mut self,
        path: &Path,
        conversation_id: &str,
        turn_id: &str,
        completed_sequence: u64,
    ) -> Result<bool, String> {
        if !identity_valid(conversation_id)
            || !identity_valid(turn_id)
            || completed_sequence == 0
        {
            return Err("invalid checkpoint source identity".to_owned());
        }
        if self.entries.iter().any(|entry| {
            entry.conversation_id == conversation_id && entry.turn_id == turn_id
        }) {
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
        assert!(queue.enqueue_durable(&path, "conversation-1", "turn-1", 51).unwrap());
        assert!(!queue.enqueue_durable(&path, "conversation-1", "turn-1", 52).unwrap());
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
        assert!(queue.enqueue_durable(&path, "conversation-1", "turn-1", 51).is_err());
        assert_eq!(queue.pending_count(), 0);
        let _ = fs::remove_file(parent);
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
