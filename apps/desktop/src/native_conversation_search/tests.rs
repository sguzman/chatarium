use super::*;

#[test]
fn stale_native_copy_selection_never_falls_back_to_another_conversation() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let matches = vec![(first, Some(0)), (second, Some(3))];

    assert_eq!(activate_copy_selection(&matches, None), Some((first, 0)));
    assert_eq!(
        activate_copy_selection(&matches, Some(1)),
        Some((second, 3))
    );
    // The selected row disappeared after a query/filter update. Copying
    // the first result instead would attribute it to the wrong chat.
    assert_eq!(activate_copy_selection(&matches, Some(2)), None);
    assert_eq!(activate_copy_selection(&matches, Some(usize::MAX)), None);

    let title_only = vec![(first, None), (second, Some(3))];
    assert_eq!(activate_copy_selection(&title_only, None), None);
    assert_eq!(activate_copy_selection(&title_only, Some(0)), None);
    assert_eq!(
        activate_copy_selection(&title_only, Some(1)),
        Some((second, 3))
    );
    assert_eq!(activate_copy_selection(&[], None), None);
}

#[test]
fn identity_selection_survives_reorder_and_rejects_missing_rows() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let third = LocalConversationId::new();
    let absent = LocalConversationId::new();
    let before = [first, second, third];
    let after = [third, first, second];

    assert_eq!(
        activate_identity_selection(&after, Some(second)),
        Some(second)
    );
    assert_eq!(
        move_identity_selection(&after, Some(second), -1),
        Some(first)
    );
    assert_eq!(
        move_identity_selection(&before, Some(second), 1),
        Some(third)
    );
    assert_eq!(activate_identity_selection(&after, None), Some(third));
    assert_eq!(
        activate_identity_selection(&[first, third], Some(second)),
        None
    );
    // A removed owner must never cause Enter/copy/staging to select a
    // different chat. Only a subsequent arrow explicitly reselects.
    assert_eq!(activate_identity_selection(&after, Some(absent)), None);
    assert_eq!(
        move_identity_selection(&after, Some(absent), 1),
        Some(third)
    );
    assert_eq!(
        move_identity_selection(&after, Some(absent), -1),
        Some(second)
    );
    assert_eq!(move_identity_selection(&[], Some(absent), 1), None);
    assert_eq!(selected_identity_row(&after, None), Some(0));
    assert_eq!(selected_identity_row(&after, Some(first)), Some(1));
    assert_eq!(selected_identity_row(&after, Some(absent)), None);
    assert_eq!(selected_identity_row(&[], None), None);
    let candidates = [(first, Some(0)), (third, Some(5))];
    let title_only = [(third, None), (first, Some(5))];
    assert_eq!(
        activate_identity_copy_selection(&candidates, Some(second)),
        None
    );
    assert_eq!(
        activate_identity_copy_selection(&title_only, Some(third)),
        None
    );
    assert_eq!(
        activate_identity_copy_selection(&title_only, Some(first)),
        Some((first, 5))
    );
}

#[test]
fn native_selected_hit_follows_event_sequence_when_positions_reorder() {
    let message = |sequence, text: &str| DisplayMessage {
        role: super::super::DisplayRole::User,
        text: text.to_owned(),
        sequence,
        timestamp: None,
        provenance_label: None,
    };
    let original = [message(17, "alpha match"), message(29, "beta match")];
    let row = collect_native_search_row("match", 0, "test".to_owned(), &original).unwrap();
    let selected_sequence = selected_hit_sequence(&row, &original, 1);
    assert_eq!(selected_sequence, Some(29));
    let reordered = [original[1].clone(), original[0].clone()];
    let later = collect_native_search_row("match", 0, "test".to_owned(), &reordered).unwrap();
    let ordinal = selected_hit_ordinal(&later, &reordered, selected_sequence);
    assert_eq!(ordinal, 0);
    assert_eq!(selected_hit_sequence(&later, &reordered, ordinal), Some(29));
    assert_eq!(selected_hit_ordinal(&later, &reordered, Some(999)), 0);
    assert_eq!(confirmed_hit_index(&later, &reordered, Some(29)), Some(0));
    assert_eq!(confirmed_hit_index(&later, &reordered, Some(17)), Some(1));
    assert_eq!(confirmed_hit_index(&later, &reordered, None), Some(0));
    assert_eq!(confirmed_hit_index(&later, &reordered, Some(999)), None);

    // The old selected event disappeared but another matching text remains.
    // A new row position must not become the target of copy or memory stage.
    let replaced = [message(31, "a new match")];
    let new_row = collect_native_search_row("match", 0, "test".to_owned(), &replaced).unwrap();
    assert_eq!(confirmed_hit_index(&new_row, &replaced, Some(29)), None);
    assert_eq!(confirmed_hit_index(&new_row, &replaced, None), Some(0));
}

#[test]
fn sidebar_summary_is_cached_with_local_search_rows() {
    let display = |role, text: &str, sequence| DisplayMessage {
        role,
        text: text.to_owned(),
        sequence,
        timestamp: None,
        provenance_label: None,
    };
    let messages = [
        display(DisplayRole::User, "Hola México", 1),
        display(DisplayRole::Assistant, "answer now", 2),
        display(DisplayRole::Mcp, "tool is not authored chat", 3),
        display(DisplayRole::Activity, "Agent activity complete", 4),
    ];
    let listed = collect_native_search_row("", 0, "Chat".to_owned(), &messages).unwrap();
    let matched = collect_native_search_row("answer", 0, "Chat".to_owned(), &messages).unwrap();
    assert_eq!(listed.kind, NativeSearchMatch::Title);
    assert_eq!(matched.message_index, Some(1));
    assert_eq!(listed.summary, matched.summary);
    assert_eq!(listed.summary.message_count, 4);
    assert_eq!(listed.summary.authored_words, 4);
    assert_eq!(
        listed.summary.authored_bytes,
        "Hola México".len() + "answer now".len()
    );
    assert_eq!(
        listed.summary.latest_excerpt.as_deref(),
        Some("Agent activity complete")
    );
    let updated = [
        display(DisplayRole::User, "first question", 1),
        display(DisplayRole::Assistant, "different response", 2),
    ];
    let changed = collect_native_search_row("", 0, "Chat".to_owned(), &updated).unwrap();
    assert_eq!(changed.summary.message_count, 2);
    assert_eq!(changed.summary.authored_words, 4);
    assert_eq!(
        changed.summary.latest_excerpt.as_deref(),
        Some("different response")
    );
}

fn author(
    store: &mut impl chatarium_store::EventStore,
    owner: LocalConversationId,
    text: &str,
) -> chatarium_core::LocalTurnId {
    use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
    let turn = LocalTurnId::new();
    chatarium_store::authored::commit_user_message(
        store,
        &AuthoredUserMessage::new(owner, turn, LocalMessageId::new(), text),
    )
    .unwrap();
    turn
}

