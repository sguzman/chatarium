//! Variable-height viewport planning for the native local transcript reader.
//!
//! This cache is a disposable UI measurement projection. The journal and
//! native conversation search index remain the authoritative message source.
//! No hidden memory, tool output, or cross-conversation content is consulted.

use super::{DisplayMessage, DisplayRole};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

const MIN_ROW_HEIGHT: f32 = 48.0;
const OVERSCAN_PX: f32 = 320.0;

#[derive(Clone, Default)]
pub struct VirtualReaderLayout {
    source: Option<Arc<Vec<DisplayMessage>>>,
    signatures: Vec<u64>,
    heights: Vec<f32>,
    measured: Vec<bool>,
    /// Fenwick sums permit O(log n) height changes without rebuilding the tail.
    height_tree: Vec<f64>,
    width: f32,
    changed: bool,
    last_search: Option<(String, Option<usize>)>,
    last_view_offset: Option<f32>,
}

impl VirtualReaderLayout {
    /// Incremental source identity, with exact per-message hashes on changes.
    /// Changing window width invalidates measurements because wrapped bubbles
    /// can become taller or shorter without any transcript change.
    pub fn sync(&mut self, source: Option<Arc<Vec<DisplayMessage>>>, width: f32) {
        let stable_source = match (&self.source, &source) {
            (Some(before), Some(after)) => Arc::ptr_eq(before, after),
            (None, None) => true,
            _ => false,
        };
        let width = width.max(1.0);
        let same_width = (self.width - width).abs() <= 1.0;
        if stable_source && same_width && self.height_tree.len() == self.heights.len() + 1 {
            return;
        }
        let mut next_signatures = Vec::new();
        let mut next_heights = Vec::new();
        let mut next_measured = Vec::new();
        if let Some(messages) = &source {
            next_signatures.reserve(messages.len());
            next_heights.reserve(messages.len());
            next_measured.reserve(messages.len());
            for (index, message) in messages.iter().enumerate() {
                let signature = message_signature(message);
                let preserve = same_width
                    && self.signatures.get(index) == Some(&signature)
                    && self.measured.get(index) == Some(&true);
                next_signatures.push(signature);
                next_heights.push(if preserve {
                    self.heights[index]
                } else {
                    estimated_height(message, width)
                });
                next_measured.push(preserve);
            }
        }
        // A selected search hit must be revealed again after reflow or an
        // edit preceding its row. Pure tail appends leave its anchor intact
        // and must not unexpectedly undo the reader's manual scrolling.
        let anchor_changed = self
            .last_search
            .as_ref()
            .and_then(|(_, target)| *target)
            .is_some_and(|target| {
                !same_width || self.signatures.get(..=target) != next_signatures.get(..=target)
            });
        if anchor_changed {
            self.last_search = None;
        }
        self.source = source;
        self.signatures = next_signatures;
        self.heights = next_heights;
        self.measured = next_measured;
        self.width = width;
        self.rebuild_height_tree();
        self.changed = false;
    }

    #[must_use]
    pub fn saved_view_offset(&self) -> Option<f32> {
        self.last_view_offset
    }

    pub fn remember_view_offset(&mut self, offset: f32) {
        if offset.is_finite() && offset >= 0.0 {
            self.last_view_offset = Some(offset);
        }
    }

    /// Build a Fenwick tree in linear time. f64 internal sums minimize
    /// cumulative rounding over large archives; egui coordinates remain f32.
    fn rebuild_height_tree(&mut self) {
        let count = self.heights.len();
        self.height_tree = vec![0.0; count + 1];
        for index in 1..=count {
            self.height_tree[index] += f64::from(self.heights[index - 1]);
            let parent = index + (index & index.wrapping_neg());
            if parent <= count {
                self.height_tree[parent] += self.height_tree[index];
            }
        }
    }

    fn add_height_delta(&mut self, index: usize, delta: f64) {
        let mut position = index + 1;
        while position < self.height_tree.len() {
            self.height_tree[position] += delta;
            position += position & position.wrapping_neg();
        }
    }

