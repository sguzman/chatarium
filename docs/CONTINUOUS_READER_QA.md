# Continuous reader: performance and acceptance

Chatarium's native **Continuous reader** is an opt-in, variable-height egui
viewport over the existing source-message projection. It never changes local
journal contents, inferred conversation ownership, search permissions, memory
admission, transport, or request context. The 160-message paged reader remains
the default for sufficiently large native archives, and full rendering remains
available. Imported and mirrored historical readers are separate.

## Automated evidence

- Unit tests: index-to-offset geometry, exact Fenwick rank boundaries,
  measured-height updates, source edits, width changes, and search anchor
  recovery.
- Deterministic 4,096-message stress replay: multilingual content, long code,
  line breaks, append, repeated widths, and an independent linear reference
  for viewport geometry.
- Large-archive tests: 16,384 and 32,768-message windows remain bounded;
  bad measurements are rejected.
- Headless egui test: the production bubble renderer is exercised at several
  widths; emitted shapes must have finite bounds and non-overlapping bubbles.
  This is a rendering-geometry test, **not** a pixel screenshot comparison.
- CI: the regular workflow tests Rust and Linux desktop behavior. The separate
  virtual-reader release-profile workflow is path-triggered and runs the
  ignored, synthetic benchmark in an optimized binary.

## Performance methodology

The release profiler uses a synthetic 65,536-message archive. It records
medians for cold layout construction, append and resize rebuilds, a warm cache,
20,000 viewport queries, 12,000 measured-height changes, and headless bubble
rendering. Linux process RSS is observational.

Individual GitHub Actions runner timings are not directly comparable. Starting
with schema version 1, the profiler also measures the legacy nested
binary-search implementation against Fenwick prefix descent **in the same
process**, alternating execution order and asserting identical viewport ranges
and anchors. Timing differences are informational, never a CI duration gate.

Historical starting points:
- Baseline release profile: https://github.com/sguzman/chatarium/actions/runs/38167022174
- First prefix-descent release profile: https://github.com/sguzman/chatarium/actions/runs/38167290782

To run locally without human GUI interaction:

    cargo test -p chatarium-desktop --bin chatarium-desktop virtual_reader -- --nocapture
    cargo test --release -p chatarium-desktop --bin chatarium-desktop virtual_reader_perf_profile -- --ignored --nocapture

## Synthetic native screenshot QA

The isolated `--qa-reader` entry point creates a **disposable** 4,096-message
synthetic reader, using the production bubble renderer and virtual layout inside
a real native eframe window. It branches before normal application startup and
does not open the user's journal, browser, account bridge, or network provider.

The dedicated `native-reader-visual-qa` GitHub Actions workflow builds the
actual desktop binary and runs it under Xvfb/Openbox with software graphics.
X11 automation captures six PNG screenshots per scale: top, middle,
wheel scroll, end,
long-distance search jump, and resize after search. It checks for nonblank
screenshots, changed pixel contents between scroll positions, bounded visible
row counts, expected source indices, and search-target visibility.

The workflow repeats the scenarios under `WINIT_X11_SCALE_FACTOR=1.25`
in a separate Xvfb display and synthetic output directory.

Evidence is uploaded from hidden disposable QA directories with
`include-hidden-files: true`, and the job fails if screenshot artifacts are
missing. Artifacts are retained for seven days; no PNG or telemetry file is
checked into Git. The test prints a machine-readable synthetic-only
`CHATARIUM_NATIVE_VISUAL_QA` summary.

This is **native screenshot coverage of a synthetic reader shell**, not yet a
complete screenshot acceptance test of the ordinary authenticated
`ChatariumApp` workspace. Pixel-difference checks establish that real content
was painted and changed; they do not establish typographic correctness.
Remaining work includes whole-app conversation switching, typed-message
appends, search result changes after reflow, keyboard PageUp/PageDown, high-DPI
coverage, and reviewed image comparisons.

## Promotion criteria

Do **not** enable Continuous reader by default based on performance timings
alone. Before promotion, automated native UI QA must exercise first/last,
search jump, wheel/page scroll, resize, appending, and conversation switching
in a real desktop session; capture screenshots and compare visual content, not
just egui rectangle bounds. Keep long-code and production-app fixture coverage before promotion.

QA owns setup, data generation, launching, interaction, screenshots, and
recovery. No principal-operated regression clicks or private-session exports
are acceptance prerequisites.
