use chatarium_core::{
    AssistantEvidence, AuthoredUserMessage, EventKind, LocalConversationId, LocalEvidence,
    LocalMessageId, LocalTurnId, RemoteEvidence,
};
use chatarium_store::authored::{commit_user_message, local_turn_scope};
use chatarium_store::projection::SqliteProjection;
use chatarium_store::{AuthoredTurnRow, EventStore, JsonlEventStore};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "chatarium-crash-matrix-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create temp root");
    root
}

fn paths(root: &Path) -> (PathBuf, PathBuf) {
    (root.join("events.jsonl"), root.join("projection.sqlite3"))
}

fn append_turn_event(journal_path: &Path, turn_id: LocalTurnId, kind: EventKind) -> u64 {
    let mut journal = JsonlEventStore::open(journal_path).expect("reopen journal for append");
    journal
        .append_scoped(Some(local_turn_scope(turn_id)), kind, String::new())
        .expect("append turn event")
}

fn recover_turn(
    journal_path: &Path,
    projection_path: &Path,
    message: &AuthoredUserMessage,
) -> (Vec<chatarium_store::EventEnvelope>, AuthoredTurnRow) {
    let journal = JsonlEventStore::open(journal_path).expect("reopen journal");
    let events = journal.events().to_vec();
    drop(journal);

    let mut projection = SqliteProjection::open(projection_path).expect("reopen projection");
    if !projection
        .is_current_with(&events)
        .expect("check projection currency")
    {
        projection.rebuild(&events).expect("rebuild projection");
    }
    assert!(
        projection
            .is_current_with(&events)
            .expect("projection current after rebuild")
    );

    let row = projection
        .authored_turn(message.turn_id)
        .expect("query authored turn")
        .expect("authored turn exists");
    assert_eq!(row.conversation_id, message.conversation_id);
    assert_eq!(row.turn_id, message.turn_id);
    assert_eq!(row.message_id, message.message_id);
    assert_eq!(row.exact_user_text, message.text);
    (events, row)
}

fn assert_evidence(
    row: &AuthoredTurnRow,
    local: LocalEvidence,
    remote: RemoteEvidence,
    assistant: AssistantEvidence,
) {
    assert_eq!(row.evidence.local, local);
    assert_eq!(row.evidence.remote, remote);
    assert_eq!(row.evidence.assistant, assistant);
}