    fn height_sum(&self, length: usize) -> f64 {
        let mut position = length.min(self.heights.len());
        let mut sum = 0.0;
        while position > 0 {
            sum += self.height_tree[position];
            position -= position & position.wrapping_neg();
        }
        sum
    }

    #[must_use]
    pub fn total_height(&self) -> f32 {
        self.height_sum(self.heights.len()) as f32
    }

    #[must_use]
    pub fn offset(&self, index: usize) -> f32 {
        self.height_sum(index) as f32
    }

    /// Locate the row intersecting the un-overscanned viewport top. Keeping
    /// its start position stable prevents upper overscan measurements from
    /// visibly shifting the user's reading position.
    #[must_use]
    pub fn row_at(&self, top: f32) -> Option<usize> {
        let count = self.heights.len();
        if count == 0 {
            return None;
        }
        let top = f64::from(top.max(0.0));
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            if self.height_sum(mid + 1) <= top {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        Some(low.min(count - 1))
    }

    #[must_use]
    pub fn visible_range(&self, top: f32, bottom: f32) -> Range<usize> {
        let count = self.heights.len();
        if count == 0 {
            return 0..0;
        }
        let top = f64::from((top - OVERSCAN_PX).max(0.0));
        let bottom = f64::from((bottom + OVERSCAN_PX).max(top as f32));

        // Find the first row whose end lies strictly after the viewport top.
        let mut low = 0;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            if self.height_sum(mid + 1) <= top {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let start = low;

        // Find the first row whose start reaches the viewport bottom.
        low = 0;
        high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            if self.height_sum(mid) < bottom {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        start..low.max(start.saturating_add(1)).min(count)
    }

    /// Record only heights measured by egui from actually rendered bubbles.
    /// A point update is O(log n), even for bubbles near the archive start.
    pub fn measure(&mut self, index: usize, height: f32) {
        if !height.is_finite() || height < MIN_ROW_HEIGHT {
            return;
        }
        let Some(previous) = self.heights.get_mut(index) else {
            return;
        };
        if !self.measured[index] || (*previous - height).abs() > 0.5 {
            let delta = f64::from(height) - f64::from(*previous);
            *previous = height;
            self.measured[index] = true;
            self.add_height_delta(index, delta);
            self.changed = true;
        }
    }

    /// Reports whether measurements changed since the last frame; no
    /// downstream prefix rebuild is necessary.
    pub fn finish_measurements(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Navigate to a new result using virtual offsets without requiring
    /// the target bubble to have been instantiated in the prior frame.
    pub fn new_search_offset(&mut self, query: &str, hit_message: Option<usize>) -> Option<f32> {
        let selection = (query.to_owned(), hit_message);
        if self.last_search.as_ref() == Some(&selection) {
            return None;
        }
        if query.trim().is_empty() {
            self.last_search = Some(selection);
            return None;
        }
        let offset = hit_message
            .filter(|index| *index < self.heights.len())
            .map(|index| self.offset(index));
        // The search index and visible projection can briefly disagree while
        // a journal append is applied. Do not consume an unreachable target.
        if offset.is_some() || hit_message.is_none() {
            self.last_search = Some(selection);
        }
        offset
    }
}

fn message_signature(message: &DisplayMessage) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message.sequence.hash(&mut hasher);
    match message.role {
        DisplayRole::User => 0_u8,
        DisplayRole::Assistant => 1_u8,
    }
    .hash(&mut hasher);
    message.text.hash(&mut hasher);
    message.provenance_label.hash(&mut hasher);
    message.timestamp.map(f64::to_bits).hash(&mut hasher);
    hasher.finish()
}

fn estimated_height(message: &DisplayMessage, width: f32) -> f32 {
    let content_width = (width * 0.72).clamp(320.0, 900.0);
    let approximate_characters_per_line = (content_width / 8.0).max(24.0);
    let hard_lines = message.text.lines().count().max(1) as f32;
    let wrapped_lines = message.text.chars().count() as f32 / approximate_characters_per_line;
    (76.0 + (hard_lines + wrapped_lines) * 15.0).clamp(96.0, 12_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(sequence: u64, text: &str) -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::User,
            text: text.to_owned(),
            sequence,
            timestamp: None,
            provenance_label: None,
        }
    }

    #[test]
    fn variable_heights_map_viewports_to_source_message_indices() {
        let source = Arc::new(vec![
            message(1, "short"),
            message(2, "a long message with \n line breaks 🌿"),
            message(3, "last"),
        ]);
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(source), 640.0);
        layout.measure(0, 105.0);
        layout.measure(1, 470.0);
        layout.measure(2, 90.0);
        assert!(layout.finish_measurements());
        assert_eq!(layout.offset(0), 0.0);
        assert_eq!(layout.offset(1), 105.0);
        assert_eq!(layout.offset(2), 575.0);
        assert_eq!(layout.total_height(), 665.0);
        assert_eq!(layout.row_at(0.0), Some(0));
        assert_eq!(layout.row_at(105.0), Some(1));
        assert_eq!(layout.row_at(600.0), Some(2));
        assert_eq!(layout.row_at(10_000.0), Some(2));
        let anchor_before = layout.offset(2);
        layout.measure(0, 205.0);
        assert!(layout.finish_measurements());
        assert_eq!(layout.offset(2) - anchor_before, 100.0);
        layout.measure(0, 105.0);
        assert!(layout.finish_measurements());
        // Overscan extends only 320px above 450px, so the 105px
        // first message is rightly excluded while rows 1-2 remain visible.
        assert_eq!(layout.visible_range(450.0, 455.0), 1..3);
        assert_eq!(layout.new_search_offset("last", Some(2)), Some(575.0));
        assert_eq!(layout.new_search_offset("last", Some(2)), None);
        assert_eq!(layout.new_search_offset("last", Some(99)), None);
        assert_eq!(layout.new_search_offset("", Some(2)), None);
    }

    #[test]
    fn cached_measurements_survive_unrelated_updates_but_not_mutations_or_resize() {
        let old = Arc::new(vec![message(1, "one"), message(2, "two")]);
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(Arc::clone(&old)), 700.0);
        layout.measure(0, 399.0);
        layout.measure(1, 222.0);
        assert!(layout.finish_measurements());
        layout.sync(Some(Arc::clone(&old)), 700.0);
        assert_eq!(layout.offset(1), 399.0);
        assert!(!layout.finish_measurements());

        let with_append = Arc::new(vec![
            message(1, "one"),
            message(2, "two"),
            message(3, "three"),
        ]);
        // An out-of-range hit is not consumed before the appended projection
        // contains its actual row.
        assert_eq!(layout.new_search_offset("three", Some(2)), None);
        layout.sync(Some(with_append), 700.0);
        assert_eq!(
            layout.new_search_offset("three", Some(2)),
            Some(layout.offset(2))
        );
        assert_eq!(layout.offset(1), 399.0);
        assert_eq!(layout.offset(2), 621.0);

        let edited = Arc::new(vec![
            message(1, "one"),
            message(2, "another two"),
            message(3, "three"),
        ]);
        assert_eq!(
            layout.new_search_offset("two", Some(1)),
            Some(layout.offset(1))
        );
        layout.sync(Some(edited), 700.0);
        assert_eq!(
            layout.new_search_offset("two", Some(1)),
            Some(layout.offset(1))
        );
        assert_eq!(layout.offset(1), 399.0);
        assert_ne!(layout.offset(2), 621.0);

        layout.sync(Some(Arc::new(vec![message(1, "one")])), 450.0);
        assert_eq!(layout.new_search_offset("one", Some(0)), Some(0.0));
        assert_eq!(layout.visible_range(0.0, 300.0), 0..1);
        assert_ne!(layout.offset(1), 399.0);
        layout.sync(None, 450.0);
        assert_eq!(layout.visible_range(0.0, 300.0), 0..0);
        assert_eq!(layout.row_at(0.0), None);
        assert_eq!(layout.saved_view_offset(), None);
        layout.remember_view_offset(245.0);
        layout.remember_view_offset(f32::NAN);
        assert_eq!(layout.saved_view_offset(), Some(245.0));
    }

