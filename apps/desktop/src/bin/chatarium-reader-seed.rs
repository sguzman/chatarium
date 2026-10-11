//! Seed a disposable native-reader archive through the production durability API.
//! This executable never reads a normal Chatarium workspace, never overwrites
//! an existing journal, and never performs any remote or account operation.

use chatarium_core::{AuthoredUserMessage, LocalConversationId, LocalMessageId, LocalTurnId};
use chatarium_store::authored::commit_user_message;
use chatarium_store::{EventStore, JsonlEventStore};
use std::path::PathBuf;

const MESSAGES: usize = 257;
const FIRST_MARKER: &str = "CHATARIUM_QA_LONG_ARCHIVE_FIRST_161803";
const LAST_MARKER: &str = "CHATARIUM_QA_LONG_ARCHIVE_LAST_271828";

fn message_text(index: usize) -> String {
    if index == 0 {
        return format!("{FIRST_MARKER}\n\nFirst synthetic durable message.");
    }
    if index == MESSAGES - 1 {
        return format!("{LAST_MARKER}\n\nFinal synthetic durable message.");
    }
    match index % 7 {
        0 => format!("# Synthetic heading {index}\n\n- item A\n- item B\n\nA paragraph."),
        1 => format!("Long wrapped message #{index}: {}", "repeated text ".repeat(32)),
        2 => format!("Line 1 of {index}\nLine 2\nLine 3\nLine 4"),
        3 => format!("Español 日本語 العربية עברית — synthetic message {index}"),
        4 => format!("Code sample {index}\nfn main() {{ println!(\"fixture\"); }}"),
        5 => format!("A short native user message number {index}."),
        _ => format!("A longer paragraph in the locally generated archive {index}."),
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let supplied = args
        .next()
        .ok_or_else(|| "usage: chatarium-reader-seed .qa-native-long-reader/data/journal.jsonl".to_owned())?;
    if args.next().is_some() {
        return Err("expected exactly one disposable journal path".to_owned());
    }

    // An exact relative path prevents an accidental command against a user's
    // real archive, even if this dedicated QA executable is invoked manually.
    let journal = PathBuf::from(".qa-native-long-reader/data/journal.jsonl");
    if PathBuf::from(&supplied) != journal {
        return Err("refusing any path outside the dedicated .qa-native-long-reader fixture".to_owned());
    }
    if journal.exists() {
        return Err("refusing to overwrite an existing disposable QA journal".to_owned());
    }
    let parent = journal.parent().expect("constant journal has a parent");
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let conversation = LocalConversationId::new();
    {
        let mut store = JsonlEventStore::open(&journal).map_err(|error| error.to_string())?;
        for index in 0..MESSAGES {
            let message = AuthoredUserMessage::new(
                conversation,
                LocalTurnId::new(),
                LocalMessageId::new(),
                message_text(index),
            );
            commit_user_message(&mut store, &message)
                .map_err(|error| format!("commit synthetic row {index}: {error}"))?;
        }
        if store.events().len() != MESSAGES {
            return Err("fixture event count mismatch".to_owned());
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "schema": "chatarium-long-native-reader-fixture",
            "version": 1,
            "events": MESSAGES,
            "messages": MESSAGES,
            "conversation_id": conversation.to_string(),
            "first_marker": FIRST_MARKER,
            "last_marker": LAST_MARKER,
            "disposable": true
        })
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("chatarium-reader-seed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markers_and_multilingual_rows_are_stable() {
        assert!(message_text(0).contains(FIRST_MARKER));
        assert!(message_text(MESSAGES - 1).contains(LAST_MARKER));
        assert!(message_text(3).contains("日本語"));
        assert!(message_text(3).contains("العربية"));
    }
}
