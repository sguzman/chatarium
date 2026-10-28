use super::*;

#[test]
fn historical_lookup_retrieves_relevant_old_checkpoint_but_preserves_latest() {
    use crate::openviking_checkpoint_outbox::{Outbox, State};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-scoped-history-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&path, 0).unwrap();
    for (owner, turn, sequence, content, confirmed) in [
        (
            "owner",
            "source-old",
            1,
            "DECISION: rust ownership and borrow checking",
            true,
        ),
        (
            "foreign",
            "secret",
            2,
            "rust ownership secret foreign memory",
            true,
        ),
        (
            "owner",
            "unconfirmed",
            3,
            "rust ownership prohibited pending",
            false,
        ),
        ("owner", "middle", 4, "weather tonight", true),
        ("owner", "middle-2", 5, "calendar meetings", true),
        ("owner", "newest", 6, "active future milestones", true),
    ] {
        outbox
            .enqueue_durable(&path, owner, turn, sequence)
            .unwrap();
        outbox
            .save_summary_durable(&path, owner, turn, sequence, content)
            .unwrap();
        if confirmed {
            outbox
                .transition_durable(
                    &path,
                    owner,
                    turn,
                    sequence,
                    State::Ready,
                    State::Dispatched,
                )
                .unwrap();
            outbox
                .transition_durable(
                    &path,
                    owner,
                    turn,
                    sequence,
                    State::Dispatched,
                    State::Confirmed,
                )
                .unwrap();
        }
    }
    let relevant = select_confirmed_history(
        &outbox,
        &path,
        "owner",
        "Can we resume RUST ownership decisions?",
    )
    .unwrap();
    let names = relevant
        .iter()
        .map(|(entry, _)| entry.turn_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names[0], "newest");
    assert!(names.contains(&"source-old"));
    assert!(!names.contains(&"secret"));
    assert!(!names.contains(&"unconfirmed"));
    assert_eq!(relevant.len(), 3);
    assert_eq!(
        select_confirmed_history(&outbox, &path, "owner", "")
            .unwrap()
            .iter()
            .map(|(entry, _)| entry.turn_id.as_str())
            .collect::<Vec<_>>(),
        ["newest", "middle-2", "middle"]
    );
    // Wrong owner or path scope is rejected before local file scanning.
    assert!(select_confirmed_history(&outbox, &path, "../escape", "rust").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_candidate_scan_is_bounded_and_does_not_rank_metadata() {
    use crate::openviking_checkpoint_outbox::{Outbox, State};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-bounded-history-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&path, 0).unwrap();
    for sequence in 1..=(MAX_HISTORY_SCAN_ITEMS as u64 + 3) {
        let turn = format!("checkpoint-{sequence}");
        let text = if sequence == 1 {
            "ORBITAL ANCHOR"
        } else {
            "unrelated summary"
        };
        outbox
            .enqueue_durable(&path, "owner", &turn, sequence)
            .unwrap();
        outbox
            .save_summary_durable(&path, "owner", &turn, sequence, text)
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                &turn,
                sequence,
                State::Ready,
                State::Dispatched,
            )
            .unwrap();
        outbox
            .transition_durable(
                &path,
                "owner",
                &turn,
                sequence,
                State::Dispatched,
                State::Confirmed,
            )
            .unwrap();
    }
    let selected = select_confirmed_history(&outbox, &path, "owner", "ORBITAL").unwrap();
    assert_eq!(selected.len(), 3);
    assert!(
        selected
            .iter()
            .all(|(entry, _)| entry.completed_sequence > 3)
    );
    assert_eq!(
        checkpoint_relevance(
            "# Chatarium memory checkpoint\n\nConversation: orbital\nTurn: orbital\nSource journal sequence: 8\n\nNo meaningful match.",
            &relevance_terms("orbital"),
        ),
        0,
    );
    std::fs::remove_dir_all(root).unwrap();
}

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
                .transition_durable(&path, owner, turn, seq, State::Dispatched, State::Confirmed)
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
fn own_confirmed_memory_rejects_corrupt_owner_header_before_network_read() {
    use crate::openviking_checkpoint_outbox::{Outbox, State};
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-own-checkpoint-identity-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(crate::openviking_checkpoint_outbox::FILE_NAME);
    let mut outbox = Outbox::default();
    outbox.activate_durable(&path, 0).unwrap();
    outbox
        .enqueue_durable(&path, "owner", "turn-1", 17)
        .unwrap();
    outbox
        .save_summary_durable(&path, "owner", "turn-1", 17, "correct source")
        .unwrap();
    outbox
        .transition_durable(
            &path,
            "owner",
            "turn-1",
            17,
            State::Ready,
            State::Dispatched,
        )
        .unwrap();
    outbox
        .transition_durable(
            &path,
            "owner",
            "turn-1",
            17,
            State::Dispatched,
            State::Confirmed,
        )
        .unwrap();

    let summary = Outbox::summary_path(&path, "owner", "turn-1").unwrap();
    let contents = std::fs::read_to_string(&summary).unwrap();
    std::fs::write(
        &summary,
        contents.replace("Conversation: owner", "Conversation: other"),
    )
    .unwrap();
    let error = retrieve_confirmed_for_conversation(&path, "owner").unwrap_err();
    assert!(error.contains("identity mismatch"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let external = root.join("matching-confirmed.md");
        std::fs::write(&external, contents).unwrap();
        std::fs::remove_file(&summary).unwrap();
        symlink(&external, &summary).unwrap();
        let error = retrieve_confirmed_for_conversation(&path, "owner").unwrap_err();
        assert!(error.contains("checkpoint evidence"));
    }
    std::fs::remove_dir_all(root).unwrap();
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let external = dir.join("matching-outside.md");
        std::fs::write(&external, &content).unwrap();
        std::fs::remove_file(&source).unwrap();
        symlink(&external, &source).unwrap();
        assert!(
            verify_uncertain_with_read(&path, "owner", "turn-1", 12, |_| {
                panic!("remote read must not run for redirected source")
            })
            .is_err()
        );
    }
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
fn asynchronous_create_is_written_once_and_verified_with_read_only_retries() {
    let mut calls = Vec::new();
    let mut read_count = 0;
    let mut pauses = Vec::new();
    deliver_create_once_with(
        "conversation-1",
        "turn-1",
        "expected durable memory",
        |owner, key, provider, operation, args| {
            assert_eq!(owner, "conversation-1");
            assert_eq!(provider, local_http_mcp::OPENVIKING);
            assert_eq!(
                args.get(if operation == "write" { "uri" } else { "uris" })
                    .and_then(Value::as_str),
                checkpoint_uri("conversation-1", "turn-1").ok().as_deref(),
            );
            calls.push((operation.to_owned(), key.to_owned()));
            match operation {
                "write" => {
                    assert_eq!(args["mode"], "create");
                    assert_eq!(args["wait"], false);
                    Ok(json!({"content": [{"type": "text", "text": "accepted"}]}))
                }
                "read" => {
                    read_count += 1;
                    match read_count {
                        1 => Err("not materialized yet".to_owned()),
                        2 => Ok(json!({"content": [{"type": "text", "text": "stale"}]})),
                        3 => Ok(json!({"content": [{"type": "text", "text": "expected durable memory"}]})),
                        _ => panic!("verification must stop after an exact read"),
                    }
                }
                _ => panic!("unexpected operation"),
            }
        },
        |delay| pauses.push(delay.as_millis()),
    )
    .unwrap();
    assert_eq!(
        calls.iter().filter(|(method, _)| method == "write").count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|(method, _)| method == "read").count(),
        3
    );
    assert_eq!(pauses, [250, 500]);
    assert_ne!(
        calls[2].1, calls[3].1,
        "verification IDs must stay distinct"
    );
}