    #[test]
    fn changing_early_rows_updates_distant_offsets_without_rebuilding_the_tail() {
        let source = Arc::new((0..32_768).map(|i| message(i, "entry")).collect());
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(source), 850.0);
        let old_first = layout.offset(1);
        let old_tail = layout.total_height();
        layout.measure(0, 500.0);
        layout.measure(0, 600.0);
        layout.measure(16_384, 380.0);
        assert!(layout.finish_measurements());
        assert!(!layout.finish_measurements());
        assert_eq!(layout.offset(1), 600.0);
        let expected_tail = old_tail + 600.0 - old_first + 380.0 - old_first;
        assert!((layout.total_height() - expected_tail).abs() < 1.0);
        assert!(
            layout
                .visible_range(layout.offset(20_000), layout.offset(20_000) + 650.0)
                .contains(&20_000)
        );
        // Unchanged measurements are not reported as another mutation.
        layout.measure(0, 600.0);
        assert!(!layout.finish_measurements());
    }

    /// A simple linear reference independent of the Fenwick implementation.
    /// It deliberately uses f64 sums to catch cumulative rounding errors.
    fn reference_offset(heights: &[f32], index: usize) -> f64 {
        heights
            .iter()
            .take(index.min(heights.len()))
            .map(|height| f64::from(*height))
            .sum()
    }

    fn reference_visible_range(heights: &[f32], top: f32, bottom: f32) -> Range<usize> {
        if heights.is_empty() {
            return 0..0;
        }
        let top = f64::from((top - OVERSCAN_PX).max(0.0));
        let bottom = f64::from((bottom + OVERSCAN_PX).max(top as f32));
        let mut start = heights.len();
        let mut end = 0;
        let mut running = 0.0;
        for (index, height) in heights.iter().enumerate() {
            let next = running + f64::from(*height);
            if start == heights.len() && next > top {
                start = index;
            }
            if running < bottom {
                end = index + 1;
            }
            running = next;
        }
        start..end.max(start.saturating_add(1)).min(heights.len())
    }

    #[test]
    fn irregular_aggregate_height_replay_agrees_with_linear_geometry_oracle() {
        // Reproduce a diverse archive without network, user history, a GPU,
        // external dependencies or nondeterministic seeds. Long messages,
        // Unicode, tiny messages, soft wrapping and hard line breaks coexist.
        let mut source = Arc::new(
            (0..4_096)
                .map(|index| {
                    let text = match index % 7 {
                        0 => String::new(),
                        1 => "short".to_owned(),
                        2 => "Español 日本語 العربية 🪐".repeat(13),
                        3 => "first\nsecond\nthird".repeat(7),
                        4 => "wide code: ".to_owned() + &"x".repeat(700),
                        5 => "x".repeat(2_048),
                        _ => "ordinary user / assistant message".to_owned(),
                    };
                    message(index, &text)
                })
                .collect(),
        );
        let mut width = 800.0;
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(Arc::clone(&source)), width);
        let mut expected = source
            .iter()
            .map(|entry| estimated_height(entry, width))
            .collect::<Vec<_>>();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next_random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for step in 0..768 {
            if step > 0 && step % 113 == 0 {
                width = if width == 800.0 { 460.0 } else { 800.0 };
                layout.sync(Some(Arc::clone(&source)), width);
                expected = source
                    .iter()
                    .map(|entry| estimated_height(entry, width))
                    .collect();
            } else if step > 0 && step % 181 == 0 {
                let mut appended = source.as_ref().clone();
                appended.push(message(
                    appended.len() as u64,
                    "new multiline message\nwith 🪐 Unicode",
                ));
                source = Arc::new(appended);
                layout.sync(Some(Arc::clone(&source)), width);
                expected.push(estimated_height(source.last().unwrap(), width));
            } else {
                let index = (next_random() as usize) % expected.len();
                let height = 48.0 + (next_random() % 20_000) as f32;
                layout.measure(index, height);
                expected[index] = height;
                assert!(layout.finish_measurements());
            }

            let measured = expected.len();
            let random_index = (next_random() as usize) % (measured + 1);
            let reference = reference_offset(&expected, random_index);
            let actual = f64::from(layout.offset(random_index));
            assert!(
                (actual - reference).abs() <= (reference.abs() * 0.000_000_2).max(1.0),
                "offset mismatch at iteration {step}, row {random_index}: {actual} != {reference}"
            );

            let total = reference_offset(&expected, measured);
            let top = ((next_random() as f64 / u64::MAX as f64) * total) as f32;
            let bottom = top + 900.0;
            assert_eq!(
                layout.visible_range(top, bottom),
                reference_visible_range(&expected, top, bottom),
                "incorrect virtual window at iteration {step}"
            );
            let expected_row = expected
                .iter()
                .scan(0.0_f64, |sum, height| {
                    *sum += f64::from(*height);
                    Some(*sum)
                })
                .position(|end| end > f64::from(top))
                .unwrap_or(measured - 1);
            assert_eq!(
                layout.row_at(top),
                Some(expected_row),
                "incorrect viewport anchor at iteration {step}"
            );
        }
        assert_eq!(expected.len(), 4_100);
    }

    #[test]
    fn headless_egui_reader_bubbles_keep_separate_bounds_after_reflow() {
        use eframe::egui;

        // Drive the real production bubble renderer through an egui frame.
        // No GPU, window server, private transcript, or screenshot fixture is
        // required. Paint output and bubble geometry must remain well-formed.
        let mut records = vec![
            message(1, "A short local user turn"),
            message(
                2,
                "# Heading\n\nA paragraph with Español, 日本語, العربية and 🪐.\n\n- First\n- Second\n\n`rust\nlet x = 1;\n`",
            ),
            message(3, &("This paragraph needs to wrap naturally. ".repeat(115))),
            message(4, "first line\nsecond line\nthird line\n"),
        ];
        records[1].role = DisplayRole::Assistant;
        records[3].role = DisplayRole::Assistant;

        let mut former_heights = Vec::new();
        for width in [460.0_f32, 700.0, 1_200.0] {
            let context = egui::Context::default();
            let mut bounds = Vec::new();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 1_200.0),
                )),
                ..Default::default()
            };
            let output = context.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    for record in &records {
                        let response = super::draw_reader_bubble(
                            ui,
                            record,
                            "",
                            false,
                            ctx,
                        );
                        bounds.push(response.rect);
                        ui.add_space(12.0);
                    }
                });
            });
            assert!(!output.shapes.is_empty(), "egui did not emit paint output");
            assert_eq!(bounds.len(), records.len());

            for (index, rect) in bounds.iter().enumerate() {
                assert!(
                    rect.width().is_finite()
                        && rect.height().is_finite()
                        && rect.width() > 0.0
                        && rect.height() > 0.0,
                    "invalid rendered bubble {index} at width {width}: {rect:?}"
                );
                assert!(
                    rect.left() >= -1.0 && rect.right() <= width + 1.0,
                    "bubble {index} escaped horizontal bounds at width {width}: {rect:?}"
                );
            }
            for pair in bounds.windows(2) {
                assert!(
                    pair[1].top() >= pair[0].bottom(),
                    "actual egui bubble rectangles overlap vertically at width {width}: {pair:?}"
                );
            }
            if !former_heights.is_empty() {
                // The long paragraph should not become taller when more width
                // is available. The other bubbles include hard line breaks
                // and may retain the same height.
                assert!(
                    bounds[2].height() <= former_heights[2] + 2.0,
                    "long paragraph became taller on a wider reader"
                );
            }
            former_heights = bounds.iter().map(egui::Rect::height).collect();
        }
    }

    #[test]
    fn huge_archives_use_a_bounded_row_subset_and_ignore_bad_heights() {
        let source = Arc::new((0..16_384).map(|i| message(i, "one row")).collect());
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(source), 850.0);
        let range = layout.visible_range(
            layout.total_height() / 2.0,
            layout.total_height() / 2.0 + 700.0,
        );
        assert!(!range.is_empty());
        assert!(range.len() < 50);
        let before = layout.total_height();
        layout.measure(range.start, f32::NAN);
        layout.measure(range.start, -50.0);
        assert!(!layout.finish_measurements());
        assert_eq!(layout.total_height(), before);
    }
}
