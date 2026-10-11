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
X11 automation captures sixteen PNG screenshots per scale: top, middle,
keyboard PageDown and PageUp (through the same scroll-state helper used
by the production workspace), wheel scroll, end, long-distance search jump, automatic hit recovery after resizing and after a long earlier-message edit
without reissuing Search,
synthetic append, switching away, switching back, and restoration of an
independent mid-archive reading bookmark, and non-disruptive background
tail append followed by explicit end navigation. The append marker
survives the synthetic conversation switch. The shell preserves a
separate measured virtual layout and saved offset for each conversation,
just as the native workspace does, and verifies returned visibility again
after paint/reflow stabilization. Overscan layout membership
alone does not prove a message is on screen: the visual harness requires the
appended bubble's production-renderer rectangle to intersect the scroll clip
by at least 48 logical pixels. The same rule now applies to the distant search
target, both before and after resizing; screenshot checking rejects states where
only an overscanned offscreen row was laid out. This prevents a laid-out,
offscreen row from being misreported as visible. It checks for nonblank
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
Remaining work includes exercising conversation switching and user-authored
messages inside the ordinary ChatariumApp (rather than this isolated shell),
reviewed typographic screenshot comparisons and tests in the full
production workspace.

## X11 desktop isolation guard

Every GUI QA harness now fails closed unless the caller explicitly enables
`CHATARIUM_QA_ISOLATED_XVFB=1` **and** the current local `DISPLAY` matches
a live, local `Xvfb` process. The test aborts before starting Openbox,
creating fixtures, or injecting any mouse/keyboard events otherwise. This
guards the user's Hyprland/XWayland desktop against accidental QA automation.
The dedicated GitHub Actions runner opts in only while its disposable Xvfb
server supplies the display. Remote display connections are never supported.
The guard was independently tested against missing opt-in, a nonexistent
Xvfb display, and a live isolated Xvfb session.

## Isolated actual-workspace GUI smoke

In addition to the synthetic virtual-reader shell, the visual QA workflow now
launches the **actual ChatariumApp** with a fresh, empty `HOME`,
`XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME`, and
`CHATARIUM_DATA_DIR`. Environment variables are allowlisted via `env -i`;
existing credentials, ordinary journals, and the user's browser profile are
not visible to this launch. There is no automated sign-in.

Xvfb/Openbox captures a real production-window screenshot and commits
a synthetic message through the actual composer, waiting for exactly one
durable `UserMessageCommitted` event rather than merely a draft save. It
then creates another local conversation through `Ctrl+Shift+N`, commits a
different synthetic message, and verifies that the typed journal events carry
**distinct conversation owners**. `Ctrl+Shift+Tab` and `Ctrl+Tab` switch
back and forth while production screenshots verify that the active transcript
changes. A final resize checks layout reflow. The seven production screenshots
and machine-readable summary join the seven-day CI artifact. This validates
production startup, durable authored-message isolation, native conversation
navigation, and basic layout reflow; it does **not** yet validate production
continuous-reader navigation through a long persisted transcript.

## Actual-app restart and durable replay

Before composing the second message, QA compares the production screenshot
with the freshly launched empty-chat view. It aligns the actual title
and central empty-state prompt independently within 45 horizontal pixels:
the sidebar may grow when conversations appear, even though the selected
content is identical. Both regions must match, so a missed shortcut cannot
be mistaken for a legitimate layout shift. If Ctrl+Shift+N is lost to focus,
QA retries using the actual sidebar New local button and verifies the
empty view again. No second message is typed into the first conversation
under a missed-shortcut condition.

The global Ctrl+Tab / Ctrl+Shift+Tab and Ctrl+Shift+N handlers consume
modifiers recorded on each keypress event rather than the frame-end held-key
snapshot. Fast keypress-and-release sequences otherwise lose their modifiers
before egui evaluates the event. Headless tests reproduce this released-chord
case and confirm ordinary Tab and Alt-modified keys remain untouched.

Keyboard conversation navigation now requires a materially different actual
painted transcript region, not merely an injected X11 key. The harness
re-establishes window focus and retries a missed Ctrl+Tab/Ctrl+Shift+Tab
chord up to four times, failing rather than clicking a sidebar fallback.
This guards against transient X11 focus loss while retaining keyboard
navigation as the behavior under test.

The isolated production GUI smoke now stops the native process after
committing messages in two independently owned conversations, then launches
a **new Chatarium process** using the same disposable HOME and journal.
The QA helper parses every JSONL event (rejecting truncated records) and
verifies both typed `UserMessageCommitted` events survive with their exact
conversation owners, without duplicated commits. The automated screenshots
also compare the first and second conversation's actual painted title/message
region before and after restart and require both to be reconstructed from
the durable journal, independent of which conversation the application
initially selects. This test uses screenshot similarity, not OCR, and never
accesses the user's normal HOME, credentials, or journal.

