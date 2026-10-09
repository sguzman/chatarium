//! Synthetic stress workloads for native-only, provenance-preserving search.
//! This module exercises correctness at archive scale, not wall-clock budgets.

use super::super::local_conversations::LocalConversationCatalog;
use super::*;
use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
use chatarium_store::{EventStore, MemoryEventStore};

fn author(store: &mut impl EventStore, owner: LocalConversationId, text: &str) {
    let authored = AuthoredUserMessage::new(owner, LocalTurnId::new(), LocalMessageId::new(), text);
    chatarium_store::authored::commit_user_message(store, &authored).unwrap();
}

#[test]
fn cold_projection_preserves_out_of_order_assistant_ownership_and_chronology() {
    let owner = LocalConversationId::new();
    let unrelated = LocalConversationId::new();
    let turn = LocalTurnId::new();
    let mut store = MemoryEventStore::default();
    let scope = chatarium_store::authored::local_turn_scope(turn);
    let observed = serde_json::json!({
        "schema": "chatarium-responses-turn-observation",
        "version": 1,
        "text": "early assistant observation",
        "details": {
            "local_turn_id": turn.to_string(),
            "request_id": turn.to_string(),
        }
    });
    store
        .append_scoped(
            Some(scope.clone()),
            EventKind::AssistantSnapshotObserved,
            observed.to_string(),
        )
        .unwrap();
    chatarium_store::authored::commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(owner, turn, LocalMessageId::new(), "authored later"),
    )
    .unwrap();
    let completed = serde_json::json!({
        "schema": "chatarium-responses-turn-observation",
        "version": 1,
        "text": "final assistant reply",
        "details": {
            "local_turn_id": turn.to_string(),
            "request_id": turn.to_string(),
        }
    });
    store
        .append_scoped(
            Some(scope),
            EventKind::AssistantCompletionObserved,
            completed.to_string(),
        )
        .unwrap();
    author(
        &mut store,
        unrelated,
        "PRIVATE FOREIGN TRANSCRIPT".to_owned(),
    );
    store
        .append_scoped(
            None,
            EventKind::ToolCallOutcomeObserved,
            "PRIVATE TOOL OUTPUT".to_owned(),
        )
        .unwrap();

    let indexed = NativeConversationSearchIndex::build(store.events());
    let expected = super::super::projected_local_display_messages(store.events(), owner);
    let visible = indexed.messages(owner);
    assert_eq!(visible.len(), 2);
    assert_eq!(
        visible.iter().map(|message| message.text.as_str()).collect::<Vec<_>>(),
        vec!["final assistant reply", "authored later"]
    );
    assert_eq!(
        visible.iter().map(|message| message.sequence).collect::<Vec<_>>(),
        expected.iter().map(|message| message.sequence).collect::<Vec<_>>()
    );
    assert!(visible.iter().all(|message| {
        !message.text.contains("PRIVATE")
    }));
    assert_eq!(indexed.messages(unrelated).len(), 1);
}

