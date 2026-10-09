//! Opt-in measurements over synthetic, typed native local conversations.
//! These numbers describe one CI runner and intentionally set no SLO.

use super::super::local_conversations::LocalConversationCatalog;
use super::*;
use chatarium_core::{AuthoredUserMessage, LocalMessageId, LocalTurnId};
use chatarium_store::{EventStore, MemoryEventStore};
use std::hint::black_box;
use std::time::Instant;

fn author(store: &mut impl EventStore, owner: LocalConversationId, text: String) {
    chatarium_store::authored::commit_user_message(
        store,
        &AuthoredUserMessage::new(
            owner,
            LocalTurnId::new(),
            LocalMessageId::new(),
            text,
        ),
    )
    .unwrap();
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
}

#[test]
#[ignore = "opt-in, non-gating synthetic search performance profile"]
fn native_search_perf_profile() {
    const CONVERSATIONS: usize = 128;
    const MESSAGES_PER_CONVERSATION: usize = 128;
    const CACHE_ITERATIONS: usize = 100;

    let mut store = MemoryEventStore::default();
    let mut catalog = LocalConversationCatalog::default();
    let mut owners = Vec::with_capacity(CONVERSATIONS);
    for conversation in 0..CONVERSATIONS {
        let owner = LocalConversationId::new();
        owners.push(owner);
        catalog.create(owner, (conversation + 1) as u64);
        catalog
            .rename(
                owner,
                Some(format!("Synthetic archive {conversation:03}")),
                (conversation + 1) as u64,
            )
            .unwrap();
        for position in 0..MESSAGES_PER_CONVERSATION {
            let mark = if position % 16 == 0 && conversation % 4 == 0 {
                "needle"
            } else {
                "nonmatching"
            };
            author(
                &mut store,
                owner,
                format!(
                    "conversation {conversation:03} message {position:03} {mark} {}",
                    "x".repeat(100)
                ),
            );
        }
    }

    let setup_rss_kib = rss_kib();
    let entries = catalog.entries();
    let ctx = egui::Context::default();

    let cold_start = Instant::now();
    let index = cached_index(&ctx, store.events());
    let cold_index_us = cold_start.elapsed().as_micros();
    black_box(index.messages(owners[0]).len());
    let index_rss_kib = rss_kib();

    let first_start = Instant::now();
    let rows = cached_rows(&ctx, &index, &entries, false, "needle");
    let first_query_us = first_start.elapsed().as_micros();
    assert_eq!(rows.len(), CONVERSATIONS / 4);
    assert!(rows.iter().all(|row| row.matching_message_indices.len() == 8));
    black_box(rows.len());
    let rows_rss_kib = rss_kib();

    let warm_start = Instant::now();
    for _ in 0..CACHE_ITERATIONS {
        let repeated = cached_rows(&ctx, &index, &entries, false, "needle");
        assert!(Arc::ptr_eq(&rows, &repeated));
        black_box(repeated.len());
    }
    let cached_queries_total_us = warm_start.elapsed().as_micros();

    let churn_start = Instant::now();
    for term in ["needle", "nonmatching", "not-found", "archive", "message", "needle", "x"] {
        let matches = cached_rows(&ctx, &index, &entries, false, term);
        black_box(matches.len());
    }
    let query_edits_total_us = churn_start.elapsed().as_micros();

    let draft_start = Instant::now();
    store
        .append_scoped(None, EventKind::DraftChanged, "synthetic draft".to_owned())
        .unwrap();
    let unchanged = cached_index(&ctx, store.events());
    assert!(Arc::ptr_eq(&index, &unchanged));
    let unrelated_append_us = draft_start.elapsed().as_micros();

    let native_start = Instant::now();
    author(&mut store, owners[0], "needle newly appended".to_owned());
    let extended = cached_index(&ctx, store.events());
    assert!(!Arc::ptr_eq(&index, &extended));
    assert!(Arc::ptr_eq(
        index.messages.get(&owners[1]).unwrap(),
        extended.messages.get(&owners[1]).unwrap(),
    ));
    let native_append_us = native_start.elapsed().as_micros();

    eprintln!(
        "CHATARIUM_SEARCH_PROFILE {}",
        serde_json::json!({
            "schema": "chatarium-native-search-profile",
            "version": 1,
            "mode": "debug-ci-synthetic",
            "conversations": CONVERSATIONS,
            "messages": CONVERSATIONS * MESSAGES_PER_CONVERSATION,
            "query_matches": rows.len(),
            "cold_index_us": cold_index_us,
            "first_query_us": first_query_us,
            "cached_queries": CACHE_ITERATIONS,
            "cached_queries_total_us": cached_queries_total_us,
            "query_edits": 7,
            "query_edits_total_us": query_edits_total_us,
            "unrelated_append_us": unrelated_append_us,
            "native_append_us": native_append_us,
            "process_rss_kib_after_journal": setup_rss_kib,
            "process_rss_kib_after_index": index_rss_kib,
            "process_rss_kib_after_rows": rows_rss_kib,
        })
    );
}
