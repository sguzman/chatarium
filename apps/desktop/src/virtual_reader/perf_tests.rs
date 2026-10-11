//! Non-gating release measurements of the native virtual reader planner.
//! Synthetic transcripts only; no user journal or browser session access.

use super::*;
use eframe::egui;
use std::hint::black_box;
use std::time::Instant;

fn message(sequence: u64, text: String) -> DisplayMessage {
    DisplayMessage {
        role: if sequence.is_multiple_of(2) {
            DisplayRole::User
        } else {
            DisplayRole::Assistant
        },
        text,
        sequence,
        timestamp: None,
        provenance_label: None,
    }
}

fn synthetic_archive(count: usize) -> Arc<Vec<DisplayMessage>> {
    Arc::new(
        (0..count)
            .map(|index| {
                let text = match index % 11 {
                    0 => String::new(),
                    1 => "short".to_owned(),
                    2 => "Español 日本語 العربية 🪐 ".repeat(8),
                    3 => "first\nsecond\nthird\n".repeat(5),
                    4 => "# heading\n\n- one\n- two\n".repeat(5),
                    5 => "code rust\nlet x = 1;\n".repeat(4),
                    6 => "long unbroken text ".repeat(65),
                    _ => "ordinary local conversation text ".repeat(5),
                };
                message(index as u64, text)
            })
            .collect(),
    )
}

fn median_us(values: &[u128]) -> u128 {
    let mut copy = values.to_vec();
    copy.sort_unstable();
    copy[copy.len() / 2]
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|number| number.parse().ok())
}