#[test]
fn native_activity_survives_search_and_never_becomes_model_context() {
    use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let other = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let turn = author(&mut store, owner, "What happened to the network?");
    author(&mut store, other, "Separate confidential conversation");
    let activity_frame = |sequence, stage, extra: serde_json::Value| {
        let mut frame = serde_json::json!({
            "type": "agent_activity",
            "source": "siwc_bridge_observation",
            "request_id": "retry-test",
            "sequence": sequence,
            "stage": stage,
            "observed_at_utc": "2026-10-13T19:00:00.000Z",
            "elapsed_ms": sequence as f64 * 10.0,
        });
        for (key, value) in extra.as_object().unwrap() {
            frame[key] = value.clone();
        }
        frame.to_string()
    };
    for (sequence, stage, extra) in [
        (1, "started", serde_json::json!({})),
        (2, "wire_attempted", serde_json::json!({})),
        (
            3,
            "failed",
            serde_json::json!({
                "delivery_evidence": "wire_unconfirmed",
                "cancelled": false
            }),
        ),
    ] {
        append_siwc_agent_activity_checked(
            &mut store,
            owner,
            turn,
            &activity_frame(sequence, stage, extra),
        )
        .unwrap();
    }
    let index = NativeConversationSearchIndex::build(store.events());
    let owner_messages = index.messages(owner);
    assert_eq!(owner_messages.len(), 2);
    assert_eq!(owner_messages[1].role, super::super::DisplayRole::Activity);
    assert!(owner_messages[1].text.contains("wire_unconfirmed"));
    assert_eq!(
        find_match(
            "wire_unconfirmed",
            "",
            owner_messages.iter().map(|message| message.text.as_str())
        ),
        Some(NativeSearchMatch::Message)
    );
    assert_eq!(
        find_match(
            "wire_unconfirmed",
            "",
            index
                .messages(other)
                .iter()
                .map(|message| message.text.as_str())
        ),
        None
    );
    let context = super::super::context_transcript(owner_messages);
    assert_eq!(
        context.len(),
        1,
        "audit-only events are never model context"
    );
    assert!(!context[0].text.contains("wire_unconfirmed"));
    assert!(
        store
            .events()
            .iter()
            .any(|event| { event.kind == EventKind::SiwcAgentActivityObserved })
    );
}

#[test]
fn incremental_siwc_activity_refresh_preserves_other_chats_and_later_authored_messages() {
    use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
    use chatarium_store::{EventStore, MemoryEventStore};
    let first = LocalConversationId::new();
    let other = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let turn = author(&mut store, first, "First user");
    author(&mut store, other, "Different conversation");
    let mut index = NativeConversationSearchIndex::build(store.events());
    let original_other = index.message_projection(other).unwrap();

    let make_frame = |number: u64, stage: &str| {
        serde_json::json!({
            "type":"agent_activity",
            "source":"siwc_bridge_observation",
            "request_id":"incremental-test",
            "sequence":number,
            "stage":stage,
            "observed_at_utc":"2026-10-13T19:00:00.000Z",
            "elapsed_ms":number as f64 * 10.0
        })
        .to_string()
    };
    for (number, stage) in [(1, "started"), (2, "wire_attempted")] {
        let old_count = store.events().len();
        append_siwc_agent_activity_checked(&mut store, first, turn, &make_frame(number, stage))
            .unwrap();
        index = index
            .extend_native_appends(store.events(), old_count)
            .unwrap();
        let first_messages = index.messages(first);
        assert_eq!(first_messages.len(), 2);
        assert_eq!(first_messages[1].role, super::super::DisplayRole::Activity);
        assert!(first_messages[1].text.contains(stage));
        assert!(
            Arc::ptr_eq(&original_other, &index.message_projection(other).unwrap()),
            "unrelated conversation vector must remain shared"
        );
    }
    // Later authored messages are not allowed to erase the existing
    // activity card when the incremental index refreshes its owner.
    let old_count = store.events().len();
    author(&mut store, first, "A second question");
    index = index
        .extend_native_appends(store.events(), old_count)
        .unwrap();
    assert_eq!(index.messages(first).len(), 3);
    assert_eq!(
        index
            .messages(first)
            .iter()
            .filter(|entry| entry.role == super::super::DisplayRole::Activity)
            .count(),
        1
    );
    assert!(
        index
            .messages(first)
            .iter()
            .any(|entry| entry.text == "A second question")
    );
    assert!(Arc::ptr_eq(
        &original_other,
        &index.message_projection(other).unwrap()
    ));

    // A corrupt appended lifecycle must not leave cached cards visible.
    let old_count = store.events().len();
    store
        .append_scoped(
            Some(chatarium_store::siwc_agent_activity_audit::activity_scope(
                first, turn,
            )),
            EventKind::SiwcAgentActivityObserved,
            "{not-valid-json".to_owned(),
        )
        .unwrap();
    index = index
        .extend_native_appends(store.events(), old_count)
        .unwrap();
    assert!(
        index
            .messages(first)
            .iter()
            .all(|entry| entry.role != super::super::DisplayRole::Activity)
    );
    assert!(Arc::ptr_eq(
        &original_other,
        &index.message_projection(other).unwrap()
    ));
}

#[test]
fn activity_delta_in_one_chat_does_not_clone_unrelated_activity_cards() {
    use chatarium_store::siwc_agent_activity_audit::append_siwc_agent_activity_checked;
    use chatarium_store::{EventStore, MemoryEventStore};
    let first = LocalConversationId::new();
    let other = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let first_turn = author(&mut store, first, "First");
    let other_turn = author(&mut store, other, "Other");
    let make_frame = |request: &str, sequence: u64, stage: &str| {
        serde_json::json!({
            "type": "agent_activity",
            "source": "siwc_bridge_observation",
            "request_id": request,
            "sequence": sequence,
            "stage": stage,
            "observed_at_utc": "2026-10-13T19:00:00.000Z",
            "elapsed_ms": sequence as f64
        })
        .to_string()
    };
    append_siwc_agent_activity_checked(
        &mut store,
        first,
        first_turn,
        &make_frame("first", 1, "started"),
    )
    .unwrap();
    append_siwc_agent_activity_checked(
        &mut store,
        other,
        other_turn,
        &make_frame("other", 1, "started"),
    )
    .unwrap();
    let before = NativeConversationSearchIndex::build(store.events());
    let original_other = before.message_projection(other).unwrap();
    assert!(
        original_other
            .iter()
            .any(|entry| entry.role == super::super::DisplayRole::Activity)
    );
    let previous_count = store.events().len();
    append_siwc_agent_activity_checked(
        &mut store,
        first,
        first_turn,
        &make_frame("first", 2, "wire_attempted"),
    )
    .unwrap();
    let after = before
        .extend_native_appends(store.events(), previous_count)
        .unwrap();
    assert!(
        after
            .messages(first)
            .iter()
            .any(|entry| entry.role == super::super::DisplayRole::Activity
                && entry.text.contains("wire_attempted"))
    );
    assert!(
        Arc::ptr_eq(&original_other, &after.message_projection(other).unwrap()),
        "untouched activity-bearing conversation must remain shared"
    );
}

