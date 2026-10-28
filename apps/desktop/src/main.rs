mod account_bridge;
mod app;
mod prelude;
pub(crate) use app::bootstrap::*;
pub(crate) use app::checkpoint::*;
pub(crate) use app::controller::*;
pub(crate) use app::dispatch::*;
pub(crate) use app::history::*;
pub(crate) use app::model::*;
pub(crate) use app::projection::*;
pub(crate) use app::recovery::*;
pub(crate) use app::storage::*;
pub(crate) use prelude::*;
mod journal_ops;
mod ui_helpers;
pub(crate) use journal_ops::*;
pub(crate) use ui_helpers::*;
mod startup;
#[cfg(test)]
mod tests;
mod ui;
mod worker;
use worker::persistence_worker;
mod attachment_intake;
mod behavior_profile;
mod capability_probes;
mod context_composer;
mod conversation_keybindings;
mod conversation_navigation;
mod coordination_tool_budget;
mod coordination_wake_scheduler;
mod coordination_workspace;
mod delivery_telemetry;
mod diagnostics;
mod display_preferences;
mod host_shell_mcp;
#[cfg(test)]
mod integration_journey;
mod local_archive_search;
mod local_conversations;
mod local_http_mcp;
mod local_inference_contract;
mod local_inference_settings;
mod local_stdio_host_diagnostics;
mod openviking_checkpoint_outbox;
mod openviking_memory_delivery;
mod openviking_memory_summary;
mod openviking_project_cache;
mod openviking_project_ui;
mod openviking_write_preflight;

mod local_stdio_runner;
mod local_tool_adapter;
mod local_tool_provider_control;
mod markdown_inline;
mod mcp_agent_defaults;
#[cfg(test)]
mod mcp_async_tests;
mod mcp_call_review;
mod mcp_context_inventory;
mod mcp_dispatch_manifest;
mod mcp_output_audit;
mod mcp_provider_workflow;
mod mcp_result_review;
mod mcp_transcript;
mod native_conversation_search;
mod native_transcript_export;
mod network_health;
mod offline_reader;
mod reader_visual_qa;
mod response_text_provenance;
mod send_admission_gate;
mod siwc_activity_transcript;
mod siwc_bridge;
mod system_fonts;
mod timestamp_display;
mod virtual_reader;

// History mirroring is parked. Local archives remain readable and the code is retained.
// This never disables the separate ChatGPT inference/sign-in runtime.
const ACCOUNT_MIRRORING_ENABLED: bool = false;

fn main() -> eframe::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--qa-reader") {
        return reader_visual_qa::run();
    }
    diagnostics::init();
    if std::env::args().nth(1).as_deref() == Some("--production-mirror-acceptance") {
        run_production_mirror_acceptance();
        return Ok(());
    }
    diagnostics::info("app", "starting Chatarium desktop");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0])
            .with_transparent(false),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Chatarium",
        options,
        Box::new(|creation_context| {
            configure_ui(&creation_context.egui_ctx);
            Ok(Box::new(startup::BootstrapApp::new()))
        }),
    );
    match &result {
        Ok(()) => diagnostics::info("app", "desktop event loop exited cleanly"),
        Err(error) => diagnostics::error("app", format!("desktop event loop failed: {error}")),
    }
    result
}