#[test]
fn wide_archive_search_keeps_owner_boundaries_and_incremental_reuse() {
    const SHARDS: usize = 96;
    const MESSAGES: usize = 128;
    let ctx = egui::Context::default();
    let mut catalog = LocalConversationCatalog::default();
    let mut store = MemoryEventStore::default();
    let mut owners = Vec::with_capacity(SHARDS);

    for shard in 0..SHARDS {
        let owner = LocalConversationId::new();
        catalog.create(owner, (shard + 1) as u64);
        catalog
            .rename(owner, Some(format!("Shard {shard:03}")), (shard + 1) as u64)
            .unwrap();
        if shard % 12 == 0 {
            catalog
                .set_archived(owner, true, (shard + 1) as u64)
                .unwrap();
        }
        for position in 0..MESSAGES {
            let text = if shard % 2 == 0 && matches!(position, 0 | 64 | 127) {
                format!("search-pin shard {shard} message {position}")
            } else {
                format!("unrelated shard {shard} message {position}")
            };
            author(&mut store, owner, &text);
        }
        owners.push(owner);
    }

    // Non-chat events must remain excluded even when they share a journal.
    store
        .append_scoped(
            None,
            EventKind::ToolCallOutcomeObserved,
            "NEVER_INDEX_TOOL_BODY".to_owned(),
        )
        .unwrap();
    let original = cached_index(&ctx, store.events());
    let entries = catalog.entries();
    let visible = cached_rows(&ctx, &original, &entries, false, "search-pin");
    assert_eq!(visible.len(), 40);
    for row in visible.iter() {
        assert_eq!(row.kind, NativeSearchMatch::Message);
        assert_eq!(row.matching_message_indices, vec![0, 64, 127]);
        let owner = entries[row.catalog_index].id;
        for position in &row.matching_message_indices {
            let staged = stageable_hit(&original, owner, *position).unwrap();
            assert_eq!(staged.source_conversation_id, owner);
            assert!(staged.text.contains("search-pin"));
            assert_eq!(
                staged.source_event_sequence,
                original.messages(owner)[*position].sequence,
            );
        }
    }
    assert!(Arc::ptr_eq(
        &visible,
        &cached_rows(&ctx, &original, &entries, false, "search-pin"),
    ));
    assert_eq!(
        cached_rows(&ctx, &original, &entries, true, "search-pin").len(),
        48,
    );
    let excluded = cached_rows(&ctx, &original, &entries, false, "NEVER_INDEX_TOOL_BODY");
    assert!(excluded.is_empty());

    // Draft events advance the journal cursor without projecting messages.
    store
        .append_scoped(None, EventKind::DraftChanged, "draft only".to_owned())
        .unwrap();
    let unchanged = cached_index(&ctx, store.events());
    assert!(Arc::ptr_eq(&original, &unchanged));
    assert_eq!(
        cached_rows(&ctx, &unchanged, &entries, false, "search-pin").len(),
        visible.len(),
    );

    // The next user message only changes the affected conversation's Arc.
    let target = owners[4];
    let untouched = Arc::clone(original.messages.get(&owners[5]).unwrap());
    author(&mut store, target, "search-pin appended independently");
    let extended = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&original, &extended));
    assert!(Arc::ptr_eq(
        &untouched,
        extended.messages.get(&owners[5]).unwrap(),
    ));
    let rows = cached_rows(&ctx, &extended, &entries, false, "search-pin");
    let row = rows
        .iter()
        .find(|row| entries[row.catalog_index].id == target)
        .unwrap();
    assert_eq!(row.matching_message_indices, vec![0, 64, 127, 128]);
    assert_eq!(
        reader_hit_ordinal(extended.messages(target), "search-pin", 128),
        Some(3),
    );
    let staged = stageable_hit(&extended, target, 128).unwrap();
    let (copied, sequence) = copyable_hit_markdown(&extended, target, &row.title, 128).unwrap();
    assert_eq!(staged.source_event_sequence, sequence);
    assert_eq!(staged.text, "search-pin appended independently");
    assert!(copied.contains(&staged.text));
    assert!(!copied.contains("unrelated shard 5"));
}

#[test]
fn deep_archive_search_tracks_individual_hits_without_cloning_bodies() {
    const COUNT: usize = 4_096;
    const STRIDE: usize = 13;
    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();

    for position in 0..COUNT {
        let text = if position % STRIDE == 0 {
            format!("dense-needle message {position} text {}", "x".repeat(220))
        } else {
            format!("message {position} background {}", "z".repeat(220))
        };
        author(&mut store, owner, &text);
    }

    let mut catalog = LocalConversationCatalog::default();
    catalog.create(owner, 1);
    catalog
        .rename(owner, Some("Deep archive".to_owned()), 2)
        .unwrap();
    let entries = catalog.entries();
    let index = cached_index(&ctx, store.events());
    let rows = cached_rows(&ctx, &index, &entries, false, "dense-needle");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let expected = (0..COUNT).step_by(STRIDE).collect::<Vec<_>>();
    assert_eq!(row.matching_message_indices, expected);
    assert!(row.preview.as_deref().unwrap().chars().count() < 180);
    assert_eq!(row.message_index, Some(0));
    assert_eq!(selected_message_index(row, 0), Some(0));
    assert_eq!(
        selected_message_index(row, expected.len() - 1),
        expected.last().copied(),
    );
    assert_eq!(
        cycle_matching_message(row, expected.len() - 1, 1),
        Some((0, 0)),
    );
    let last = *expected.last().unwrap();
    assert_eq!(
        reader_hit_ordinal(index.messages(owner), "dense-needle", last),
        Some(expected.len() - 1),
    );
    let staged = stageable_hit(&index, owner, last).unwrap();
    assert!(
        staged
            .text
            .starts_with(&format!("dense-needle message {last}"))
    );
    assert!(Arc::ptr_eq(
        &rows,
        &cached_rows(&ctx, &index, &entries, false, "dense-needle"),
    ));
}