#[test]
fn native_index_matches_reference_projection_and_isolates_other_conversations() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let first_turn = author(&mut store, first, "First owner mentions RUST");
    let second_turn = author(&mut store, second, "Other owner mentions PYTHON");
    let assistant = |turn: chatarium_core::LocalTurnId, body: &str| {
        serde_json::json!({
            "schema": "chatarium-responses-turn-observation",
            "version": 1,
            "text": body,
            "details": {
                "local_turn_id": turn.to_string(),
                "request_id": turn.to_string(),
            }
        })
        .to_string()
    };
    store
        .append_scoped(
            Some(local_turn_scope(first_turn)),
            EventKind::AssistantSnapshotObserved,
            assistant(first_turn, "partial answer"),
        )
        .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(second_turn)),
            EventKind::AssistantCompletionObserved,
            assistant(second_turn, "second answer private to second"),
        )
        .unwrap();
    store
        .append_scoped(
            Some(local_turn_scope(first_turn)),
            EventKind::AssistantCompletionObserved,
            assistant(first_turn, "completed first answer"),
        )
        .unwrap();
    // An event with a legitimate assistant scope but a forbidden kind
    // must never be indexed as a native conversation message.
    store
        .append_scoped(
            Some(local_turn_scope(first_turn)),
            EventKind::ToolCallOutcomeObserved,
            "SECRET RESULT NOT A CHAT MESSAGE".to_owned(),
        )
        .unwrap();
    let index = NativeConversationSearchIndex::build(store.events());
    for owner in [first, second] {
        let reference = super::super::projected_local_display_messages(store.events(), owner);
        let actual = index.messages(owner);
        assert_eq!(actual.len(), reference.len());
        assert_eq!(
            actual.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            reference
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            actual.iter().map(|m| m.sequence).collect::<Vec<_>>(),
            reference.iter().map(|m| m.sequence).collect::<Vec<_>>()
        );
    }
    let first_text = index
        .messages(first)
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(first_text.contains("RUST"));
    assert!(first_text.contains("completed first answer"));
    assert!(!first_text.contains("partial answer"));
    assert!(!first_text.contains("second answer private"));
    assert!(!first_text.contains("SECRET RESULT"));
    assert_eq!(
        find_match(
            "RUST",
            "",
            index.messages(first).iter().map(|m| m.text.as_str())
        ),
        Some(NativeSearchMatch::Message)
    );
    assert_eq!(
        find_match(
            "RUST",
            "",
            index.messages(second).iter().map(|m| m.text.as_str())
        ),
        None
    );
}

#[test]
fn conflicting_turn_owners_never_attribute_assistant_text_to_either_conversation() {
    use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
    use chatarium_store::authored::commit_user_message;
    use chatarium_store::{EventStore, MemoryEventStore};

    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let shared_turn = LocalTurnId::new();
    let scope = local_turn_scope(shared_turn);
    let mut store = MemoryEventStore::default();
    let ctx = egui::Context::default();

    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            first,
            shared_turn,
            LocalMessageId::new(),
            "First author's own message",
        ),
    )
    .unwrap();
    let original = cached_index(&ctx, store.events());
    assert_eq!(original.messages(first).len(), 1);

    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            second,
            shared_turn,
            LocalMessageId::new(),
            "Second author's own message",
        ),
    )
    .unwrap();
    let quarantined = cached_index(&ctx, store.events());
    assert!(quarantined.conflicted_scopes.contains(&scope));
    assert!(!quarantined.owners.contains_key(&scope));

    store
        .append_scoped(
            Some(scope.clone()),
            EventKind::AssistantCompletionObserved,
            super::super::remote_turn_payload(
                shared_turn,
                &shared_turn.to_string(),
                None,
                Some("AMBIGUOUS_ASSISTANT_PRIVATE_TEXT"),
                None,
            ),
        )
        .unwrap();
    let rebuilt = cached_index(&ctx, store.events());
    for (owner, expected, foreign) in [
        (
            first,
            "First author's own message",
            "Second author's own message",
        ),
        (
            second,
            "Second author's own message",
            "First author's own message",
        ),
    ] {
        let messages = rebuilt.messages(owner);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, expected);
        assert!(!messages[0].text.contains(foreign));
        assert!(
            messages
                .iter()
                .all(|message| { !message.text.contains("AMBIGUOUS_ASSISTANT_PRIVATE_TEXT") })
        );
    }
    assert!(rebuilt.conflicted_scopes.contains(&scope));
    assert!(!rebuilt.owners.contains_key(&scope));

    // A further authored event using the same conflicted turn must not
    // revive the assistant scope through incremental cache extension.
    commit_user_message(
        &mut store,
        &AuthoredUserMessage::new(
            first,
            shared_turn,
            LocalMessageId::new(),
            "Another first-author event",
        ),
    )
    .unwrap();
    let after_append = cached_index(&ctx, store.events());
    assert!(after_append.conflicted_scopes.contains(&scope));
    assert!(!after_append.owners.contains_key(&scope));
    assert!(
        after_append
            .messages(second)
            .iter()
            .all(|message| { !message.text.contains("AMBIGUOUS_ASSISTANT_PRIVATE_TEXT") })
    );
}

#[test]
fn single_pass_row_preserves_unicode_title_and_multi_hit_semantics() {
    let bodies = [
        "İstanbul and İzmir",
        "El NIÑO sonríe",
        "ΑΘΗΝΑ και Ελλάδα",
        "مرحبا بالعالم 🌿",
        "日本語の世界",
    ];
    let messages = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| DisplayMessage {
            role: super::super::DisplayRole::User,
            text: (*body).to_owned(),
            sequence: index as u64 + 1,
            timestamp: None,
            provenance_label: None,
        })
        .collect::<Vec<_>>();
    let title = "Title-only fallback";
    for query in [
        "i",
        "İ",
        "niño",
        "αθηνα",
        "ΕΛΛΑΔΑ",
        "مرحبا",
        "🌿",
        "世界",
        "fallback",
        "nonexistent",
    ] {
        let needle = query.trim().to_lowercase();
        let row = collect_native_search_row(&needle, 0, title.to_owned(), &messages);
        let reference = find_match_with_position(
            query,
            title,
            messages.iter().map(|message| message.text.as_str()),
        );
        assert_eq!(
            row.as_ref().map(|row| row.kind),
            reference.as_ref().map(|(kind, _)| *kind),
            "classification differs for {query}"
        );
        assert_eq!(
            row.as_ref().and_then(|row| row.message_index),
            reference
                .as_ref()
                .and_then(|(_, hit)| hit.as_ref().map(|(index, _)| *index)),
            "first match differs for {query}"
        );
        assert_eq!(
            row.as_ref().and_then(|row| row.preview.as_ref()),
            reference
                .as_ref()
                .and_then(|(_, hit)| hit.as_ref().map(|(_, excerpt)| excerpt)),
            "preview differs for {query}"
        );
        let expected = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                message
                    .text
                    .to_lowercase()
                    .contains(&needle)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if let Some(row) = row {
            assert_eq!(
                row.matching_message_indices, expected,
                "hits differ for {query}"
            );
        } else {
            assert!(expected.is_empty());
        }
    }
}

#[test]
fn native_index_cache_reuses_drafts_but_invalidates_for_tool_audit_appends() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let turn = author(&mut store, owner, "initial typed message");
    let base = cached_index(&ctx, store.events());
    store
        .append_scoped(
            None,
            EventKind::DraftChanged,
            "draft input updated".to_owned(),
        )
        .unwrap();
    let after_draft = cached_index(&ctx, store.events());
    assert!(Arc::ptr_eq(&base, &after_draft));
    store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::ToolCallOutcomeObserved,
            "tool outcome must not be indexed".to_owned(),
        )
        .unwrap();
    let after_tool = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&base, &after_tool));
    assert!(
        after_tool
            .messages(owner)
            .iter()
            .all(|message| !message.text.contains("tool outcome must not be indexed"))
    );

    store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::AssistantCompletionObserved,
            serde_json::json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "text": "newly observed assistant message",
                "details": {
                    "local_turn_id": turn.to_string(),
                    "request_id": turn.to_string()
                }
            })
            .to_string(),
        )
        .unwrap();
    let changed = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&base, &changed));
    assert!(
        changed
            .messages(owner)
            .iter()
            .any(|message| { message.text == "newly observed assistant message" })
    );
}

