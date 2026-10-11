//! Disposable native egui screenshot QA. Never opens user journals,
//! sessions, account profiles, or normal Chatarium persistence.
use super::{DisplayMessage, DisplayRole, draw_reader_bubble, virtual_reader::VirtualReaderLayout};
use eframe::egui;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const COUNT: usize = 4_096;
const SEARCH_TARGET: usize = 3_017;

struct ReaderQa {
    source: Arc<Vec<DisplayMessage>>,
    layout: VirtualReaderLayout,
    output_dir: PathBuf,
    phase: &'static str,
    jump_to: Option<f32>,
    frame: u64,
}

fn fixture() -> Arc<Vec<DisplayMessage>> {
    Arc::new(
        (0..COUNT)
            .map(|i| {
                let text = if i == SEARCH_TARGET {
                    "ARCHIVE NEEDLE -- unique synthetic search target.\n\n\
                    This text marks the deterministic long-distance search jump."
                        .to_owned()
                } else {
                    match i % 7 {
                        0 => "# Synthetic heading\n\n- One\n- Two\n\nA normal paragraph".to_owned(),
                        1 => "Short user turn".to_owned(),
                        2 => "This line wraps to multiple display rows. ".repeat(16),
                        3 => "Español 日本語 العربية 🪐\nSecond line".repeat(3),
                        4 => "first\nsecond\nthird\nfourth".to_owned(),
                        5 => "code:\nfn main() { println!(\"synthetic\"); }".to_owned(),
                        _ => "An ordinary, locally generated Chatarium message".to_owned(),
                    }
                };
                DisplayMessage {
                    role: if i.is_multiple_of(2) {
                        DisplayRole::User
                    } else {
                        DisplayRole::Assistant
                    },
                    text,
                    sequence: i as u64 + 1,
                    timestamp: None,
                    provenance_label: Some("SYNTHETIC QA".to_owned()),
                }
            })
            .collect(),
    )
}

impl ReaderQa {
    fn new(output_dir: PathBuf) -> Self {
        Self {
            source: fixture(),
            layout: VirtualReaderLayout::default(),
            output_dir,
            phase: "top",
            jump_to: Some(0.0),
            frame: 0,
        }
    }

    fn write_status(&self, scroll_offset: f32, panel_width: f32, rendered: &[usize]) {
        let status = json!({
            "schema": "chatarium-native-visual-qa",
            "version": 1,
            "mode": "synthetic-continuous-reader",
            "source_count": self.source.len(),
            "phase": self.phase,
            "frame": self.frame,
            "viewport_width": panel_width,
            "scroll_offset": scroll_offset,
            "total_estimated_height": self.layout.total_height(),
            "rendered_count": rendered.len(),
            "render_start": rendered.first(),
            "render_end": rendered.last(),
            "search_target": SEARCH_TARGET,
            "rendered_search_hit": rendered.contains(&SEARCH_TARGET),
            "persistent_user_journal_access": false,
            "remote_network_access": false
        });
        let temp = self.output_dir.join("reader-state.tmp");
        let target = self.output_dir.join("reader-state.json");
        if std::fs::write(&temp, status.to_string()).is_ok() {
            let _ = std::fs::rename(&temp, &target);
        }
    }
}

impl eframe::App for ReaderQa {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame += 1;
        let (top, middle, bottom, search) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::F1),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F2),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F3),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F4),
            )
        });
        let mut panel_width = 0.0;
        let mut rendered = Vec::new();
        let mut scroll_offset = 0.0;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                panel_width = ui.available_width();
                self.layout
                    .sync(Some(Arc::clone(&self.source)), panel_width);
                if top {
                    self.phase = "top";
                    self.jump_to = Some(0.0);
                }
                if middle {
                    self.phase = "middle";
                    self.jump_to = Some(self.layout.total_height() * 0.5);
                }
                if bottom {
                    self.phase = "bottom";
                    self.jump_to = Some(self.layout.total_height());
                }
                if search {
                    self.phase = "search";
                    self.jump_to = Some((self.layout.offset(SEARCH_TARGET) - 100.0).max(0.0));
                }
                ui.label(
                    egui::RichText::new("CHATARIUM / NATIVE READER QA / SYNTHETIC")
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::from_rgb(185, 215, 243)),
                );
                ui.label("F1 Top   ·   F2 Middle   ·   F3 End   ·   F4 Find ARCHIVE NEEDLE");
                ui.label(format!(
                    "Mode: continuous  |  Phase: {}  |  Messages: {COUNT}",
                    self.phase
                ));
                ui.separator();

                let mut scroller = egui::ScrollArea::vertical()
                    .id_salt("qa-native-virtual-transcript")
                    .auto_shrink([false, false])
                    .stick_to_bottom(false);
                if let Some(jump) = self.jump_to.take() {
                    scroller = scroller.vertical_scroll_offset(jump);
                }
                let out = scroller.show_viewport(ui, |ui, viewport| {
                    self.layout
                        .sync(Some(Arc::clone(&self.source)), ui.available_width());
                    let list_top = ui.cursor().top();
                    let header_height = list_top - ui.max_rect().top();
                    let range = self.layout.visible_range(
                        viewport.min.y - header_height,
                        viewport.max.y - header_height,
                    );
                    ui.set_min_height(header_height + self.layout.total_height());
                    if range.is_empty() {
                        return;
                    }
                    let top = list_top + self.layout.offset(range.start);
                    let bottom = list_top + self.layout.offset(range.end);
                    let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=bottom);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |inner| {
                        for index in range {
                            inner.push_id(("qa-native-message", index), |row| {
                                let before = row.cursor().top();
                                draw_reader_bubble(
                                    row,
                                    &self.source[index],
                                    if self.phase == "search" {
                                        "ARCHIVE NEEDLE"
                                    } else {
                                        ""
                                    },
                                    index == SEARCH_TARGET && self.phase == "search",
                                    ctx,
                                );
                                row.add_space(12.0);
                                self.layout.measure(index, row.cursor().top() - before);
                                rendered.push(index);
                            });
                        }
                    });
                    if self.layout.finish_measurements() {
                        ctx.request_repaint();
                    }
                });
                scroll_offset = out.state.offset.y;
            });
        self.write_status(scroll_offset, panel_width, &rendered);
        ctx.request_repaint_after(Duration::from_millis(160));
    }
}

pub fn run() -> eframe::Result<()> {
    let output_dir = PathBuf::from(
        std::env::var_os("CHATARIUM_READER_QA_DIR")
            .expect("--qa-reader requires CHATARIUM_READER_QA_DIR (disposable output only)"),
    );
    std::fs::create_dir_all(&output_dir).expect("create disposable reader QA output");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_180.0, 800.0])
            .with_min_inner_size([860.0, 560.0])
            .with_transparent(false),
        ..Default::default()
    };
    eframe::run_native(
        "Chatarium Reader QA",
        options,
        Box::new(move |cc| {
            super::configure_ui(&cc.egui_ctx);
            Ok(Box::new(ReaderQa::new(output_dir.clone())))
        }),
    )
}
