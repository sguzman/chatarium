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

## Promotion criteria

Do **not** enable Continuous reader by default based on performance timings
alone. Before promotion, automated native UI QA must exercise first/last,
search jump, wheel/page scroll, resize, appending, and conversation switching
in a real desktop session; capture screenshots and compare visual content, not
just egui rectangle bounds. Include high-DPI and long-code fixtures.

QA owns setup, data generation, launching, interaction, screenshots, and
recovery. No principal-operated regression clicks or private-session exports
are acceptance prerequisites.