#[test]
fn reader_hit_cache_reuses_unchanged_native_messages_and_invalidates_on_append() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let unrelated = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, owner, "HELLO hello");
    author(&mut store, unrelated, "private unrelated content");

    let original = cached_index(&ctx, store.events());
    let hits = cached_reader_hits(&ctx, &original, owner, "hello");
    assert_eq!(hits.as_slice(), &[(0, 0, 5), (0, 6, 11)]);
    assert!(Arc::ptr_eq(
        &hits,
        &cached_reader_hits(&ctx, &original, owner, "hello"),
    ));
    assert!(cached_reader_hits(&ctx, &original, owner, "absent").is_empty());
    assert_eq!(
        cached_reader_hits(&ctx, &original, owner, "hello").as_slice(),
        &[(0, 0, 5), (0, 6, 11)],
    );
    let warm_before_unrelated = cached_reader_hits(&ctx, &original, owner, "hello");

    author(&mut store, unrelated, "different private append");
    let unrelated_append = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&original, &unrelated_append));
    assert!(Arc::ptr_eq(
        original.messages.get(&owner).unwrap(),
        unrelated_append.messages.get(&owner).unwrap(),
    ));
    let original_again = cached_reader_hits(&ctx, &unrelated_append, owner, "hello");
    assert!(Arc::ptr_eq(&warm_before_unrelated, &original_again));
    assert!(Arc::ptr_eq(
        &original_again,
        &cached_reader_hits(&ctx, &unrelated_append, owner, "hello"),
    ));

    author(&mut store, owner, "hello from this conversation");
    let own_append = cached_index(&ctx, store.events());
    let expanded = cached_reader_hits(&ctx, &own_append, owner, "hello");
    assert_eq!(expanded.as_slice(), &[(0, 0, 5), (0, 6, 11), (1, 0, 5)]);
    assert!(!Arc::ptr_eq(&original_again, &expanded));
    assert!(cached_reader_hits(&ctx, &own_append, unrelated, "hello").is_empty());
    assert!(cached_reader_hits(&ctx, &own_append, owner, "  ").is_empty());
}

#[test]
fn sidebar_results_cache_tracks_query_catalog_and_visible_messages() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(first, 1);
    catalog.create(second, 2);
    // Neutral catalog titles ensure the initial hits come from message
    // bodies; derived titles may repeat the first message.
    catalog.rename(first, Some("One".to_owned()), 1).unwrap();
    catalog.rename(second, Some("Two".to_owned()), 2).unwrap();

    let mut store = MemoryEventStore::default();
    author(&mut store, first, "Rust renderer");
    author(&mut store, second, "Python tools");
    let index = cached_index(&ctx, store.events());
    let entries = catalog.entries();

    let initial = cached_rows(&ctx, &index, &entries, false, "rust");
    assert_eq!(initial.len(), 1);
    assert_eq!(entries[initial[0].catalog_index].id, first);
    assert_eq!(initial[0].kind, NativeSearchMatch::Message);
    assert_eq!(initial[0].message_index, Some(0));
    assert_eq!(
        index.messages(first)[initial[0].message_index.unwrap()].text,
        "Rust renderer"
    );
    let repeated = cached_rows(&ctx, &index, &entries, false, "rust");
    assert!(Arc::ptr_eq(&initial, &repeated));

    store
        .append_scoped(None, EventKind::DraftChanged, "unrelated".to_owned())
        .unwrap();
    let unchanged_index = cached_index(&ctx, store.events());
    assert!(Arc::ptr_eq(&index, &unchanged_index));
    let unchanged = cached_rows(&ctx, &unchanged_index, &entries, false, "rust");
    assert!(Arc::ptr_eq(&initial, &unchanged));

    let changed_query = cached_rows(&ctx, &index, &entries, false, "python");
    assert!(!Arc::ptr_eq(&initial, &changed_query));
    assert_eq!(changed_query.len(), 1);
    assert_eq!(entries[changed_query[0].catalog_index].id, second);

    catalog
        .rename(first, Some("Python project".to_owned()), 3)
        .unwrap();
    let renamed_entries = catalog.entries();
    let renamed = cached_rows(&ctx, &index, &renamed_entries, false, "python");
    assert_eq!(renamed.len(), 2);
    assert_eq!(renamed[0].kind, NativeSearchMatch::Title);
    assert_eq!(renamed[0].message_index, None);
    assert!(!Arc::ptr_eq(&changed_query, &renamed));

    catalog.set_archived(first, true, 4).unwrap();
    let archived_entries = catalog.entries();
    let hidden = cached_rows(&ctx, &index, &archived_entries, false, "python");
    assert_eq!(hidden.len(), 1);
    let shown = cached_rows(&ctx, &index, &archived_entries, true, "python");
    assert_eq!(shown.len(), 2);

    author(&mut store, second, "A new rust discussion");
    let advanced_index = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&index, &advanced_index));
    let fresh = cached_rows(&ctx, &advanced_index, &archived_entries, false, "rust");
    assert_eq!(fresh.len(), 1);
    assert_eq!(archived_entries[fresh[0].catalog_index].id, second);
}

#[test]
fn sidebar_rows_reuse_unchanged_conversations_after_other_chat_appends() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let third = LocalConversationId::new();
    let mut catalog = LocalConversationCatalog::default();
    for (owner, when, title) in [
        (first, 1, "First"),
        (second, 2, "Second"),
        (third, 3, "Third"),
    ] {
        catalog.create(owner, when);
        catalog.rename(owner, Some(title.to_owned()), when).unwrap();
    }
    let entries = catalog.entries();
    let mut store = MemoryEventStore::default();
    author(&mut store, first, "needle in the first chat");
    author(&mut store, second, "no initial match");
    author(&mut store, third, "needle in the third chat");
    let initial_index = cached_index(&ctx, store.events());
    let initial = cached_rows(&ctx, &initial_index, &entries, false, "needle");
    assert_eq!(initial.len(), 2);
    let cache_id = egui::Id::new("chatarium-native-conversation-search-rows");
    let before = ctx
        .data_mut(|data| data.get_temp::<NativeRowsCache>(cache_id))
        .unwrap();

    // A new message changes only its owner's projection; the two earlier
    // hits remain correct, and a previously cached miss becomes visible.
    author(&mut store, second, "needle appears here now");
    let updated_index = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&initial_index, &updated_index));
    assert!(Arc::ptr_eq(
        initial_index.messages.get(&first).unwrap(),
        updated_index.messages.get(&first).unwrap()
    ));
    assert!(Arc::ptr_eq(
        initial_index.messages.get(&third).unwrap(),
        updated_index.messages.get(&third).unwrap()
    ));
    let updated = cached_rows(&ctx, &updated_index, &entries, false, "needle");
    assert_eq!(updated.len(), 3);
    assert!(
        updated
            .iter()
            .any(|row| entries[row.catalog_index].id == second
                && row.message_index == Some(1)
                && row.summary.message_count == 2)
    );
    let after = ctx
        .data_mut(|data| data.get_temp::<NativeRowsCache>(cache_id))
        .unwrap();
    for owner in [first, third] {
        let position = entries.iter().position(|entry| entry.id == owner).unwrap();
        assert!(Arc::ptr_eq(
            before.row_sources[position].as_ref().unwrap(),
            after.row_sources[position].as_ref().unwrap()
        ));
        assert_eq!(
            before.rows.iter().find(|row| row.catalog_index == position),
            after.rows.iter().find(|row| row.catalog_index == position)
        );
    }
    assert!(Arc::ptr_eq(
        &updated,
        &cached_rows(&ctx, &updated_index, &entries, false, "needle")
    ));

    // An unchanged search miss must stay excluded after an unrelated
    // journal append, but the owner's own append must invalidate it.
    let absent = cached_rows(&ctx, &updated_index, &entries, false, "new topic");
    assert!(absent.is_empty());
    author(&mut store, first, "still no matching phrase");
    let next_index = cached_index(&ctx, store.events());
    assert!(cached_rows(&ctx, &next_index, &entries, false, "new topic").is_empty());
    author(&mut store, third, "a new topic belongs to this chat");
    let last_index = cached_index(&ctx, store.events());
    let found = cached_rows(&ctx, &last_index, &entries, false, "new topic");
    assert_eq!(found.len(), 1);
    assert_eq!(entries[found[0].catalog_index].id, third);
    assert_eq!(found[0].message_index, Some(1));
    assert_eq!(found[0].summary.message_count, 2);
}

