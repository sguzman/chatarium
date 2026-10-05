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
    AccountBridgeRuntime, FreshTabHistoryDiscoveryObservation,
    HistoryDiscoveryObservation, HistorySurfaceCandidate,
};
use chatarium_protocol::conversation_list::ConversationListItem;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CACHE_SCHEMA: &str = "chatarium-remote-history-cache";
const CACHE_VERSION: u64 = 1;

fn main() {
    diagnostics::init();
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