#[test]
fn persistent_turn_lifecycle_survives_restart_matrix() {
    let root = temp_root("lifecycle");
    let (journal_path, projection_path) = paths(&root);
    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        " exact durable text\nwith spacing ",
    );
    let scope = local_turn_scope(message.turn_id);

    // Crash before immutable local commit: durable draft evidence may survive, but there is no
    // authored turn and no remote mutation evidence.
    {
        let mut journal = JsonlEventStore::open(&journal_path).expect("open empty journal");
        assert_eq!(
            journal
                .append_scoped(
                    Some(scope.clone()),
                    EventKind::DraftChanged,
                    message.text.clone(),
                )
                .expect("append draft"),
            1
        );
    }
    {
        let journal = JsonlEventStore::open(&journal_path).expect("restart before commit");
        assert_eq!(journal.events().len(), 1);
        assert_eq!(journal.events()[0].kind, EventKind::DraftChanged);
        assert_eq!(journal.events()[0].payload, message.text);

        let mut projection = SqliteProjection::open(&projection_path).expect("open projection");
        projection
            .rebuild(journal.events())
            .expect("rebuild pre-commit");
        assert!(projection.authored_turns().unwrap().is_empty());
    }

    // Exact authorship crosses the real JSONL durability boundary.
    {
        let mut journal = JsonlEventStore::open(&journal_path).expect("reopen for commit");
        let receipt = commit_user_message(&mut journal, &message).expect("typed durable commit");
        assert_eq!(receipt.sequence, 2);
        assert_eq!(receipt.message, message);
    }
    let (events, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_eq!(events.len(), 2);
    assert_eq!(row.commit_sequence, 2);
    assert_eq!(row.last_sequence, 2);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::NotAttempted,
        AssistantEvidence::None,
    );

    // Dispatch began, but no acceptance or failure has been observed.
    assert_eq!(
        append_turn_event(&journal_path, message.turn_id, EventKind::DispatchAttempted),
        3
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::Dispatching,
        AssistantEvidence::None,
    );

    // Transport interruption preserves uncertainty instead of fabricating failure.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::TransportInterrupted
        ),
        4
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::OutcomeUnknown,
        AssistantEvidence::None,
    );

    // Merely attempting/recording reconciliation does not invent a remote outcome.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::ReconciliationAttempted
        ),
        5
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_eq!(row.evidence.remote, RemoteEvidence::OutcomeUnknown);

    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::ReconciliationObserved
        ),
        6
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_eq!(row.evidence.remote, RemoteEvidence::OutcomeUnknown);

    // Later positive acceptance evidence may resolve prior uncertainty.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::RemoteAcceptanceObserved
        ),
        7
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::AcceptedObserved,
        AssistantEvidence::None,
    );

    // Assistant output remains streaming until interrupted/completed evidence says otherwise.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::AssistantStreamStarted
        ),
        8
    );
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::AssistantDeltaObserved
        ),
        9
    );
    let (_, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::AcceptedObserved,
        AssistantEvidence::Streaming,
    );

    // A later transport interruption cannot erase observed acceptance; it only marks output partial.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::TransportInterrupted
        ),
        10
    );
    let (events_before_completion, row_before_completion) =
        recover_turn(&journal_path, &projection_path, &message);
    assert_eq!(row_before_completion.last_sequence, 10);
    assert_evidence(
        &row_before_completion,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::AcceptedObserved,
        AssistantEvidence::PartialInterrupted,
    );

    // Simulate the critical crash boundary: completion is durable in JSONL, but the process dies
    // before updating SQLite. Reopening must detect stale SQLite and rebuild from the journal.
    assert_eq!(
        append_turn_event(
            &journal_path,
            message.turn_id,
            EventKind::AssistantCompletionObserved
        ),
        11
    );
    let journal_after_completion =
        JsonlEventStore::open(&journal_path).expect("restart after completion append");
    assert_eq!(journal_after_completion.events().len(), 11);
    assert_eq!(
        journal_after_completion.events().last().unwrap().kind,
        EventKind::AssistantCompletionObserved
    );

    let mut stale_projection =
        SqliteProjection::open(&projection_path).expect("open stale projection");
    assert!(
        stale_projection
            .is_current_with(&events_before_completion)
            .expect("old projection matched old journal")
    );
    assert!(
        !stale_projection
            .is_current_with(journal_after_completion.events())
            .expect("new durable completion makes projection stale")
    );
    let stale_row = stale_projection
        .authored_turn(message.turn_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        stale_row.evidence.assistant,
        AssistantEvidence::PartialInterrupted
    );

    stale_projection
        .rebuild(journal_after_completion.events())
        .expect("rebuild after crash boundary");
    assert!(
        stale_projection
            .is_current_with(journal_after_completion.events())
            .unwrap()
    );
    let completed = stale_projection
        .authored_turn(message.turn_id)
        .unwrap()
        .unwrap();
    assert_eq!(completed.last_sequence, 11);
    assert_evidence(
        &completed,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::AcceptedObserved,
        AssistantEvidence::CompletedObserved,
    );
    assert_eq!(completed.exact_user_text, message.text);

    drop(stale_projection);
    drop(journal_after_completion);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn typed_commit_survives_unterminated_next_event_tail() {
    let root = temp_root("torn-tail");
    let (journal_path, projection_path) = paths(&root);
    let message = AuthoredUserMessage::new(
        LocalConversationId::new(),
        LocalTurnId::new(),
        LocalMessageId::new(),
        "must survive exactly",
    );

    {
        let mut journal = JsonlEventStore::open(&journal_path).expect("open journal");
        let receipt = commit_user_message(&mut journal, &message).expect("commit exact text");
        assert_eq!(receipt.sequence, 1);
    }

    // Bytes for a would-be next record reach disk without a terminating newline/complete JSON.
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .expect("open raw journal append");
        file.write_all(
            br#"{"v":2,"sequence":2,"at_unix_ms":123,"scope":"local-turn:partial","kind":"dispatch_attempted""#,
        )
        .expect("write partial tail");
        file.sync_data().expect("sync partial tail");
    }

    let recovered = JsonlEventStore::open(&journal_path).expect("recover torn tail");
    assert_eq!(recovered.events().len(), 1);
    assert_eq!(recovered.events()[0].kind, EventKind::UserMessageCommitted);
    assert_eq!(recovered.events()[0].sequence, 1);

    let mut projection = SqliteProjection::open(&projection_path).expect("open projection");
    projection
        .rebuild(recovered.events())
        .expect("rebuild recovered");
    let row = projection
        .authored_turn(message.turn_id)
        .unwrap()
        .expect("typed authored turn survives");
    assert_eq!(row.exact_user_text, message.text);
    assert_evidence(
        &row,
        LocalEvidence::MessageCommitted,
        RemoteEvidence::NotAttempted,
        AssistantEvidence::None,
    );

    drop(projection);
    drop(recovered);

    // Sequence continuity resumes at 2 after the torn tail was removed.
    assert_eq!(
        append_turn_event(&journal_path, message.turn_id, EventKind::DispatchAttempted),
        2
    );
    let (events, row) = recover_turn(&journal_path, &projection_path, &message);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].sequence, 2);
    assert_eq!(row.evidence.remote, RemoteEvidence::Dispatching);

    let _ = fs::remove_dir_all(root);
}