#[test]
fn streaming_assistant_updates_only_reproject_the_owned_conversation() {
    use chatarium_store::{EventStore, MemoryEventStore};

    fn assert_same_messages(actual: &[DisplayMessage], expected: &[DisplayMessage]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.role, expected.role);
            assert_eq!(actual.text, expected.text);
            assert_eq!(actual.sequence, expected.sequence);
            assert_eq!(actual.timestamp, expected.timestamp);
            assert_eq!(actual.provenance_label, expected.provenance_label);
        }
    }

    let ctx = egui::Context::default();
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let first_turn = author(&mut store, first, "First conversation");
    author(&mut store, second, "Second private conversation");

    let initial = cached_index(&ctx, store.events());
    let other_messages = initial.messages.get(&second).unwrap();

    for (kind, text) in [
        (EventKind::AssistantSnapshotObserved, "partial reply"),
        (EventKind::AssistantCompletionObserved, "completed reply"),
    ] {
        store
            .append_scoped(
                Some(local_turn_scope(first_turn)),
                kind,
                serde_json::json!({
                    "schema": "chatarium-responses-turn-observation",
                    "version": 1,
                    "text": text,
                    "details": {
                        "local_turn_id": first_turn.to_string(),
                        "request_id": first_turn.to_string(),
                    }
                })
                .to_string(),
            )
            .unwrap();
        let current = cached_index(&ctx, store.events());
        let full = NativeConversationSearchIndex::build(store.events());
        for owner in [first, second] {
            assert_same_messages(current.messages(owner), full.messages(owner));
        }
        assert!(Arc::ptr_eq(
            other_messages,
            current.messages.get(&second).unwrap()
        ));
        assert!(
            !current
                .messages(first)
                .iter()
                .any(|m| m.text.contains("private"))
        );
        assert!(current.messages(first).iter().any(|m| m.text == text));
    }

    // A new native authored turn is replayed without touching the
    // unrelated conversation's projection.
    author(&mut store, first, "A new turn");
    let after_user = cached_index(&ctx, store.events());
    assert_same_messages(
        after_user.messages(first),
        NativeConversationSearchIndex::build(store.events()).messages(first),
    );
    assert!(Arc::ptr_eq(
        other_messages,
        after_user.messages.get(&second).unwrap()
    ));
}

#[test]
fn many_conversations_preserve_unaffected_projection_on_new_turns() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let mut store = MemoryEventStore::default();
    let conversations = (0..128)
        .map(|index| {
            let owner = LocalConversationId::new();
            for message in 0..4 {
                author(
                    &mut store,
                    owner,
                    &format!("archive {index} entry {message}"),
                );
            }
            owner
        })
        .collect::<Vec<_>>();
    let target = conversations[32];
    let index = cached_index(&ctx, store.events());
    let untouched = conversations
        .iter()
        .filter(|owner| **owner != target)
        .map(|owner| (*owner, Arc::clone(index.messages.get(owner).unwrap())))
        .collect::<Vec<_>>();

    // The full index would scan hundreds of historical events here.
    // The incremental projection must preserve all other message Arcs.
    let new_turn = author(&mut store, target, "new isolated user turn");
    let after_user = cached_index(&ctx, store.events());
    assert_eq!(after_user.messages(target).len(), 5);
    for (owner, previous) in &untouched {
        assert!(Arc::ptr_eq(
            previous,
            after_user.messages.get(owner).unwrap()
        ));
    }
    store
        .append_scoped(
            Some(local_turn_scope(new_turn)),
            EventKind::AssistantCompletionObserved,
            serde_json::json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "text": "new isolated assistant reply",
                "details": {
                    "local_turn_id": new_turn.to_string(),
                    "request_id": new_turn.to_string(),
                }
            })
            .to_string(),
        )
        .unwrap();
    let after_answer = cached_index(&ctx, store.events());
    let rebuilt = NativeConversationSearchIndex::build(store.events());
    for owner in &conversations {
        let actual = after_answer.messages(*owner);
        let expected = rebuilt.messages(*owner);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.text, expected.text);
            assert_eq!(actual.sequence, expected.sequence);
            assert_eq!(actual.role, expected.role);
        }
    }
    for (owner, previous) in &untouched {
        assert!(Arc::ptr_eq(
            previous,
            after_answer.messages.get(owner).unwrap()
        ));
    }

    // Unknown assistant ownership forces the conservative full rebuild.
    store
        .append_scoped(
            Some("unknown-local-turn".to_owned()),
            EventKind::AssistantCompletionObserved,
            "UNOWNED_PRIVATE_OUTPUT".to_owned(),
        )
        .unwrap();
    let fallback = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(
        &untouched[0].1,
        fallback.messages.get(&untouched[0].0).unwrap()
    ));
    assert!(conversations.iter().all(|owner| {
        fallback
            .messages(*owner)
            .iter()
            .all(|message| !message.text.contains("UNOWNED_PRIVATE_OUTPUT"))
    }));
}

#[test]
fn batched_new_authorship_and_assistant_use_full_builder_ordering() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let mut store = MemoryEventStore::default();
    let other = LocalConversationId::new();
    author(&mut store, other, "untouched conversation");
    let original = cached_index(&ctx, store.events());

    let owner = LocalConversationId::new();
    let turn = author(&mut store, owner, "new conversation");
    store
        .append_scoped(
            Some(local_turn_scope(turn)),
            EventKind::AssistantCompletionObserved,
            serde_json::json!({
                "schema": "chatarium-responses-turn-observation",
                "version": 1,
                "text": "new answer",
                "details": {
                    "local_turn_id": turn.to_string(),
                    "request_id": turn.to_string()
                }
            })
            .to_string(),
        )
        .unwrap();

    let extended = cached_index(&ctx, store.events());
    let rebuilt = NativeConversationSearchIndex::build(store.events());
    let expected = rebuilt.messages(owner);
    let actual = extended.messages(owner);
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        actual
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>()
    );
    assert!(Arc::ptr_eq(
        original.messages.get(&other).unwrap(),
        extended.messages.get(&other).unwrap()
    ));
}

