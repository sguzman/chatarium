//! admissions controls extracted from the advanced sidebar.
//! Same closures and action order; no change to egui widget salts.
use super::*;

impl ChatariumApp {
    pub(super) fn render_admitted_mcp_evidence(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Admitted MCP evidence · current conversation", |ui| {
            // Use the exact verified admission replay consumed by the
            // normal request composer, not the bounded provider review.
            match replay_admitted_tool_results(
                &self.events,
                self.local_conversation_id,
            ) {
                Ok(admitted) => match replay_tool_call_audit(&self.events) {
                    Ok(calls) => {
                        match mcp_context_inventory::verified_inventory(
                            &admitted,
                            &calls,
                            self.local_conversation_id,
                        ) {
                            Ok(rows) => {
                                let can_revoke = !self.tool_command_pending
                                    && self.persist_tx.is_some();
                                if let Some(call_id) =
                                    mcp_context_inventory::render_inventory(
                                        ui,
                                        &rows,
                                        can_revoke,
                                    )
                                {
                                    self.decide_tool_result_context(
                                        call_id,
                                        ToolResultContextDecision::Exclude,
                                    );
                                }
                            }
                            Err(error) => {
                                ui.label(format!(
                                    "Admitted evidence inventory blocked by inconsistent identities: {error}"
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        ui.label(format!(
                            "Admitted evidence inventory blocked by tool call audit: {error}"
                        ));
                    }
                },
                Err(error) => {
                    ui.label(format!(
                        "Admitted evidence inventory blocked by admission audit: {error}"
                    ));
                }
            }
        });
    }
}