Production shortcut failure reports now include a strictly opt-in,
isolated-fixture trace of egui key-event names and modifier flags (no text,
message bodies, cookies or account state). This distinguishes a missing key
event from a rejected modifier chord without touching personal data.
The long-archive production test now runs independently after a successful
build, even if the earlier two-conversation GUI smoke fails; its result
is collected in the same CI run instead of being skipped.

The first independent 257-message production run exposed an egui NaN layout
panic when switching to Continuous reader after End navigation. Its cause is
an obsolete `f32::MAX` ScrollArea offset in the mode-toggle and restore
paths. All native tail/restore offsets now use the same bounded finite value
already established by the Home/End regression fix. The disposable app also
runs with `RUST_BACKTRACE=1` so future native layout panics retain actionable
stack traces. Keyboard QA additionally records boolean-only activation guard
and result evidence after a shortcut is consumed.

## Isolated long-archive production GUI QA

The dedicated `chatarium-reader-seed` test executable writes exactly 257
typed user-message commits into a new, named, disposable QA journal using
Chatarium's own `commit_user_message` storage API. It refuses existing
journals and paths outside `.qa-native-long-reader/data/journal.jsonl`. One
conversation owns every message, including distinct first/last markers,
multilingual text, and code/long-wrapping fixtures. No normal journal or
account data is opened.

A separate Xvfb test launches the **actual ChatariumApp** with that archive.
It captures startup, then establishes a settled tail reference using an explicit
End key. Home must leave that reference and a second End must restore it.
Screenshot comparison independently aligns the transcript to accommodate small
sidebar width shifts; this rejects the earlier regression where Home stayed at
the tail while End unexpectedly jumped to the first message.

The same disposable production run then clicks the actual Continuous reader
checkbox through X11 (without changing the default), checks that the control
paint changed, and captures separate Home/End screenshots from the virtualized
reader. The journal remains read-only throughout navigation; no private data or
credentials are available to the test.

## ScrollArea ID correctness

egui scopes a ScrollArea's stored state to its parent Ui. It first hashes
the argument to `ScrollArea::id_salt` into an `Id` in egui 0.33.3,
then uses `ui.make_persistent_id(egui::Id::new(scroll_salt))` as its state key.
Neither a raw `Id::new(...)` nor a directly parent-scoped raw ID matches. Chatarium computes the
parent-scoped ID inside the reader's owning UI, and uses it for PageDown/Up,
Home/End, paged-reader resets, and post-frame scroll-anchor corrections.

The headless egui test checks the distinction directly, then asserts that a
PageDown-style state update reaches the actual ScrollArea. Native screenshot
QA exercises the same production helper via X11 keyboard events.

## Linux Unicode font fallback

Native Chatarium now registers locally installed Noto CJK, Arabic and Hebrew
faces as **last-resort glyph fallbacks** in egui's proportional and monospace
families. The original Latin fonts retain priority. No font binaries are
vendored; there are no font downloads, subprocesses, or external network calls
at startup. Linux packages such as `noto-fonts`/`noto-fonts-cjk` on Arch and
`fonts-noto-core`/`fonts-noto-cjk` on Ubuntu supply supported paths. Missing
fonts are optional and never block launch.

The synthetic visual-QA journal reports registered fallback names without
exposing font bytes or filesystem paths. CI installs and checks the Noto Arabic
and CJK fixtures, then asserts that both fallbacks were registered before any
screenshots pass. A highlighted multilingual search target makes missing-glyph
problems visible in captured images. Hebrew is optional but exercised when
its font is installed.

## Human-readable conversation timestamps

Native conversation bubbles format journal Unix-second timestamps into
`YYYY-MM-DD HH:MM UTC` instead of displaying a raw Unix number with an
incorrect `s` duration suffix. UTC is explicit rather than pretending to
know the reader's local timezone. An invalid or unsupported timestamp falls
back to the existing provenance/event-label display; it never produces a
nonsensical date. Pure-Rust date-conversion unit tests include the epoch,
leap-day boundaries, fractional seconds and invalid values.

## Promotion criteria

Do **not** enable Continuous reader by default based on performance timings
alone. Before promotion, automated native UI QA must exercise first/last,
search jump, wheel/page scroll, resize, appending, and conversation switching
in a real desktop session; capture screenshots and compare visual content, not
just egui rectangle bounds. Keep long-code and production-app fixture coverage before promotion.

QA owns setup, data generation, launching, interaction, screenshots, and
recovery. No principal-operated regression clicks or private-session exports
are acceptance prerequisites.