#[test]
fn native_index_cache_rebuilds_if_journal_replaces_prior_tail() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, owner, "first");
    let cached = cached_index(&ctx, store.events());
    let mut replaced = store.events().to_vec();
    replaced.last_mut().unwrap().scope = Some("different-scope".to_owned());
    let next = cached_index(&ctx, &replaced);
    assert!(!Arc::ptr_eq(&cached, &next));
}

#[test]
fn native_index_cache_rebuilds_when_same_sized_tail_payload_changes() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, owner, "first");
    let original = cached_index(&ctx, store.events());
    assert_eq!(original.messages(owner)[0].text, "first");

    // Preserve every old revision discriminator, including the byte
    // length, while changing the source text of a durable user message.
    let mut replaced = store.events().to_vec();
    let tail = replaced.last_mut().unwrap();
    let new_payload = tail.payload.replacen("first", "other", 1);
    assert_ne!(new_payload, tail.payload);
    assert_eq!(new_payload.len(), tail.payload.len());
    tail.payload = new_payload;

    let refreshed = cached_index(&ctx, &replaced);
    assert!(!Arc::ptr_eq(&original, &refreshed));
    assert_eq!(refreshed.messages(owner)[0].text, "other");
    assert!(Arc::ptr_eq(&refreshed, &cached_index(&ctx, &replaced)));
}

#[test]
fn equal_length_replaced_archive_with_same_tail_rebuilds_native_search() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, owner, "unchanged first");
    author(&mut store, owner, "interior old");
    author(&mut store, owner, "unchanged tail");

    let cached = cached_index(&ctx, store.events());
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(owner, 1);
    catalog
        .rename(owner, Some("Neutral title".to_owned()), 2)
        .unwrap();
    let entries = catalog.entries();
    assert_eq!(cached_rows(&ctx, &cached, &entries, false, "old").len(), 1);

    // Preserve event count, first/last envelopes and replacement size.
    // Only the interior of a separately allocated journal is different.
    let mut swapped = store.events().to_vec();
    let original = swapped[1].payload.clone();
    swapped[1].payload = original.replacen("interior old", "interior new", 1);
    assert_ne!(swapped[1].payload, original);
    assert_eq!(swapped[1].payload.len(), original.len());
    assert_eq!(swapped.first(), store.events().first());
    assert_eq!(swapped.last(), store.events().last());

    let rebuilt = cached_index(&ctx, &swapped);
    assert!(!Arc::ptr_eq(&cached, &rebuilt));
    assert_eq!(rebuilt.messages(owner)[1].text, "interior new");
    assert!(cached_rows(&ctx, &rebuilt, &entries, false, "old").is_empty());
    let updated = cached_rows(&ctx, &rebuilt, &entries, false, "new");
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].message_index, Some(1));
    assert!(Arc::ptr_eq(&rebuilt, &cached_index(&ctx, &swapped)));
}

#[test]
fn native_index_empty_and_journal_append_revision_are_distinct() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let empty = NativeConversationSearchIndex::build(store.events());
    assert!(empty.messages(owner).is_empty());
    assert!(empty.matches_journal(store.events()));
    author(&mut store, owner, "A newly durable chat");
    assert!(!empty.matches_journal(store.events()));
    let updated = NativeConversationSearchIndex::build(store.events());
    assert!(updated.matches_journal(store.events()));
    assert_eq!(updated.messages(owner).len(), 1);
}

#[test]
fn cached_native_index_reuses_unchanged_journal_then_rebuilds_on_append() {
    use chatarium_store::{EventStore, MemoryEventStore};
    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    let first = cached_index(&ctx, store.events());
    let same = cached_index(&ctx, store.events());
    assert!(Arc::ptr_eq(&first, &same));
    author(&mut store, owner, "new message");
    let later = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&first, &later));
    assert_eq!(later.messages(owner).len(), 1);
}

#[test]
fn title_matches_do_not_invent_reader_hits_but_message_matches_can_jump() {
    assert_eq!(
        reader_query_for_result("  México  ", NativeSearchMatch::Message),
        Some("México".to_owned())
    );
    assert_eq!(
        reader_query_for_result(" query ", NativeSearchMatch::Title),
        None
    );
    assert_eq!(
        reader_query_for_result("  ", NativeSearchMatch::Message),
        None
    );
}

#[test]
fn keyboard_copy_targets_only_the_highlighted_visible_message() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    assert_eq!(activate_copy_selection(&[], None), None);
    let candidates = [(first, None), (second, Some(4))];
    assert_eq!(activate_copy_selection(&candidates, None), None);
    assert_eq!(activate_copy_selection(&candidates, Some(0)), None);
    assert_eq!(
        activate_copy_selection(&candidates, Some(1)),
        Some((second, 4))
    );
    assert_eq!(activate_copy_selection(&candidates, Some(99)), None);
    assert_eq!(
        activate_copy_selection(&[(first, Some(2))], None),
        Some((first, 2))
    );
}

#[test]
fn explicit_hit_export_contains_only_one_owned_visible_message() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let own = LocalConversationId::new();
    let other = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, own, "first unrelated message");
    author(&mut store, own, "wanted visible message");
    author(&mut store, other, "FOREIGN_PRIVATE_MESSAGE");
    store
        .append_scoped(
            None,
            EventKind::ToolCallOutcomeObserved,
            "HIDDEN_TOOL_SECRET".to_owned(),
        )
        .unwrap();

    let ctx = egui::Context::default();
    let index = cached_index(&ctx, store.events());
    let messages = index.messages(own);
    let hit = find_match_with_position(
        "wanted",
        "Neutral title",
        messages.iter().map(|message| message.text.as_str()),
    )
    .unwrap();
    let position = hit.1.unwrap().0;
    let (markdown, event_sequence) =
        copyable_hit_markdown(&index, own, "Neutral title", position).unwrap();

    assert_eq!(event_sequence, messages[position].sequence);
    assert!(markdown.contains("wanted visible message"));
    assert!(markdown.contains(&own.to_string()));
    assert!(markdown.contains(&format!("event #{event_sequence}")));
    assert!(!markdown.contains("first unrelated message"));
    assert!(!markdown.contains("FOREIGN_PRIVATE_MESSAGE"));
    assert!(!markdown.contains("HIDDEN_TOOL_SECRET"));
    assert!(copyable_hit_markdown(&index, own, "Neutral title", 99).is_none());
}

#[test]
fn staging_a_native_message_keeps_exact_source_and_excludes_foreign_data() {
    use chatarium_store::{EventStore, MemoryEventStore};

    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, first, "first visible source");
    author(&mut store, second, "private foreign message");
    store
        .append_scoped(
            None,
            EventKind::ToolCallOutcomeObserved,
            "HIDDEN_TOOL".to_owned(),
        )
        .unwrap();

    let index = NativeConversationSearchIndex::build(store.events());
    let staged = stageable_hit(&index, first, 0).unwrap();
    assert_eq!(staged.source_conversation_id, first);
    assert_eq!(
        staged.source_event_sequence,
        index.messages(first)[0].sequence
    );
    assert_eq!(staged.text, "first visible source");
    assert!(!staged.text.contains("private foreign"));
    assert!(!staged.text.contains("HIDDEN_TOOL"));
    assert!(stageable_hit(&index, first, 1).is_none());
    assert_eq!(
        stageable_hit(&index, second, 0)
            .unwrap()
            .source_conversation_id,
        second
    );
}

