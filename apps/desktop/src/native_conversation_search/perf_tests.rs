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
        &AuthoredUserMessage::new(owner, LocalTurnId::new(), LocalMessageId::new(), text),
    )
    .unwrap();
}

fn median_us(samples: &[u128]) -> u128 {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
}

/// Reconstruct the prior two-pass query path inside the test only. It is
/// never used by Chatarium's runtime search or copied into durable state.
fn legacy_search_row(
    query: &str,
    title: &str,
    catalog_index: usize,
    messages: &[DisplayMessage],
) -> Option<NativeSearchRow> {
    let (kind, hit) = find_match_with_position(
        query,
        title,
        messages.iter().map(|message| message.text.as_str()),
    )?;
    let (message_index, preview, matching_message_indices) = match hit {
        Some((first, preview)) => {
            let needle = query.trim().to_lowercase();
            let mut indices = vec![first];
            indices.extend(messages.iter().enumerate().skip(first + 1).filter_map(
                |(position, message)| {
                    message
                        .text
                        .to_lowercase()
                        .contains(&needle)
                        .then_some(position)
                },
            ));
            (Some(first), Some(preview), indices)
        }
        None => (None, None, Vec::new()),
    };
    // Keep the benchmark's legacy row structurally equal to the runtime
    // row, including the new sidebar summary. This intentionally retains
    // the legacy multiple-pass behavior for an apples-to-apples comparison.
    let mut authored_words = 0_usize;
    let mut authored_bytes = 0_usize;
    for message in messages {
        if matches!(message.role, DisplayRole::User | DisplayRole::Assistant) {
            authored_words += message.text.split_whitespace().count();
            authored_bytes += message.text.len();
        }
    }
    let summary = NativeRowSummary {
        message_count: messages.len(),
        authored_words,
        authored_bytes,
        latest_excerpt: messages
            .last()
            .map(|message| {
                message
                    .text
                    .split_whitespace()
                    .take(12)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|value| !value.is_empty()),
    };
    Some(NativeSearchRow {
        catalog_index,
        title: title.to_owned(),
        kind,
        preview,
        message_index,
        matching_message_indices,
        summary,
    })
}

/// Run uncached row construction on the same journal and queries. Alternating
/// old/new order guards against one method always warming the other first.
fn scan_query_profile(
    index: &NativeConversationSearchIndex,
    entries: &[LocalConversationEntry],
    queries: &[&str],
    legacy: bool,
) -> usize {
    let mut found = 0;
    for query in queries {
        let needle = query.trim().to_lowercase();
        for (position, entry) in entries.iter().enumerate() {
            let title = entry.title.as_deref().unwrap_or("");
            let messages = index.messages(entry.id);
            let row = if legacy {
                legacy_search_row(query, title, position, messages)
            } else {
                collect_native_search_row(&needle, position, title.to_owned(), messages)
            };
            found += usize::from(row.is_some());
            black_box(row);
        }
    }
    found
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

    // Repeat the same cold construction on immutable synthetic journal data.
    // The uncached builder is intentionally measured instead of a warmed
    // egui cache hit. Report the distribution, never gate on wall-clock time.
    const COLD_REPEATS: usize = 9;
    let mut cold_build_samples_us = Vec::with_capacity(COLD_REPEATS);
    for _ in 0..COLD_REPEATS {
        let start = Instant::now();
        let rebuilt = NativeConversationSearchIndex::build(store.events());
        assert_eq!(rebuilt.messages(owners[0]).len(), MESSAGES_PER_CONVERSATION);
        black_box(&rebuilt);
        cold_build_samples_us.push(start.elapsed().as_micros());
        // Exclude destruction of the previous sample's allocations from
        // the next cold-build measurement.
        drop(rebuilt);
    }

    let first_start = Instant::now();
    let rows = cached_rows(&ctx, &index, &entries, false, "needle");
    let first_query_us = first_start.elapsed().as_micros();
    assert_eq!(rows.len(), CONVERSATIONS / 4);
    assert!(
        rows.iter()
            .all(|row| row.matching_message_indices.len() == 8)
    );
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
    for term in [
        "needle",
        "nonmatching",
        "not-found",
        "archive",
        "message",
        "needle",
        "x",
    ] {
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

    // Compare both implementations inside the same runner, journal, and
    // binary. Ten alternating rounds are observational, not a timing gate.
    const COMPARISON_ROUNDS: usize = 10;
    const COMPARISON_QUERIES: [&str; 7] = [
        "needle",
        "nonmatching",
        "not-found",
        "archive",
        "message",
        "needle",
        "x",
    ];
    for query in COMPARISON_QUERIES {
        let needle = query.trim().to_lowercase();
        for (position, entry) in entries.iter().enumerate() {
            let title = entry.title.as_deref().unwrap_or("");
            assert_eq!(
                legacy_search_row(query, title, position, index.messages(entry.id)),
                collect_native_search_row(
                    &needle,
                    position,
                    title.to_owned(),
                    index.messages(entry.id),
                ),
            );
        }
    }
    let mut legacy_scan_total_us = 0;
    let mut single_pass_scan_total_us = 0;
    let mut legacy_scan_samples_us = Vec::with_capacity(COMPARISON_ROUNDS);
    let mut single_pass_scan_samples_us = Vec::with_capacity(COMPARISON_ROUNDS);
    for round in 0..COMPARISON_ROUNDS {
        for legacy in if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let start = Instant::now();
            let hits = scan_query_profile(&index, &entries, &COMPARISON_QUERIES, legacy);
            assert!(hits > 0);
            let elapsed = start.elapsed().as_micros();
            if legacy {
                legacy_scan_total_us += elapsed;
                legacy_scan_samples_us.push(elapsed);
            } else {
                single_pass_scan_total_us += elapsed;
                single_pass_scan_samples_us.push(elapsed);
            }
        }
    }

    eprintln!(
        "CHATARIUM_SEARCH_PROFILE {}",
        serde_json::json!({
            "schema": "chatarium-native-search-profile",
            "version": 1,
            "mode": if cfg!(debug_assertions) { "debug-ci-synthetic" } else { "release-ci-synthetic" },
            "profile_optimized": !cfg!(debug_assertions),
            "cold_build_repeats": COLD_REPEATS,
            "cold_build_min_us": *cold_build_samples_us.iter().min().unwrap(),
            "cold_build_median_us": median_us(&cold_build_samples_us),
            "cold_build_max_us": *cold_build_samples_us.iter().max().unwrap(),
            "conversations": CONVERSATIONS,
            "messages": CONVERSATIONS * MESSAGES_PER_CONVERSATION,
            "query_matches": rows.len(),
            "cold_index_us": cold_index_us,
            "first_query_us": first_query_us,
            "cached_queries": CACHE_ITERATIONS,
            "cached_queries_total_us": cached_queries_total_us,
            "query_edits": 7,
            "query_edits_total_us": query_edits_total_us,
            "ab_comparison_rounds": COMPARISON_ROUNDS,
            "ab_legacy_scan_total_us": legacy_scan_total_us,
            "ab_single_pass_scan_total_us": single_pass_scan_total_us,
            "ab_legacy_scan_median_us": median_us(&legacy_scan_samples_us),
            "ab_single_pass_scan_median_us": median_us(&single_pass_scan_samples_us),
            "unrelated_append_us": unrelated_append_us,
            "native_append_us": native_append_us,
            "process_rss_kib_after_journal": setup_rss_kib,
            "process_rss_kib_after_index": index_rss_kib,
            "process_rss_kib_after_rows": rows_rss_kib,
        })
    );
}
