//! Headless egui ScrollArea integration over a large synthetic transcript.
//! This exercises the real virtualized bubble composition path, not just math.

use super::*;
use eframe::egui;

fn synthetic_message(sequence: usize) -> DisplayMessage {
    let text = match sequence % 5 {
        0 => "short",
        1 => "An ordinary text block with hard line breaks\nline two\nline three",
        2 => "# Heading\n\n- First\n- Second\n\nA longer paragraph about synthetic UI QA",
        3 => "日本語 Español العربية 🪐 repeated content with wrapping",
        _ => "The quick brown fox jumps over the lazy dog",
    };
    DisplayMessage {
        role: if sequence.is_multiple_of(2) {
            DisplayRole::User
        } else {
            DisplayRole::Assistant
        },
        text: text.to_owned(),
        sequence: sequence as u64,
        timestamp: None,
        provenance_label: None,
    }
}

#[test]
fn virtual_reader_scrollarea_renders_bounded_contiguous_windows() {
    let source = Arc::new((0..4_096).map(synthetic_message).collect::<Vec<_>>());
    let mut layout = VirtualReaderLayout::default();
    layout.sync(Some(Arc::clone(&source)), 720.0);
    let context = egui::Context::default();

    for step in 0..5 {
        let requested = match step {
            0 => 0.0,
            1 => layout.total_height() * 0.11,
            2 => layout.total_height() * 0.48,
            3 => layout.total_height() * 0.85,
            _ => (layout.total_height() - 900.0).max(0.0),
        };
        let mut drawn = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(720.0, 960.0),
            )),
            ..Default::default()
        };
        let output = context.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("synthetic-continuous-reader")
                    .vertical_scroll_offset(requested)
                    .show_viewport(ui, |ui, viewport| {
                        layout.sync(Some(Arc::clone(&source)), ui.available_width());
                        let list_top = ui.cursor().top();
                        let header_height = list_top - ui.max_rect().top();
                        let range = layout.visible_range(
                            viewport.min.y - header_height,
                            viewport.max.y - header_height,
                        );
                        ui.set_min_height(header_height + layout.total_height());
                        if range.is_empty() {
                            return;
                        }
                        let top = list_top + layout.offset(range.start);
                        let bottom = list_top + layout.offset(range.end);
                        let rect =
                            egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=bottom);
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |inner| {
                            for index in range {
                                inner.push_id(("synthetic-row", index), |row_ui| {
                                    let before = row_ui.cursor().top();
                                    let response = super::super::draw_reader_bubble(
                                        row_ui,
                                        &source[index],
                                        "",
                                        false,
                                        ctx,
                                    );
                                    drawn.push((index, response.rect));
                                    row_ui.add_space(12.0);
                                    layout.measure(index, row_ui.cursor().top() - before);
                                });
                            }
                        });
                        layout.finish_measurements();
                    });
            });
        });
        assert!(!output.shapes.is_empty());
        assert!(
            !drawn.is_empty(),
            "no visible synthetic rows at scroll step {step}"
        );
        assert!(
            drawn.len() < 48,
            "viewport laid out too many rows: {}",
            drawn.len()
        );
        for pair in drawn.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1, "virtual rows were not contiguous");
            assert!(
                pair[1].1.top() >= pair[0].1.bottom(),
                "virtual egui message bubble bounds overlap: {pair:?}"
            );
        }
    }
}
