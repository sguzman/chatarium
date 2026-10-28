//! Desktop history helpers mechanically relocated from the original crate root.
use super::super::*;

pub(crate) fn should_run_fresh_tab_history_recovery(
    primary_current_pass_items: usize,
    durable_catalog_items: usize,
) -> bool {
    primary_current_pass_items == 0 && durable_catalog_items == 0
}

pub(crate) fn history_observed_label(observed: usize, proven_total: Option<u64>) -> String {
    match proven_total {
        Some(total) => format!("CHATGPT HISTORY · {observed}/{total}"),
        None => format!("CHATGPT HISTORY · {observed} OBSERVED"),
    }
}

pub(crate) fn remote_history_entry_state_label(
    mirrored: bool,
    partial: bool,
    imported: bool,
    pending: bool,
    rate_limited: bool,
    failed: bool,
) -> &'static str {
    if mirrored {
        if partial {
            "remote · mirrored locally · partial"
        } else {
            "remote · fully mirrored locally"
        }
    } else if imported {
        "remote · historical backup available"
    } else if pending {
        "remote · mirroring…"
    } else if rate_limited {
        "remote · rate limited · cooling down"
    } else if failed {
        "remote · mirror failed · click to retry"
    } else {
        "remote · discovered · click to mirror"
    }
}

pub(crate) fn status_row(ui: &mut egui::Ui, label: &str, value: &str, healthy: bool) {
    ui.horizontal(|ui| {
        let dot = if healthy {
            egui::Color32::from_rgb(102, 190, 132)
        } else {
            egui::Color32::from_rgb(153, 157, 166)
        };
        ui.colored_label(dot, "●");
        ui.label(
            egui::RichText::new(label)
                .size(11.0)
                .color(egui::Color32::from_rgb(186, 189, 197)),
        );
    });
    ui.add(
        egui::Label::new(
            egui::RichText::new(value)
                .size(10.0)
                .color(egui::Color32::from_rgb(126, 130, 139)),
        )
        .wrap(),
    );
    ui.add_space(5.0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryListSemanticVerdict {
    Valid,
    UnconfirmedZero,
    Contradiction,
}

pub(crate) fn classify_history_list_semantics(
    remote_total: u64,
    local_history_exists: bool,
) -> HistoryListSemanticVerdict {
    if remote_total > 0 {
        HistoryListSemanticVerdict::Valid
    } else if local_history_exists {
        HistoryListSemanticVerdict::Contradiction
    } else {
        HistoryListSemanticVerdict::UnconfirmedZero
    }
}

pub(crate) fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub(crate) fn browser_proof_label(proof: &account_bridge::BrowserProof) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · MAIN={} · debugger={} · network={} · capture-tab={} · navigation={} · exact-response={} · account-context={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.main_world_execution),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.capture_tab_created),
        yes_no(proof.navigation_started),
        yes_no(proof.exact_response_seen),
        yes_no(proof.account_context),
        proof.request_profile,
    )
}

pub(crate) fn history_discovery_proof_label(proof: &account_bridge::HistoryDiscoveryProof) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · debugger={} · network={} · auto-reload={} · account-context={} · responses={} · backend-200={} · json-candidates={} · body-read-failures={} · body-too-large={} · invalid-json={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.reload_started),
        yes_no(proof.account_context),
        proof.responses_seen,
        proof.backend_http_200_seen,
        proof.json_candidates_seen,
        proof.body_read_failures,
        proof.body_too_large,
        proof.invalid_json,
        proof.request_profile,
    )
}

pub(crate) fn fresh_tab_history_discovery_proof_label(
    proof: &account_bridge::FreshTabHistoryDiscoveryProof,
) -> String {
    format!(
        "extension={} · roundtrip={} · tab={} · temp-tab={} · debugger={} · network={} · navigate={} · account-context={} · responses={} · backend-200={} · json-candidates={} · body-read-failures={} · body-too-large={} · invalid-json={} · profile={}",
        proof.extension_version,
        yes_no(proof.desktop_roundtrip),
        yes_no(proof.chatgpt_tab_found),
        yes_no(proof.capture_tab_created),
        yes_no(proof.debugger_attached),
        yes_no(proof.network_enabled),
        yes_no(proof.navigation_started),
        yes_no(proof.account_context),
        proof.responses_seen,
        proof.backend_http_200_seen,
        proof.json_candidates_seen,
        proof.body_read_failures,
        proof.body_too_large,
        proof.invalid_json,
        proof.request_profile,
    )
}

pub(crate) fn history_probe_failure_status(error: &account_bridge::BrowserBridgeError) -> String {
    match error {
        account_bridge::BrowserBridgeError::Timeout => {
            "listener ready · Edge extension did not complete a typed roundtrip before timeout"
                .to_owned()
        }
        _ => format!("listener ready · extension proof failed: {error}"),
    }
}
