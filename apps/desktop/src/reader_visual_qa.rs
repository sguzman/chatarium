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
const APPEND_TEXT: &str = "APPENDED SYNTHETIC USER MESSAGE";

struct ReaderQa {
    source: Arc<Vec<DisplayMessage>>,
    primary: Arc<Vec<DisplayMessage>>,
    alternate: Arc<Vec<DisplayMessage>>,
    conversation: &'static str,
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
                    This text marks the deterministic long-distance search jump.\n\n                    日本語 / العربية / עברית"
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
        let primary = fixture();
        let alternate = Arc::new(
            (0_usize..256)
                .map(|index| DisplayMessage {
                    role: if index.is_multiple_of(2) {
                        DisplayRole::User
                    } else {
                        DisplayRole::Assistant
                    },
                    text: format!("ALTERNATE SYNTHETIC CONVERSATION, distinct row {index}"),
                    sequence: 100_001 + index as u64,
                    timestamp: None,
                    provenance_label: Some("ALTERNATE SYNTHETIC QA".to_owned()),
                })
                .collect::<Vec<_>>(),
        );
        Self {
            source: Arc::clone(&primary),
            primary,
            alternate,
            conversation: "primary",
            layout: VirtualReaderLayout::default(),
            output_dir,
            phase: "top",
            jump_to: Some(0.0),
            frame: 0,
        }
    }

    fn write_status(
        &self,
        ctx: &egui::Context,
        scroll_offset: f32,
        panel_width: f32,
        rendered: &[usize],
        visible: &[usize],
        appended_bubble_visible_height: f32,
        search_bubble_visible_height: f32,
    ) {
        let status = json!({
            "schema": "chatarium-native-visual-qa",
            "version": 1,
            "mode": "synthetic-continuous-reader",
            "source_count": self.source.len(),
            "conversation": self.conversation,
            "phase": self.phase,
            "last_sequence": self.source.last().map(|item| item.sequence),
            "appended_message_visible": self
                .source
                .last()
                .is_some_and(|message| message.text == APPEND_TEXT)
                && appended_bubble_visible_height >= 48.0,
            "appended_bubble_visible_height": appended_bubble_visible_height,
            "visible_count": visible.len(),
            "visible_start": visible.first(),
            "visible_end": visible.last(),
            "frame": self.frame,
            "viewport_width": panel_width,
            "scroll_offset": scroll_offset,
            "total_estimated_height": self.layout.total_height(),
            "rendered_count": rendered.len(),
            "render_start": rendered.first(),
            "render_end": rendered.last(),
            "search_target": SEARCH_TARGET,
            "font_fallbacks": super::system_fonts::loaded_names(ctx),
            "rendered_search_hit": search_bubble_visible_height >= 48.0,
            "search_bubble_visible_height": search_bubble_visible_height,
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
        let (top, middle, bottom, search, append, away, back) = ctx.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::F1),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F2),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F3),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F4),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F5),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F6),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F7),
            )
        });
        let mut panel_width = 0.0;
        let mut rendered = Vec::new();
        let mut visible = Vec::new();
        let mut appended_bubble_visible_height = 0.0;
        let mut search_bubble_visible_height = 0.0;
        let mut scroll_offset = 0.0;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 24, 29))
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| {
                panel_width = ui.available_width();
                if append {
                    let messages = Arc::make_mut(&mut self.primary);
                    messages.push(DisplayMessage {
                        role: DisplayRole::User,
                        text: APPEND_TEXT.to_owned(),
                        sequence: messages.len() as u64 + 1,
                        timestamp: None,
                        provenance_label: Some("SYNTHETIC QA".to_owned()),
                    });
                    self.source = Arc::clone(&self.primary);
                    self.conversation = "primary";
                    self.phase = "append";
                    self.layout = VirtualReaderLayout::default();
                    self.layout
                        .sync(Some(Arc::clone(&self.source)), panel_width);
                    self.jump_to = Some(self.layout.total_height());
                }
                if away {
                    self.source = Arc::clone(&self.alternate);
                    self.conversation = "alternate";
                    self.phase = "alternate";
                    self.layout = VirtualReaderLayout::default();
                    self.jump_to = Some(0.0);
                }
                if back {
                    self.source = Arc::clone(&self.primary);
                    self.conversation = "primary";
                    self.phase = "return";
                    self.layout = VirtualReaderLayout::default();
                    self.layout
                        .sync(Some(Arc::clone(&self.source)), panel_width);
                    self.jump_to = Some(self.layout.total_height());
                }
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
                }
                // Use the same virtual-layout search state as the production
                // reader. A width change invalidates measured rows and must
                // reveal the selected hit again without another F4 press.
                if self.phase == "search" {
                    if let Some(offset) = self
                        .layout
                        .new_search_offset("ARCHIVE NEEDLE", Some(SEARCH_TARGET))
                    {
                        self.jump_to = Some((offset - 100.0).max(0.0));
                    }
                }
                ui.label(
                    egui::RichText::new("CHATARIUM / NATIVE READER QA / SYNTHETIC")
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::from_rgb(185, 215, 243)),
                );
                ui.label("F1 Top · F2 Middle · F3 End · F4 Search · F5 Append · F6 Away · F7 Back");
                ui.label(format!(
                    "Mode: continuous | Conversation: {} | Phase: {} | Messages: {}",
                    self.conversation,
                    self.phase,
                    self.source.len()
                ));
                ui.separator();

                let mut scroller = egui::ScrollArea::vertical()
                    .id_salt(("qa-native-virtual-transcript", self.conversation))
                    .auto_shrink([false, false])
                    .stick_to_bottom(matches!(self.phase, "append" | "return"));
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
                                let bubble = draw_reader_bubble(
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
                                if bubble.rect.intersects(row.clip_rect()) {
                                    let clipped = bubble.rect.intersect(row.clip_rect());
                                    if clipped.width() >= 20.0 && clipped.height() >= 10.0 {
                                        visible.push(index);
                                        if index == self.source.len() - 1
                                            && self.source[index].text == APPEND_TEXT
                                        {
                                            appended_bubble_visible_height = clipped.height();
                                        }
                                        if index == SEARCH_TARGET {
                                            search_bubble_visible_height = clipped.height();
                                        }
                                    }
                                }
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
        self.write_status(
            ctx,
            scroll_offset,
            panel_width,
            &rendered,
            &visible,
            appended_bubble_visible_height,
            search_bubble_visible_height,
        );
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