#[test]
fn derived_title_hit_preserves_explicit_copy_and_memory_staging() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let source = LocalConversationId::new();
    let foreign = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(
        &mut store,
        source,
        "Reusable statement from the first message",
    );
    author(&mut store, foreign, "PRIVATE FOREIGN MESSAGE");

    let mut catalog = LocalConversationCatalog::default();
    catalog.create(source, 1);
    catalog.create(foreign, 2);
    let ctx = egui::Context::default();
    let index = cached_index(&ctx, store.events());
    let entries = catalog.entries();
    let rows = cached_rows(&ctx, &index, &entries, false, "reusable");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, NativeSearchMatch::Message);
    assert_eq!(rows[0].message_index, Some(0));
    assert!(rows[0].preview.as_deref().unwrap().contains("Reusable"));
    assert_eq!(entries[rows[0].catalog_index].id, source);

    let (owner, message_index) =
        activate_copy_selection(&[(source, rows[0].message_index)], None).unwrap();
    let (exported, sequence) =
        copyable_hit_markdown(&index, owner, &rows[0].title, message_index).unwrap();
    let staged = stageable_hit(&index, owner, message_index).unwrap();
    assert_eq!(staged.source_event_sequence, sequence);
    assert_eq!(staged.source_conversation_id, source);
    assert_eq!(staged.text, "Reusable statement from the first message");
    assert!(exported.contains(&staged.text));
    assert!(!exported.contains("PRIVATE FOREIGN MESSAGE"));
    assert_eq!(
        reader_query_for_result(" reusable ", rows[0].kind),
        Some("reusable".to_owned())
    );

    // A custom title alone must never fabricate a message to copy or stage.
    catalog
        .rename(source, Some("Title-only keyword".to_owned()), 3)
        .unwrap();
    let renamed = catalog.entries();
    let title_only = cached_rows(&ctx, &index, &renamed, false, "keyword");
    assert_eq!(title_only.len(), 1);
    assert_eq!(title_only[0].kind, NativeSearchMatch::Title);
    assert_eq!(title_only[0].message_index, None);
}

#[test]
fn multiple_matching_messages_have_independent_provenance_and_reader_targets() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let source = LocalConversationId::new();
    let foreign = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, source, "target first");
    author(&mut store, source, "not a match");
    author(&mut store, source, "target second");
    author(&mut store, foreign, "PRIVATE target foreign");
    author(&mut store, source, "target third");

    let mut catalog = LocalConversationCatalog::default();
    catalog.create(source, 1);
    catalog.create(foreign, 2);
    catalog
        .rename(source, Some("Independent title".to_owned()), 3)
        .unwrap();
    catalog
        .rename(foreign, Some("Unrelated".to_owned()), 4)
        .unwrap();
    let ctx = egui::Context::default();
    let index = cached_index(&ctx, store.events());
    let entries = catalog.entries();
    let rows = cached_rows(&ctx, &index, &entries, false, "target");
    let row = rows
        .iter()
        .find(|row| entries[row.catalog_index].id == source)
        .unwrap();
    assert_eq!(row.matching_message_indices, vec![0, 2, 3]);
    assert_eq!(row.message_index, Some(0));
    assert_eq!(selected_message_index(row, 1), Some(2));
    assert_eq!(selected_message_index(row, 2), Some(3));
    assert_eq!(selected_message_index(row, 999), Some(3));
    assert_eq!(cycle_matching_message(row, 0, 1), Some((1, 2)));
    assert_eq!(cycle_matching_message(row, 2, 1), Some((0, 0)));
    assert_eq!(cycle_matching_message(row, 0, -1), Some((2, 3)));
    assert_eq!(cycle_matching_message(row, 999, 1), Some((0, 0)));
    assert_eq!(
        reader_hit_ordinal(index.messages(source), "target", 2),
        Some(1)
    );
    assert_eq!(
        reader_hit_ordinal(index.messages(source), "target", 1),
        None
    );

    for (ordinal, expected) in [
        (0, "target first"),
        (1, "target second"),
        (2, "target third"),
    ] {
        let position = selected_message_index(row, ordinal).unwrap();
        let staged = stageable_hit(&index, source, position).unwrap();
        let (copied, seq) =
            copyable_hit_markdown(&index, source, &row.title, position).unwrap();
        assert_eq!(staged.text, expected);
        assert_eq!(staged.source_event_sequence, seq);
        assert_eq!(staged.source_conversation_id, source);
        assert!(copied.contains(expected));
        assert!(!copied.contains("PRIVATE target foreign"));
    }

    // A repeated query reuses its cached snapshot until it changes.
    assert!(Arc::ptr_eq(
        &rows,
        &cached_rows(&ctx, &index, &entries, false, "target"),
    ));
    let title_only = cached_rows(&ctx, &index, &entries, false, "independent");
    let row = title_only
        .iter()
        .find(|row| entries[row.catalog_index].id == source)
        .unwrap();
    assert_eq!(row.kind, NativeSearchMatch::Title);
    assert!(row.matching_message_indices.is_empty());
    assert_eq!(selected_message_index(row, 0), None);
    assert_eq!(cycle_matching_message(row, 0, 1), None);
}

