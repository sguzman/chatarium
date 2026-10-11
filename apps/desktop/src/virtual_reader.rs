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
    prefix: Vec<f32>,
    width: f32,
    dirty_from: Option<usize>,
    last_search: Option<(String, Option<usize>)>,
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
        if stable_source && same_width && self.prefix.len() == self.heights.len() + 1 {
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
        self.source = source;
        self.signatures = next_signatures;
        self.heights = next_heights;
        self.measured = next_measured;
        self.width = width;
        self.prefix = vec![0.0; self.heights.len() + 1];
        self.dirty_from = Some(0);
        self.finish_measurements();
    }

    #[must_use]
    pub fn total_height(&self) -> f32 {
        self.prefix.last().copied().unwrap_or(0.0)
    }

    #[must_use]
    pub fn offset(&self, index: usize) -> f32 {
        self.prefix
            .get(index.min(self.heights.len()))
            .copied()
            .unwrap_or(0.0)
    }

    #[must_use]
    pub fn visible_range(&self, top: f32, bottom: f32) -> Range<usize> {
        if self.heights.is_empty() {
            return 0..0;
        }
        let top = (top - OVERSCAN_PX).max(0.0);
        let bottom = (bottom + OVERSCAN_PX).max(top);
        // Prefix sums remain monotonic for nonnegative measured heights.
        let start = self.prefix[1..].partition_point(|end| *end <= top);
        let end = self.prefix[..self.heights.len()]
            .partition_point(|start| *start < bottom)
            .max(start.saturating_add(1))
            .min(self.heights.len());
        start.min(self.heights.len())..end
    }

    /// Record only heights measured by egui from actually rendered bubbles.
    /// Coalesce recalculation into one tail prefix rebuild per frame.
    pub fn measure(&mut self, index: usize, height: f32) {
        if !height.is_finite() || height < MIN_ROW_HEIGHT {
            return;
        }
        let Some(previous) = self.heights.get_mut(index) else {
            return;
        };
        if !self.measured[index] || (*previous - height).abs() > 0.5 {
            *previous = height;
            self.measured[index] = true;
            self.dirty_from = Some(self.dirty_from.map_or(index, |prior| prior.min(index)));
        }
    }

    /// Returns true if the layout's total height or row positions changed.
    pub fn finish_measurements(&mut self) -> bool {
        let Some(start) = self.dirty_from.take() else {
            return false;
        };
        if self.prefix.len() != self.heights.len() + 1 {
            self.prefix.resize(self.heights.len() + 1, 0.0);
        }
        for index in start..self.heights.len() {
            self.prefix[index + 1] = self.prefix[index] + self.heights[index];
        }
        true
    }

    /// Navigate to a new result using virtual offsets without requiring
    /// the target bubble to have been instantiated in the prior frame.
    pub fn new_search_offset(
        &mut self,
        query: &str,
        hit_message: Option<usize>,
    ) -> Option<f32> {
        let selection = (query.to_owned(), hit_message);
        if self.last_search.as_ref() == Some(&selection) {
            return None;
        }
        self.last_search = Some(selection);
        if query.trim().is_empty() {
            return None;
        }
        hit_message
            .filter(|index| *index < self.heights.len())
            .map(|index| (self.offset(index) - 100.0).max(0.0))
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
        assert_eq!(layout.visible_range(450.0, 455.0), 0..3);
        assert_eq!(layout.new_search_offset("last", Some(2)), Some(475.0));
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
        layout.sync(Some(with_append), 700.0);
        assert_eq!(layout.offset(1), 399.0);
        assert_eq!(layout.offset(2), 621.0);

        let edited = Arc::new(vec![
            message(1, "one"),
            message(2, "another two"),
            message(3, "three"),
        ]);
        layout.sync(Some(edited), 700.0);
        assert_eq!(layout.offset(1), 399.0);
        assert_ne!(layout.offset(2), 621.0);

        layout.sync(Some(Arc::new(vec![message(1, "one")])), 450.0);
        assert_eq!(layout.visible_range(0.0, 300.0), 0..1);
        assert_ne!(layout.offset(1), 399.0);
        layout.sync(None, 450.0);
        assert_eq!(layout.visible_range(0.0, 300.0), 0..0);
    }

    #[test]
    fn huge_archives_use_a_bounded_row_subset_and_ignore_bad_heights() {
        let source = Arc::new((0..16_384).map(|i| message(i, "one row")).collect());
        let mut layout = VirtualReaderLayout::default();
        layout.sync(Some(source), 850.0);
        let range = layout.visible_range(layout.total_height() / 2.0, layout.total_height() / 2.0 + 700.0);
        assert!(!range.is_empty());
        assert!(range.len() < 50);
        let before = layout.total_height();
        layout.measure(range.start, f32::NAN);
        layout.measure(range.start, -50.0);
        assert!(!layout.finish_measurements());
        assert_eq!(layout.total_height(), before);
    }
}