#[test]
fn mismatched_readback_remains_uncertain_without_any_second_create() {
    let mut writes = 0;
    let mut reads = 0;
    let error = deliver_create_once_with(
        "conversation-1",
        "turn-2",
        "verified bytes only",
        |_, _, _, operation, _| match operation {
            "write" => {
                writes += 1;
                Ok(json!({"content": []}))
            }
            "read" => {
                reads += 1;
                Ok(json!({"content": [{"type": "text", "text": "REMOTE DRIFT"}]}))
            }
            _ => panic!("unexpected operation"),
        },
        |_| {},
    )
    .unwrap_err();
    assert!(error.contains("unconfirmed"));
    assert_eq!(writes, 1);
    assert_eq!(reads, VERIFY_READ_ATTEMPTS);
}

#[test]
fn failed_or_rejected_create_does_not_attempt_read_or_retry() {
    for fail_as_tool_error in [false, true] {
        let mut calls = Vec::new();
        let result = deliver_create_once_with(
            "conversation-1",
            "turn-3",
            "private checkpoint",
            |_, _, _, operation, _| {
                calls.push(operation.to_owned());
                if fail_as_tool_error {
                    Ok(json!({"isError": true, "content": []}))
                } else {
                    Err("create uncertain".to_owned())
                }
            },
            |_| panic!("no delay after failed create"),
        );
        assert!(result.is_err());
        assert_eq!(calls, ["write"]);
    }
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