#[test]
fn native_rows_match_reference_search_semantics_across_scripts() {
    use super::super::local_conversations::LocalConversationCatalog;
    use chatarium_store::{EventStore, MemoryEventStore};

    let ctx = egui::Context::default();
    let owner = LocalConversationId::new();
    let isolated = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    for body in [
        "İstanbul first",
        "not related",
        "MÉXICO second",
        "the rust user guide",
        "rust again",
        "مرحبا بالعالم",
        "emoji 🦀 second",
    ] {
        author(&mut store, owner, body);
    }
    author(&mut store, isolated, "PRIVATE title-exclusive phrase");
    let index = cached_index(&ctx, store.events());
    let mut catalog = LocalConversationCatalog::default();
    catalog.create(owner, 1);
    catalog.create(isolated, 2);
    catalog
        .rename(owner, Some("Rust México Title".to_owned()), 3)
        .unwrap();
    catalog
        .rename(isolated, Some("Other".to_owned()), 4)
        .unwrap();
    let entries = catalog.entries();

    for query in [
        "",
        "  ",
        "rust",
        "RUST",
        " méxico ",
        "i",
        "İ",
        "🦀",
        "مرحبا",
        "title",
        "missing",
        "private",
    ] {
        let rows = cached_rows(&ctx, &index, &entries, false, query);
        for entry in &entries {
            let title = entry.title.clone().unwrap();
            let messages = index.messages(entry.id);
            let reference = find_match_with_position(
                query,
                &title,
                messages.iter().map(|message| message.text.as_str()),
            );
            let actual = rows
                .iter()
                .find(|row| entries[row.catalog_index].id == entry.id);
            match (reference, actual) {
                (None, None) => {}
                (Some((kind, hit)), Some(row)) => {
                    assert_eq!(row.kind, kind, "{query}");
                    assert_eq!(
                        row.message_index,
                        hit.as_ref().map(|(position, _)| *position),
                        "{query}"
                    );
                    assert_eq!(row.preview, hit.map(|(_, excerpt)| excerpt), "{query}");
                    let needle = query.trim().to_lowercase();
                    let expected = if needle.is_empty() {
                        Vec::new()
                    } else {
                        messages
                            .iter()
                            .enumerate()
                            .filter_map(|(position, message)| {
                                message
                                    .text
                                    .to_lowercase()
                                    .contains(&needle)
                                    .then_some(position)
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(row.matching_message_indices, expected, "{query}");
                }
                _ => panic!("native result diverged for query {query:?}"),
            }
        }
    }
}

#[test]
fn recording_staged_message_remains_excluded_until_destination_admits_it() {
    use chatarium_core::local_memory::LocalMemoryId;
    use chatarium_store::local_memory_audit::{
        record_local_memory_artifact, replay_local_memory_audit,
    };
    use chatarium_store::local_memory_context_audit::{
        LocalMemoryContextDecision, record_local_memory_context_decision,
        replay_admitted_local_memory_context,
    };
    use chatarium_store::{EventStore, MemoryEventStore};

    let source = LocalConversationId::new();
    let destination = LocalConversationId::new();
    let unrelated = LocalConversationId::new();
    let mut store = MemoryEventStore::default();
    author(&mut store, source, "preserved source statement");

    let projected = NativeConversationSearchIndex::build(store.events());
    let staged = stageable_hit(&projected, source, 0).unwrap();
    let staged_origin = Some((staged.source_conversation_id, staged.source_event_sequence));
    assert_eq!(
        recording_source_conversation(destination, staged_origin),
        source
    );
    assert_eq!(
        recording_source_conversation(destination, None),
        destination
    );
    let memory_id = LocalMemoryId::new(1);
    record_local_memory_artifact(
        &mut store,
        memory_id,
        recording_source_conversation(destination, staged_origin),
        staged.text,
    )
    .unwrap();

    let artifacts = replay_local_memory_audit(store.events()).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].source_conversation_id, source);
    assert_eq!(artifacts[0].text, "preserved source statement");

    // Recording a memory is never itself an inference-context admission.
    assert!(
        replay_admitted_local_memory_context(store.events(), destination)
            .unwrap()
            .is_empty()
    );
    record_local_memory_context_decision(
        &mut store,
        memory_id,
        destination,
        LocalMemoryContextDecision::Admit,
    )
    .unwrap();
    let admitted = replay_admitted_local_memory_context(store.events(), destination).unwrap();
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].source_conversation_id, source);
    assert!(
        replay_admitted_local_memory_context(store.events(), unrelated)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_search_enter_targets_selection_or_first_match_safely() {
    let first = LocalConversationId::new();
    let second = LocalConversationId::new();
    assert_eq!(activate_selection(&[], None), None);
    assert_eq!(activate_selection(&[first, second], None), Some(first));
    assert_eq!(activate_selection(&[first, second], Some(1)), Some(second));
    assert_eq!(activate_selection(&[first, second], Some(99)), Some(first));
}

#[test]
fn snippet_is_bounded_to_matching_context_and_handles_unicode_case_folding() {
    let long = format!("{}find MIDDLE HERE{}", "p".repeat(150), "s".repeat(300));
    let snippet = matching_message_preview([long.as_str()], "middle").unwrap();
    assert!(snippet.contains("MIDDLE"));
    assert!(snippet.starts_with('…'));
    assert!(snippet.ends_with('…'));
    assert!(snippet.chars().count() < 180);

    let unicode = "İstanbul is a city";
    assert_eq!(
        matching_message_preview([unicode], "i"),
        Some(unicode.to_owned())
    );
    assert_eq!(
        matching_message_preview(["EL NIÑO\nsueña"], "niño"),
        Some("EL NIÑO ↵ sueña".to_owned())
    );
    assert!(matching_message_preview(["Private other conversation"], "absent").is_none());
    assert!(matching_message_preview(["Hello"], "  ").is_none());
}

#[test]
fn snippets_scan_only_supplied_messages_not_tool_or_other_conversation_bodies() {
    assert_eq!(
        matching_message_preview(["unrelated", "SECOND owner match"], "match"),
        Some("SECOND owner match".to_owned())
    );
    assert!(matching_message_preview(["safe native message"], "tool secret").is_none());
}

#[test]
fn matching_positions_identify_only_visible_first_message_hits() {
    let messages = ["not this", "مرحبا México", "MÉXICO later"];
    let found = find_match_with_position("méxico", "Other", messages);
    assert_eq!(
        found,
        Some((
            NativeSearchMatch::Message,
            Some((1, "مرحبا México".to_owned())),
        ))
    );
    assert_eq!(
        find_match_with_position("mex", "Mex title", messages),
        Some((NativeSearchMatch::Title, None))
    );
    assert_eq!(
        find_match_with_position("", "Whatever", messages),
        Some((NativeSearchMatch::Title, None))
    );
    assert_eq!(find_match_with_position("absent", "Other", messages), None);
}

#[test]
fn combined_match_and_preview_stops_at_first_hit_without_scanning_twice() {
    use std::cell::Cell;

    let visited = Cell::new(0);
    let messages = ["unrelated", "México ↵ matters", "later matching text"];
    let counted = messages
        .iter()
        .copied()
        .inspect(|_| visited.set(visited.get() + 1));
    let result = find_match_with_preview(" MÉXICO ", "Other title", counted);
    let expected = (
        NativeSearchMatch::Message,
        Some("México ↵ matters".to_owned()),
    );
    assert_eq!(result, Some(expected));
    assert_eq!(visited.get(), 2);

    visited.set(0);
    let counted = messages
        .iter()
        .copied()
        .inspect(|_| visited.set(visited.get() + 1));
    let title_result = find_match_with_preview("mex", "MEX title", counted);
    assert_eq!(title_result, Some((NativeSearchMatch::Title, None)));
    assert_eq!(
        visited.get(),
        messages.len(),
        "a title-only fallback must verify there is no visible message match"
    );

    assert_eq!(
        find_match_with_preview("absent", "Other", ["not a hit"]),
        None
    );
    assert_eq!(
        find_match_with_preview("  ", "", ["anything"]),
        Some((NativeSearchMatch::Title, None))
    );
}

#[test]
fn empty_query_preserves_empty_or_nonempty_native_conversations() {
    assert_eq!(find_match("", "", []), Some(NativeSearchMatch::Title));
    assert_eq!(
        find_match(" \n ", "Drafted conversation", ["anything"]),
        Some(NativeSearchMatch::Title)
    );
}

#[test]
fn searches_titles_case_insensitively_without_needing_message_text() {
    assert_eq!(
        find_match("  RUST  ", "Rust toolchain design", ["unrelated"]),
        Some(NativeSearchMatch::Title)
    );
    assert_eq!(find_match("rust", "Kotlin", ["unrelated"]), None);
}

#[test]
fn searches_only_passed_local_user_and_assistant_messages() {
    assert_eq!(
        find_match(
            "permission",
            "Archive",
            ["Hello", "Explicit PERMISSION granted"]
        ),
        Some(NativeSearchMatch::Message)
    );
    assert_eq!(find_match("permission", "Archive", ["Hello"]), None);
}

#[test]
fn unicode_query_uses_title_when_no_visible_message_matches() {
    assert_eq!(
        find_match("MÉX", "México", ["mex does not need matching"]),
        Some(NativeSearchMatch::Title)
    );
    assert_eq!(
        find_match("ñ", "Untitled", ["El niño"]),
        Some(NativeSearchMatch::Message)
    );
}
