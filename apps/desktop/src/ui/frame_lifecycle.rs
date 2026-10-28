//! Lightweight frame polling; maintains the pre-existing ordered update sequence.
use super::*;

impl ChatariumApp {
    pub(super) fn poll_frame_updates(&mut self, ctx: &egui::Context) {
        self.process_notices();
        self.poll_archive_operation(ctx);
        self.network_health.poll(ctx);
        // Only rate-limit queues auto-release; human approval and manual
        // queues remain frozen until explicitly decided.
        if self.coordination_last_queue_check.elapsed() >= Duration::from_secs(2) {
            self.coordination_last_queue_check = Instant::now();
            let before = self.coordination_workspace.clone();
            if self
                .coordination_workspace
                .advance_rate_queues(unix_now_ms())
                > 0
            {
                if let Err(error) = self
                    .coordination_workspace
                    .save_atomic(&self.coordination_store_path())
                {
                    self.coordination_workspace = before;
                    self.status = format!("coordination queue delivery not saved: {error}");
                }
            }
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        if ACCOUNT_MIRRORING_ENABLED {
            self.process_mirror_controller_notices();
            self.process_live_mirror_fetch_notices();
        }
        self.process_remote_notices();
        self.process_due_delivery_retry();
        self.maybe_wake_open_coordination_worker();
        self.maybe_dispatch_ready_openviking_write();
        self.maybe_generate_openviking_summary();
        self.maybe_retrieve_openviking_context();
        self.maybe_retrieve_project_openviking_context();
    }
}
