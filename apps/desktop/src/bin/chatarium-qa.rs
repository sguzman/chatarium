//! Machine-controlled QA entry points for production Chatarium browser paths.
//!
//! This binary intentionally reuses the production account bridge provider and its typed
//! discovery parser. It emits structural evidence only; conversation bodies and titles never
//! appear in the output.

#[path = "../account_bridge.rs"]
mod account_bridge;
#[path = "../diagnostics.rs"]
mod diagnostics;

use account_bridge::{
    AccountBridgeRuntime, BrowserBridgeError, FreshTabHistoryDiscoveryObservation,
    HistoryDiscoveryObservation, HistorySurfaceCandidate,
};
use chatarium_store::remote_mirror_bootstrap::promote_discovered_live_mirror_body;
use chatarium_store::remote_mirror_queue::{
    RemoteMirrorQueueStatus, derive_remote_mirror_queue,
    record_remote_mirror_queue_capture_started, record_remote_mirror_queue_completed,
    record_remote_mirror_queue_failed, record_remote_mirror_queue_item_queued,
    record_remote_mirror_queue_rate_limited,
};
use chatarium_store::remote_mirror_snapshot_audit::replay_remote_conversation_snapshot_audit;
use chatarium_store::remote_mirror_transcript::project_remote_active_transcript;
use chatarium_store::{EventStore, JsonlEventStore};
use chatarium_protocol::conversation_list::ConversationListItem;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CACHE_SCHEMA: &str = "chatarium-remote-history-cache";
const CACHE_VERSION: u64 = 1;

