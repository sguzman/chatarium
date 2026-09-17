use chatarium_core::TurnEvidence;
use eframe::egui;

#[derive(Default)]
struct ChatariumApp {
    draft: String,
    evidence: TurnEvidence,
}

impl eframe::App for ChatariumApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Chatarium");
                ui.separator();
                ui.label("bootstrap / P0 reliability");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Local-first conversation surface");
            ui.label("The native shell is intentionally not protocol-active until an observed baseline exists.");
            ui.add_space(12.0);
            ui.label("Draft (not yet durably wired):");
            ui.add(egui::TextEdit::multiline(&mut self.draft).desired_rows(10));
            ui.add_space(12.0);
            ui.monospace(format!("turn evidence: {:?}", self.evidence));
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Chatarium",
        options,
        Box::new(|_creation_context| Ok(Box::<ChatariumApp>::default())),
    )
}