/// Reproduce the pre-optimization nested binary-search reference inside
/// the profiler only. Its f64 Fenwick sums and float bounds must match the
/// shipping rank-based viewport planner exactly.
fn legacy_row_at(layout: &VirtualReaderLayout, top: f32) -> Option<usize> {
    let count = layout.heights.len();
    if count == 0 {
        return None;
    }
    let top = f64::from(top.max(0.0));
    let mut low = 0;
    let mut high = count;
    while low < high {
        let mid = low + (high - low) / 2;
        if layout.height_sum(mid + 1) <= top {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    Some(low.min(count - 1))
}

fn legacy_visible_range(
    layout: &VirtualReaderLayout,
    top: f32,
    bottom: f32,
) -> std::ops::Range<usize> {
    let count = layout.heights.len();
    if count == 0 {
        return 0..0;
    }
    let top = f64::from((top - OVERSCAN_PX).max(0.0));
    let bottom = f64::from((bottom + OVERSCAN_PX).max(top as f32));
    let mut low = 0;
    let mut high = count;
    while low < high {
        let mid = low + (high - low) / 2;
        if layout.height_sum(mid + 1) <= top {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let start = low;
    low = 0;
    high = count;
    while low < high {
        let mid = low + (high - low) / 2;
        if layout.height_sum(mid) < bottom {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    start..low.max(start.saturating_add(1)).min(count)
}

fn viewport_scan(layout: &VirtualReaderLayout, positions: &[f32], legacy: bool) -> usize {
    let mut checksum = 0_usize;
    for &top in positions {
        let bottom = top + 900.0;
        let range = if legacy {
            legacy_visible_range(layout, top, bottom)
        } else {
            layout.visible_range(top, bottom)
        };
        let anchor = if legacy {
            legacy_row_at(layout, top)
        } else {
            layout.row_at(top)
        };
        checksum = checksum
            .wrapping_add(range.len())
            .wrapping_add(anchor.unwrap_or(0));
    }
    black_box(checksum)
}

#[test]
#[ignore = "opt-in synthetic profiling; no wall-clock gates"]
fn virtual_reader_perf_profile() {
    const MESSAGES: usize = 65_536;
    const COLD_SAMPLES: usize = 9;
    const VIEWPORT_QUERIES: usize = 20_000;
    const HEIGHT_EDITS: usize = 12_000;
    const CACHE_HITS: usize = 100_000;

    let archive = synthetic_archive(MESSAGES);
    let rss_before_kib = rss_kib();
    let mut cold_us = Vec::with_capacity(COLD_SAMPLES);
    for _ in 0..COLD_SAMPLES {
        let mut planner = VirtualReaderLayout::default();
        let start = Instant::now();
        planner.sync(Some(Arc::clone(&archive)), 800.0);
        cold_us.push(start.elapsed().as_micros());
        assert!(planner.total_height() > 0.0);
        black_box(planner);
    }

    let mut planner = VirtualReaderLayout::default();
    planner.sync(Some(Arc::clone(&archive)), 800.0);
    let rss_after_initial_layout_kib = rss_kib();

    let warm_start = Instant::now();
    for _ in 0..CACHE_HITS {
        planner.sync(Some(Arc::clone(&archive)), 800.0);
    }
    let warm_cache_us = warm_start.elapsed().as_micros();
    assert!(!planner.finish_measurements());

    let height = planner.total_height();
    let mut viewport_rows = 0_usize;
    let viewport_start = Instant::now();
    for iteration in 0..VIEWPORT_QUERIES {
        let offset = (iteration * 104_729 % MESSAGES) as f32 / MESSAGES as f32 * height;
        let range = planner.visible_range(offset, offset + 900.0);
        assert!(range.start <= range.end && range.end <= MESSAGES);
        viewport_rows = viewport_rows.wrapping_add(range.len());
        black_box(planner.row_at(offset));
    }
    let viewport_queries_us = viewport_start.elapsed().as_micros();
    black_box(viewport_rows);

    // Same-run A/B: alternate order to reduce CPU cache and clock bias.
    // Absolute timing between different GitHub runners is not a speedup
    // claim. Equality is verified before either timed loop.
    const AB_ROUNDS: usize = 9;
    let positions = (0..VIEWPORT_QUERIES)
        .map(|iteration| (iteration * 104_729 % MESSAGES) as f32 / MESSAGES as f32 * height)
        .collect::<Vec<_>>();
    for &top in &positions {
        assert_eq!(
            legacy_visible_range(&planner, top, top + 900.0),
            planner.visible_range(top, top + 900.0)
        );
        assert_eq!(legacy_row_at(&planner, top), planner.row_at(top));
    }
    let mut legacy_samples_us = Vec::with_capacity(AB_ROUNDS);
    let mut prefix_samples_us = Vec::with_capacity(AB_ROUNDS);
    let mut ab_checksum = None;
    for round in 0..AB_ROUNDS {
        for legacy in if round.is_multiple_of(2) {
            [true, false]
        } else {
            [false, true]
        } {
            let start = Instant::now();
            let checksum = viewport_scan(&planner, &positions, legacy);
            let elapsed = start.elapsed().as_micros();
            if let Some(expected) = ab_checksum {
                assert_eq!(checksum, expected);
            } else {
                ab_checksum = Some(checksum);
            }
            if legacy {
                legacy_samples_us.push(elapsed);
            } else {
                prefix_samples_us.push(elapsed);
            }
        }
    }

    let measure_start = Instant::now();
    for iteration in 0..HEIGHT_EDITS {
        let index = iteration * 8191 % MESSAGES;
        planner.measure(index, 60.0 + (iteration * 37 % 10_000) as f32);
    }
    let measured_updates_us = measure_start.elapsed().as_micros();
    assert!(planner.finish_measurements());
    assert!(!planner.finish_measurements());

    let mut append_samples_us = Vec::new();
    let mut current = Arc::clone(&archive);
    for iteration in 0..COLD_SAMPLES {
        let mut next = current.as_ref().clone();
        next.push(message(
            (MESSAGES + iteration) as u64,
            "appended user turn".to_owned(),
        ));
        let next = Arc::new(next);
        let start = Instant::now();
        planner.sync(Some(Arc::clone(&next)), 800.0);
        append_samples_us.push(start.elapsed().as_micros());
        current = next;
        assert!(planner.total_height() > 0.0);
    }
    assert_eq!(current.len(), MESSAGES + COLD_SAMPLES);

    let mut resize_samples_us = Vec::new();
    for iteration in 0..COLD_SAMPLES {
        let width = if iteration.is_multiple_of(2) {
            470.0
        } else {
            800.0
        };
        let start = Instant::now();
        planner.sync(Some(Arc::clone(&current)), width);
        resize_samples_us.push(start.elapsed().as_micros());
    }

    // Measure the production egui bubble renderer, not a height estimator.
    let render_messages = synthetic_archive(28);
    let mut headless_frames_us = Vec::new();
    let context = egui::Context::default();
    for _ in 0..COLD_SAMPLES {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 5_000.0),
            )),
            ..Default::default()
        };
        let start = Instant::now();
        let output = context.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for record in render_messages.iter() {
                    super::super::draw_reader_bubble(ui, record, "", false, ctx);
                    ui.add_space(12.0);
                }
            });
        });
        headless_frames_us.push(start.elapsed().as_micros());
        assert!(!output.shapes.is_empty());
        black_box(output.shapes.len());
    }

    eprintln!(
        "CHATARIUM_VIRTUAL_READER_PROFILE {}",
        serde_json::json!({
            "schema": "chatarium-virtual-reader-profile",
            "version": 1,
            "mode": if cfg!(debug_assertions) { "debug-ci-synthetic" } else { "release-ci-synthetic" },
            "profile_optimized": !cfg!(debug_assertions),
            "messages": MESSAGES,
            "cold_samples": COLD_SAMPLES,
            "cold_sync_median_us": median_us(&cold_us),
            "cold_sync_min_us": cold_us.iter().min(),
            "cold_sync_max_us": cold_us.iter().max(),
            "warm_cache_hits": CACHE_HITS,
            "warm_cache_total_us": warm_cache_us,
            "viewport_queries": VIEWPORT_QUERIES,
            "viewport_queries_total_us": viewport_queries_us,
            "viewport_rows_checksum": viewport_rows,
            "ab_rounds": AB_ROUNDS,
            "ab_legacy_nested_search_median_us": median_us(&legacy_samples_us),
            "ab_prefix_descent_median_us": median_us(&prefix_samples_us),
            "ab_matching_checksum": ab_checksum,
            "measured_height_edits": HEIGHT_EDITS,
            "measured_edits_total_us": measured_updates_us,
            "append_samples": append_samples_us.len(),
            "append_sync_median_us": median_us(&append_samples_us),
            "resize_samples": resize_samples_us.len(),
            "resize_sync_median_us": median_us(&resize_samples_us),
            "headless_renderer_frames": headless_frames_us.len(),
            "headless_renderer_frame_median_us": median_us(&headless_frames_us),
            "rss_before_kib": rss_before_kib,
            "rss_after_initial_layout_kib": rss_after_initial_layout_kib,
        })
    );
}