fn main() {
    diagnostics::init();
    if std::env::args().nth(1).as_deref() == Some("mirror-one") {
        run_mirror_one();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("mirror-replay") {
        run_mirror_replay();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("mirror-status") {
        run_mirror_status();
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("mirror-batch") {
        run_mirror_batch();
        return;
    }
    let started = Instant::now();
    let cache_path = default_journal_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("remote-history-cache.json");
    let durable_before = load_cache(&cache_path).unwrap_or_default();

    let runtime = match AccountBridgeRuntime::start() {
        Ok(runtime) => runtime,
        Err(error) => {
            print_json(json!({
                "terminal_state": "bridge_start_failed",
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            std::process::exit(1);
        }
    };
    let mut provider = runtime.provider();

    let authentication = match provider.probe_authentication() {
        Ok(observation) => observation,
        Err(error) => {
            print_json(json!({
                "terminal_state": "authentication_probe_failed",
                "error": error.to_string(),
                "durable_catalog_before": durable_before.len(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };

    let http_authenticated = matches!(
        authentication.evidence,
        chatarium_core::authenticated_session::SessionAuthenticationEvidence::Authenticated
    );
    // The production authentication probe proves the session with the typed /backend-api/me
    // result. Account context is learned from real first-party request headers by the bounded
    // CDP discovery pass, not from this page-world probe.
    let authenticated = http_authenticated;
    if !authenticated {
        print_json(json!({
            "terminal_state": "unauthenticated",
            "authentication": "unauthenticated",
            "authentication_http_status": authentication.http_status,
            "auth_proof": auth_proof_json(&authentication.proof),
            "durable_catalog_before": durable_before.len(),
            "elapsed_ms": started.elapsed().as_millis(),
        }));
        return;
    }

    let primary = match provider.discover_history_surfaces() {
        Ok(observation) => observation,
        Err(error) => {
            print_json(json!({
                "terminal_state": "primary_discovery_failed",
                "error": error.to_string(),
                "authentication": "authenticated",
                "auth_proof": auth_proof_json(&authentication.proof),
                "durable_catalog_before": durable_before.len(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };

    let primary_items = unique_items(&primary.candidates);
    let mut recovery_required = primary_items == 0 && durable_before.is_empty();
    let mut fresh: Option<FreshTabHistoryDiscoveryObservation> = None;
    if recovery_required {
        match provider.discover_history_surfaces_fresh_tab() {
            Ok(observation) => fresh = Some(observation),
            Err(error) => {
                print_json(json!({
                    "terminal_state": "fresh_tab_recovery_failed",
                    "error": error.to_string(),
                    "authentication": "authenticated",
                    "auth_proof": auth_proof_json(&authentication.proof),
                    "primary": primary_json(&primary),
                    "durable_catalog_before": durable_before.len(),
                    "elapsed_ms": started.elapsed().as_millis(),
                }));
                return;
            }
        }
    }

    let active_candidates = fresh
        .as_ref()
        .map(|observation| observation.candidates.as_slice())
        .unwrap_or(primary.candidates.as_slice());
    let current_pass_items = unique_items(active_candidates);
    let final_catalog = merge_catalog(durable_before.clone(), active_candidates);
    let cache_write = if current_pass_items > 0 {
        match persist_cache(&cache_path, &final_catalog) {
            Ok(()) => "written",
            Err(error) => {
                print_json(json!({
                    "terminal_state": "durable_cache_write_failed",
                    "error": error,
                    "authentication": "authenticated",
                    "auth_proof": auth_proof_json(&authentication.proof),
                    "primary": primary_json(&primary),
                    "fresh_tab": fresh.as_ref().map(fresh_json),
                    "durable_catalog_before": durable_before.len(),
                    "elapsed_ms": started.elapsed().as_millis(),
                }));
                return;
            }
        }
    } else {
        "retained"
    };

    recovery_required = recovery_required && fresh.is_some();
    print_json(json!({
        "terminal_state": if current_pass_items > 0 || !final_catalog.is_empty() { "success" } else { "zero_item_first_catalog" },
        "authentication": "authenticated",
        "auth_proof": auth_proof_json(&authentication.proof),
        "primary": primary_json(&primary),
        "fresh_tab_recovery_required": primary_items == 0 && durable_before.is_empty(),
        "fresh_tab": fresh.as_ref().map(fresh_json),
        "fresh_tab_recovery_invoked": recovery_required,
        "durable_catalog_before": durable_before.len(),
        "final_observed_catalog": final_catalog.len(),
        "durable_cache": cache_write,
        "durable_cache_path": cache_path,
        "temporary_tab_cleanup": fresh.as_ref().map(|_| "production_finally_cleanup"),
        "synthetic_private_history_request": false,
        "account_wide_completeness_claimed": false,
        "elapsed_ms": started.elapsed().as_millis(),
    }));
}

fn run_mirror_one() {
    let started = Instant::now();
    let cache_path = default_journal_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("remote-history-cache.json");
    let catalog = match load_cache(&cache_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            print_json(json!({
                "terminal_state": "catalog_read_failed",
                "error": error,
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    if catalog.is_empty() {
        print_json(json!({
            "terminal_state": "empty_discovered_catalog",
            "catalog_size": 0,
            "elapsed_ms": started.elapsed().as_millis(),
        }));
        return;
    }

    // Prefer the most recently updated catalog item, with the opaque ID as a deterministic
    // tie-breaker. A machine-supplied catalog index is allowed when a prior browser metadata
    // pass has established that one catalog item is currently visible in ChatGPT's sidebar.
    let requested_index = std::env::args()
        .nth(2)
        .filter(|value| value == "--catalog-index")
        .and_then(|_| std::env::args().nth(3))
        .and_then(|value| value.parse::<usize>().ok());
    let selected_index = requested_index
        .filter(|index| *index < catalog.len())
        .unwrap_or_else(|| {
            catalog
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| {
                    item_update_sort_key(left)
                        .cmp(&item_update_sort_key(right))
                        .then_with(|| left.id.cmp(&right.id))
                })
                .map(|(index, _)| index)
                .expect("non-empty catalog has a selectable item")
        });
    let selected = &catalog[selected_index];
    let journal_path = default_journal_path();
    let mut store = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_open_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let events_before = store.events().len();
    let snapshots_before = match replay_remote_conversation_snapshot_audit(store.events()) {
        Ok(records) => records,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_replay_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "error": error,
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let already_mirrored_before = snapshots_before
        .iter()
        .any(|record| record.remote_conversation_id.as_str() == selected.id);

    let runtime = match AccountBridgeRuntime::start() {
        Ok(runtime) => runtime,
        Err(error) => {
            print_json(json!({
                "terminal_state": "bridge_start_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let mut provider = runtime.provider();
    let authentication = match provider.probe_authentication() {
        Ok(observation) => observation,
        Err(error) => {
            print_json(json!({
                "terminal_state": "authentication_probe_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let authenticated = matches!(
        authentication.evidence,
        chatarium_core::authenticated_session::SessionAuthenticationEvidence::Authenticated
    );
    if !authenticated {
        print_json(json!({
            "terminal_state": "unauthenticated",
            "catalog_size": catalog.len(),
            "selected_catalog_index": selected_index,
            "already_mirrored_before": already_mirrored_before,
            "auth_proof": auth_proof_json(&authentication.proof),
            "elapsed_ms": started.elapsed().as_millis(),
        }));
        return;
    }

    // A fresh extension worker has no per-tab account-context cache yet. Reuse the production
    // passive discovery path to establish that context from real first-party request headers
    // before the exact C02 capture path is invoked.
    let context_preflight = if authentication.proof.account_context {
        None
    } else {
        match provider.discover_history_surfaces() {
            Ok(observation) if observation.proof.account_context => Some(json!({
                "request_profile": observation.proof.request_profile,
                "debugger_attached": observation.proof.debugger_attached,
                "network_enabled": observation.proof.network_enabled,
                "reload_started": observation.proof.reload_started,
                "responses_observed": observation.proof.responses_seen,
                "account_context": observation.proof.account_context,
            })),
            Ok(observation) => {
                print_json(json!({
                    "terminal_state": "account_context_preflight_failed",
                    "catalog_size": catalog.len(),
                    "selected_catalog_index": selected_index,
                    "already_mirrored_before": already_mirrored_before,
                    "authentication": "authenticated",
                    "auth_proof": auth_proof_json(&authentication.proof),
                    "preflight_account_context": observation.proof.account_context,
                    "elapsed_ms": started.elapsed().as_millis(),
                }));
                return;
            }
            Err(error) => {
                print_json(json!({
                    "terminal_state": "account_context_preflight_failed",
                    "catalog_size": catalog.len(),
                    "selected_catalog_index": selected_index,
                    "already_mirrored_before": already_mirrored_before,
                    "authentication": "authenticated",
                    "auth_proof": auth_proof_json(&authentication.proof),
                    "error": error.to_string(),
                    "elapsed_ms": started.elapsed().as_millis(),
                }));
                return;
            }
        }
    };

    let fetched = match provider.fetch_authenticated_conversation(&selected.id) {
        Ok(observation) => observation,
        Err(error) => {
            print_json(json!({
                "terminal_state": "exact_mirror_capture_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "authentication": "authenticated",
                "auth_proof": auth_proof_json(&authentication.proof),
                "context_preflight": context_preflight,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let capture = json!({
        "source_chatgpt_tab_found": fetched.proof.chatgpt_tab_found,
        "account_context": fetched.proof.account_context,
        "capture_tab_created": fetched.proof.capture_tab_created,
        "debugger_attached": fetched.proof.debugger_attached,
        "network_enabled": fetched.proof.network_enabled,
        "navigation_started": fetched.proof.navigation_started,
        "exact_response_seen": fetched.proof.exact_response_seen,
        "http_status": fetched.http_status,
        "content_type": fetched.content_type,
        "total_responses": fetched.responses_seen,
        "exact_responses": fetched.exact_response_count,
        "rate_limited_responses": fetched.rate_limited_responses,
        "reload_count": fetched.rate_limit_reload_count,
        "body_captured": true,
        "body_read_failures": fetched.body_read_failures,
        "body_too_large": fetched.body_too_large,
        "invalid_json": fetched.invalid_json,
    });

    let promotion = match promote_discovered_live_mirror_body(
        &mut store,
        &selected.id,
        &fetched.body,
    ) {
        Ok(result) => result,
        Err(error) => {
            print_json(json!({
                "terminal_state": "durable_promotion_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "authentication": "authenticated",
                "auth_proof": auth_proof_json(&authentication.proof),
                "capture": capture,
                "json_valid": false,
                "exact_remote_id_validated": false,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let events_after = store.events().len();
    let snapshot_appended = promotion.snapshot.appended;
    let local_conversation_id = promotion.local_conversation_id;
    let snapshot_sequence = promotion.snapshot.sequence;
    drop(provider);
    drop(runtime);
    drop(store);

    let reopened = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "local_reopen_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "authentication": "authenticated",
                "auth_proof": auth_proof_json(&authentication.proof),
                "capture": capture,
                "json_valid": true,
                "exact_remote_id_validated": true,
                "snapshot_appended": snapshot_appended,
                "error": error.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let replayed = match replay_remote_conversation_snapshot_audit(reopened.events()) {
        Ok(records) => records,
        Err(error) => {
            print_json(json!({
                "terminal_state": "local_replay_failed",
                "catalog_size": catalog.len(),
                "selected_catalog_index": selected_index,
                "already_mirrored_before": already_mirrored_before,
                "authentication": "authenticated",
                "auth_proof": auth_proof_json(&authentication.proof),
                "capture": capture,
                "json_valid": true,
                "exact_remote_id_validated": true,
                "snapshot_appended": snapshot_appended,
                "error": error,
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    let replayed_record = replayed.iter().find(|record| {
        record.local_conversation_id == local_conversation_id
            && record.remote_conversation_id.as_str() == selected.id
            && record.imported_sequence == snapshot_sequence
    });
    let projection = replayed_record
        .map(|record| project_remote_active_transcript(&record.envelope))
        .transpose();
    let (projected_messages, truncated_before, replay_passed) = match projection {
        Ok(Some(projection)) => (
            projection.messages.len(),
            projection.truncated_before,
            true,
        ),
        _ => (0, false, false),
    };
    let mirror_state = if !replay_passed {
        "failed"
    } else if truncated_before {
        "partial"
    } else {
        "fully mirrored"
    };
    print_json(json!({
        "terminal_state": if replay_passed { "success" } else { "local_replay_failed" },
        "catalog_size": catalog.len(),
        "selected_catalog_index": selected_index,
        "selected_id_length": selected.id.len(),
        "already_mirrored_before": already_mirrored_before,
        "authentication": "authenticated",
        "auth_proof": auth_proof_json(&authentication.proof),
        "context_preflight": context_preflight,
        "capture": capture,
        "synthetic_private_request_used": false,
        "json_valid": true,
        "exact_remote_id_validated": true,
        "production_promotion_path_used": true,
        "local_conversation_created_or_resolved": true,
        "snapshot_committed": true,
        "snapshot_appended": snapshot_appended,
        "durable_sequence": snapshot_sequence,
        "events_before": events_before,
        "events_after": events_after,
        "projected_messages_after_reopen": projected_messages,
        "truncated_before": truncated_before,
        "mirror_state": mirror_state,
        "local_reopen_replay": replay_passed,
        "remote_http_required_for_replay": false,
        "temporary_tab_cleanup": "production_finally_cleanup",
        "debugger_cleanup": "production_finally_cleanup",
        "elapsed_ms": started.elapsed().as_millis(),
    }));
}

fn run_mirror_replay() {
    let started = Instant::now();
    let journal_path = default_journal_path();
    let store = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_open_failed",
                "remote_http_used": false,
                "error": error.to_string(),
            }));
            return;
        }
    };
    let records = match replay_remote_conversation_snapshot_audit(store.events()) {
        Ok(records) => records,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_replay_failed",
                "remote_http_used": false,
                "error": error,
            }));
            return;
        }
    };
    let Some(record) = records.last() else {
        print_json(json!({
            "terminal_state": "no_durable_snapshot",
            "remote_http_used": false,
        }));
        return;
    };
    let projection = match project_remote_active_transcript(&record.envelope) {
        Ok(projection) => projection,
        Err(error) => {
            print_json(json!({
                "terminal_state": "projection_failed",
                "remote_http_used": false,
                "snapshot_sequence": record.imported_sequence,
                "message_count": record.envelope.messages.len(),
                "error": error,
                "elapsed_ms": started.elapsed().as_millis(),
            }));
            return;
        }
    };
    print_json(json!({
        "terminal_state": "success",
        "remote_http_used": false,
        "snapshot_sequence": record.imported_sequence,
        "message_count": record.envelope.messages.len(),
        "projected_messages": projection.messages.len(),
        "truncated_before": projection.truncated_before,
        "local_reopen_replay": true,
        "elapsed_ms": started.elapsed().as_millis(),
    }));
}

fn run_mirror_status() {
    let started = Instant::now();
    let cache_path = default_journal_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("remote-history-cache.json");
    let catalog = match load_cache(&cache_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            print_json(json!({
                "terminal_state": "catalog_read_failed",
                "error": error,
            }));
            return;
        }
    };
    let store = match JsonlEventStore::open(default_journal_path()) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_open_failed",
                "error": error.to_string(),
            }));
            return;
        }
    };
    let summary = match derive_queue_summary(&catalog, store.events()) {
        Ok(summary) => summary,
        Err(error) => {
            print_json(json!({
                "terminal_state": "queue_replay_failed",
                "error": error,
            }));
            return;
        }
    };
    print_queue_status("success", &summary, None, started.elapsed().as_millis());
}

fn run_mirror_batch() {
    let started = Instant::now();
    let max_items = std::env::args()
        .skip(2)
        .collect::<Vec<_>>()
        .windows(2)
        .find(|window| window[0] == "--max")
        .and_then(|window| window[1].parse::<usize>().ok())
        .unwrap_or(3);
    if max_items == 0 || max_items > 3 {
        print_json(json!({
            "terminal_state": "invalid_batch_limit",
            "max_allowed": 3,
            "requested": max_items,
        }));
        return;
    }

    let cache_path = default_journal_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("remote-history-cache.json");
    let catalog = match load_cache(&cache_path) {
        Ok(catalog) => catalog,
        Err(error) => {
            print_json(json!({
                "terminal_state": "catalog_read_failed",
                "error": error,
            }));
            return;
        }
    };
    let journal_path = default_journal_path();
    let mut store = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "journal_open_failed",
                "error": error.to_string(),
            }));
            return;
        }
    };
    let before = match derive_queue_summary(&catalog, store.events()) {
        Ok(summary) => summary,
        Err(error) => {
            print_json(json!({
                "terminal_state": "queue_replay_failed",
                "error": error,
            }));
            return;
        }
    };
    let selected = before.eligible_items(max_items);
    if selected.is_empty() {
        print_queue_status("nothing_pending", &before, Some(max_items), started.elapsed().as_millis());
        return;
    }

    let runtime = match AccountBridgeRuntime::start() {
        Ok(runtime) => runtime,
        Err(error) => {
            print_json(json!({
                "terminal_state": "bridge_start_failed",
                "error": error.to_string(),
            }));
            return;
        }
    };
    let mut provider = runtime.provider();
    let mut attempted = Vec::new();
    let mut succeeded_full = Vec::new();
    let mut succeeded_partial = Vec::new();
    let mut rate_limited = Vec::new();
    let mut failed = Vec::new();
    let mut terminal_state = "success";
    let mut stopped_on_rate_limit = false;

    for item in selected {
        attempted.push(item.catalog_index);
        if let Err(error) = record_remote_mirror_queue_item_queued(
            &mut store,
            &item.remote_conversation_id,
            item.catalog_index,
        ) {
            terminal_state = "queue_persistence_failed";
            failed.push(item.catalog_index);
            break;
        }
        if let Err(error) = record_remote_mirror_queue_capture_started(
            &mut store,
            &item.remote_conversation_id,
        ) {
            terminal_state = "queue_persistence_failed";
            failed.push(item.catalog_index);
            let _ = error;
            break;
        }

        let authentication = match provider.probe_authentication() {
            Ok(authentication) => authentication,
            Err(error) => {
                let _ = record_remote_mirror_queue_failed(
                    &mut store,
                    &item.remote_conversation_id,
                    "transient",
                );
                failed.push(item.catalog_index);
                terminal_state = "authentication_failed";
                let _ = error;
                break;
            }
        };
        if !matches!(
            authentication.evidence,
            chatarium_core::authenticated_session::SessionAuthenticationEvidence::Authenticated
        ) {
            let _ = record_remote_mirror_queue_failed(
                &mut store,
                &item.remote_conversation_id,
                "transient",
            );
            failed.push(item.catalog_index);
            terminal_state = "unauthenticated";
            break;
        }

        let fetched = match provider.fetch_authenticated_conversation(&item.remote_conversation_id) {
            Ok(fetched) => fetched,
            Err(BrowserBridgeError::RateLimited(_)) => {
                let _ = record_remote_mirror_queue_rate_limited(
                    &mut store,
                    &item.remote_conversation_id,
                );
                rate_limited.push(item.catalog_index);
                terminal_state = "rate_limited_batch_stopped";
                stopped_on_rate_limit = true;
                break;
            }
            Err(error) => {
                let _ = record_remote_mirror_queue_failed(
                    &mut store,
                    &item.remote_conversation_id,
                    if matches!(error, BrowserBridgeError::Protocol(_) | BrowserBridgeError::UnsupportedRevision(_)) {
                        "structural"
                    } else {
                        "transient"
                    },
                );
                failed.push(item.catalog_index);
                terminal_state = "capture_failed";
                continue;
            }
        };

        let promotion = match promote_discovered_live_mirror_body(
            &mut store,
            &item.remote_conversation_id,
            &fetched.body,
        ) {
            Ok(promotion) => promotion,
            Err(error) => {
                let _ = record_remote_mirror_queue_failed(
                    &mut store,
                    &item.remote_conversation_id,
                    "structural",
                );
                failed.push(item.catalog_index);
                terminal_state = "promotion_failed";
                let _ = error;
                break;
            }
        };
        let records = match replay_remote_conversation_snapshot_audit(store.events()) {
            Ok(records) => records,
            Err(error) => {
                let _ = record_remote_mirror_queue_failed(
                    &mut store,
                    &item.remote_conversation_id,
                    "structural",
                );
                failed.push(item.catalog_index);
                terminal_state = "snapshot_replay_failed";
                let _ = error;
                break;
            }
        };
        let Some(record) = records
            .iter()
            .find(|record| record.imported_sequence == promotion.snapshot.sequence)
        else {
            let _ = record_remote_mirror_queue_failed(
                &mut store,
                &item.remote_conversation_id,
                "structural",
            );
            failed.push(item.catalog_index);
            terminal_state = "snapshot_missing_after_promotion";
            break;
        };
        let projection = match project_remote_active_transcript(&record.envelope) {
            Ok(projection) => projection,
            Err(error) => {
                let _ = record_remote_mirror_queue_failed(
                    &mut store,
                    &item.remote_conversation_id,
                    "structural",
                );
                failed.push(item.catalog_index);
                terminal_state = "projection_failed";
                let _ = error;
                break;
            }
        };
        let mirror_state = if projection.truncated_before {
            succeeded_partial.push(item.catalog_index);
            "partial"
        } else {
            succeeded_full.push(item.catalog_index);
            "fully_mirrored"
        };
        if record_remote_mirror_queue_completed(
            &mut store,
            &item.remote_conversation_id,
            mirror_state,
        )
        .is_err()
        {
            terminal_state = "queue_persistence_failed";
            break;
        }
    }
    drop(provider);
    drop(runtime);
    drop(store);

    let reopened = match JsonlEventStore::open(&journal_path) {
        Ok(store) => store,
        Err(error) => {
            print_json(json!({
                "terminal_state": "local_reopen_failed",
                "attempted": attempted,
                "error": error.to_string(),
            }));
            return;
        }
    };
    let after = match derive_queue_summary(&catalog, reopened.events()) {
        Ok(summary) => summary,
        Err(error) => {
            print_json(json!({
                "terminal_state": "queue_replay_failed_after_run",
                "attempted": attempted,
                "error": error,
            }));
            return;
        }
    };
    let batch = json!({
        "attempted": attempted,
        "succeeded_full": succeeded_full,
        "succeeded_partial": succeeded_partial,
        "rate_limited": rate_limited,
        "failed": failed,
        "stopped_on_rate_limit": stopped_on_rate_limit,
    });
    let mut output = queue_status_value(
        terminal_state,
        &after,
        Some(max_items),
        started.elapsed().as_millis(),
    )
    .as_object()
    .cloned()
    .expect("queue status is an object");
    output.insert("batch".to_owned(), batch);
    print_json(Value::Object(output));
}

fn derive_queue_summary(
    catalog: &[ConversationListItem],
    events: &[chatarium_store::EventEnvelope],
) -> Result<chatarium_store::remote_mirror_queue::RemoteMirrorQueueSummary, String> {
    let identities = catalog
        .iter()
        .enumerate()
        .map(|(index, item)| (index, item.id.clone()))
        .collect::<Vec<_>>();
    derive_remote_mirror_queue(&identities, events)
}

fn print_queue_status(
    terminal_state: &str,
    summary: &chatarium_store::remote_mirror_queue::RemoteMirrorQueueSummary,
    live_batch_max: Option<usize>,
    elapsed_ms: u128,
) {
    print_json(queue_status_value(
        terminal_state,
        summary,
        live_batch_max,
        elapsed_ms,
    ));
}

fn queue_status_value(
    terminal_state: &str,
    summary: &chatarium_store::remote_mirror_queue::RemoteMirrorQueueSummary,
    live_batch_max: Option<usize>,
    elapsed_ms: u128,
) -> Value {
    let item_states = summary
        .items
        .iter()
        .map(|item| {
            json!({
                "catalog_index": item.catalog_index,
                "status": queue_status_name(item.status),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "terminal_state": terminal_state,
        "catalog_count": summary.items.len(),
        "already_mirrored_count": summary.full_count() + summary.partial_count(),
        "full_count": summary.full_count(),
        "partial_count": summary.partial_count(),
        "pending_count": summary.pending_count(),
        "queued_count": summary.count(RemoteMirrorQueueStatus::Queued),
        "capturing_count": summary.count(RemoteMirrorQueueStatus::Capturing),
        "rate_limited_count": summary.count(RemoteMirrorQueueStatus::RateLimited),
        "transient_failure_count": summary.count(RemoteMirrorQueueStatus::TransientFailure),
        "structural_failure_count": summary.count(RemoteMirrorQueueStatus::StructuralFailure),
        "remaining_count": summary.pending_count(),
        "live_batch_max": live_batch_max,
        "item_states": item_states,
        "elapsed_ms": elapsed_ms,
    })
}

fn queue_status_name(status: RemoteMirrorQueueStatus) -> &'static str {
    match status {
        RemoteMirrorQueueStatus::Discovered => "discovered",
        RemoteMirrorQueueStatus::Queued => "queued",
        RemoteMirrorQueueStatus::Capturing => "capturing",
        RemoteMirrorQueueStatus::MirroredFully => "mirrored_fully",
        RemoteMirrorQueueStatus::MirroredPartial => "mirrored_partial",
        RemoteMirrorQueueStatus::RateLimited => "rate_limited",
        RemoteMirrorQueueStatus::TransientFailure => "transient_failure",
        RemoteMirrorQueueStatus::StructuralFailure => "structural_failure",
    }
}

fn item_update_sort_key(item: &ConversationListItem) -> String {
    match item.update_time.as_ref() {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn auth_proof_json(proof: &account_bridge::BrowserProof) -> Value {
    json!({
        "extension_version": proof.extension_version,
        "desktop_roundtrip": proof.desktop_roundtrip,
        "chatgpt_tab_found": proof.chatgpt_tab_found,
        "main_world_execution": proof.main_world_execution,
        "debugger_attached": proof.debugger_attached,
        "network_enabled": proof.network_enabled,
        "account_context": proof.account_context,
        "request_profile": proof.request_profile,
        "first_party_http_status": proof.first_party_http_status,
    })
}

fn primary_json(observation: &HistoryDiscoveryObservation) -> Value {
    json!({
        "discovery": observation.discovery,
        "extension_version": observation.proof.extension_version,
        "request_profile": observation.proof.request_profile,
        "chatgpt_tab_found": observation.proof.chatgpt_tab_found,
        "debugger_attached": observation.proof.debugger_attached,
        "network_enabled": observation.proof.network_enabled,
        "reload_started": observation.proof.reload_started,
        "account_context": observation.proof.account_context,
        "responses_observed": observation.proof.responses_seen,
        "backend_200_observed": observation.proof.backend_http_200_seen,
        "json_candidates_observed": observation.proof.json_candidates_seen,
        "candidate_surfaces": observation.candidates.len(),
        "conversation_summaries_observed": unique_items(&observation.candidates),
        "body_read_failures": observation.proof.body_read_failures,
        "oversized_failures": observation.proof.body_too_large,
        "invalid_json": observation.proof.invalid_json,
    })
}

fn fresh_json(observation: &FreshTabHistoryDiscoveryObservation) -> Value {
    json!({
        "discovery": observation.discovery,
        "extension_version": observation.proof.extension_version,
        "request_profile": observation.proof.request_profile,
        "chatgpt_tab_found": observation.proof.chatgpt_tab_found,
        "capture_tab_created": observation.proof.capture_tab_created,
        "debugger_attached": observation.proof.debugger_attached,
        "network_enabled": observation.proof.network_enabled,
        "navigation_started": observation.proof.navigation_started,
        "account_context": observation.proof.account_context,
        "responses_observed": observation.proof.responses_seen,
        "backend_200_observed": observation.proof.backend_http_200_seen,
        "json_candidates_observed": observation.proof.json_candidates_seen,
        "candidate_surfaces": observation.candidates.len(),
        "conversation_summaries_observed": unique_items(&observation.candidates),
        "body_read_failures": observation.proof.body_read_failures,
        "oversized_failures": observation.proof.body_too_large,
        "invalid_json": observation.proof.invalid_json,
    })
}

fn unique_items(candidates: &[HistorySurfaceCandidate]) -> usize {
    candidates
        .iter()
        .flat_map(|candidate| candidate.items.iter().map(|item| item.id.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn merge_catalog(
    existing: Vec<ConversationListItem>,
    candidates: &[HistorySurfaceCandidate],
) -> Vec<ConversationListItem> {
    let mut catalog = existing
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        for item in &candidate.items {
            catalog.entry(item.id.clone()).or_insert_with(|| item.clone());
        }
    }
    catalog.into_values().collect()
}

fn load_cache(path: &Path) -> Result<Vec<ConversationListItem>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    if value.get("schema").and_then(Value::as_str) != Some(CACHE_SCHEMA)
        || value.get("version").and_then(Value::as_u64) != Some(CACHE_VERSION)
    {
        return Err("unsupported remote history cache schema".to_owned());
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "remote history cache is missing items".to_owned())?;
    let mut catalog = BTreeMap::new();
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| "remote history cache item is not an object".to_owned())?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "remote history cache item has invalid id".to_owned())?
            .to_owned();
        let optional = |field: &str| match object.get(field) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value.clone()),
        };
        catalog.entry(id.clone()).or_insert(ConversationListItem {
            id,
            title: object.get("title").and_then(Value::as_str).map(str::to_owned),
            create_time: optional("create_time"),
            update_time: optional("update_time"),
        });
    }
    Ok(catalog.into_values().collect())
}

fn persist_cache(path: &Path, items: &[ConversationListItem]) -> Result<(), String> {
    let serialized_items = items
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "title": item.title,
                "create_time": item.create_time,
                "update_time": item.update_time,
            })
        })
        .collect::<Vec<_>>();
    let body = json!({"schema": CACHE_SCHEMA, "version": CACHE_VERSION, "items": serialized_items});
    let bytes = serde_json::to_vec_pretty(&body).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn default_journal_path() -> PathBuf {
    if let Some(override_dir) = std::env::var_os("CHATARIUM_DATA_DIR") {
        return PathBuf::from(override_dir).join("journal.jsonl");
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Chatarium")
            .join("journal.jsonl");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".chatarium")
        .join("journal.jsonl")
}

fn print_json(value: Value) {
    println!("{}", serde_json::to_string_pretty(&value).expect("JSON encoding cannot fail"));
}
