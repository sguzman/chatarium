# 🟧 Chatarium module-refactoring roadmap

**Goal (scope freeze, 2026-10-28):** Make the existing Rust workspace maintainable by splitting oversized files into coherent feature modules. This is strictly a behavior-preserving structural refactor. No new features, crates, unpausing remote/browser transport, or speculative redesign. A later "continue" means finish only necessary in-scope tasks and **stop at verified 100%**.

**Source baseline:** `main` commit `a33e9ecb25a5033db4d7e088ba927569aa86f53b`. 176 Rust files (~5.44 MB). Desktop `main.rs` ~32,650 lines / 1.60 MB, including `eframe::App::update` ~9,414 lines, `process_notices` ~1,331 lines, `persistence_worker` ~2,484 lines and ~4,354 lines of root tests. Counts are pre-extraction.

**CI baseline:** [CI run 40194763254](https://github.com/sguzman/chatarium/actions/runs/40194763254): Windows `rust` success; Linux `Test Linux desktop` failed one test, `local_stdio_runner::tests::root_owned_usr_bin_ancestry_is_required_for_trusted_launcher` (486 passed, one failed, two ignored). Earlier run [39490896498](https://github.com/sguzman/chatarium/actions/runs/39490896498) passed. This is a *pre-existing* failure; distinguish it from refactor regressions. Do not weaken security to silence a test. This connector environment has no local Cargo toolchain; only claim tests passed when verified in CI.

## Weighted milestones and tasks

Each task is all-or-nothing; **only verified-complete work earns its weight**. Status vocabulary: pending, active, blocked, verified complete. All other percentages remain unchanged when unverified.

| ID | Deliverable and objective acceptance criteria | Weight | Dependencies | Status |
|---|---|---:|---|---|
| R00 🔍 | Inventory actual source hotspots and module responsibilities at pinned SHA | 6 | — | **verified complete** |
| R01 🧪 | Audit current CI baseline, test commands and known baseline failures | 4 | R00 | **verified complete** |
| R10 🧩 | Move main's root test module to `src/tests.rs`; desktop tests pass unchanged | 4 | R01 | **verified complete** |
| R11 🖥️ | Move `eframe::App` implementation into `src/ui/`; build and reader tests pass unchanged | 6 | R01 | **verified complete** |
| R12 💾 | Move single-writer persistence command loop into `src/worker/`; tests prove journal/order/recovery unchanged | 5 | R01 | **verified complete** |
| R13 🧱 | Break `update()` into sidebar, advanced controls, coordination, header, composer and reader modules; <150 orchestration lines targeted; preserve widget IDs, focus, scroll and latency | 12 | R11 | **verified complete** |
| R14 📨 | Break `process_notices()` into feature-owned outcome handlers and a small dispatcher (~100 lines target); preserve order | 5 | R10,R12 | **verified complete** |
| R15 🧭 | Separate app actions for conversations, routing, tools, memory, workers and coordination; no new coupling | 6 | R13,R14 | **verified complete** |
| R16 🚪 | Reduce `main.rs` to bootstrap/wiring (<200 lines target) without new crates or unnecessary public API | 2 | R13–R15 | pending |
| R20 🔎 | Split native search matching/index/cache/navigation/tests; existing stress and performance checks pass | 4 | R13 | **active** |
| R21 🧩 | Split coordination workspace model/persistence/topology/UI/tests; durable grants and authority preserved | 5 | R15 | pending |
| R22 📚 | Split context composition sources/plan/gates/assembly/tests; request evidence unchanged | 5 | R14 | pending |
| R23 🛠️ | Split MCP manifest capture, provenance, history and rendering; no admission/approval regression | 5 | R12,R14 | pending |
| R24 🧠 | Split OpenViking write/read-back/project retrieval/tests; at-most-once and confirmation rules unchanged | 6 | R12,R14 | pending |
| R30 🔐 | Split parked account bridge runtime, HTTP, parser, tests; no new remote work | 5 | R16 | pending |
| R31 🧰 | Split parked Edge capture lifecycle/profiles/locks/diagnostics/tests; Windows tests pass | 5 | R30 | pending |
| R32 🛡️ | Split flight recorder ingestion, sanitizer, validation/tests; fail-closed corpus verified | 5 | R31 | pending |
| R40 ⚡ | Verify egui reader/search behavior and frame-time discipline with existing QA/perf checks | 4 | R13,R20 | pending |
| R41 🧹 | Tighten visibility/imports and dependency boundaries; format, build, clippy regressions checked | 3 | R16,R20–R32 | pending |
| R42 ✅ | Final test/integration/documentation review, no refactor-caused bugs, all changes committed/pushed | 3 | all others | pending |

**Total: 100 points. Current verified baseline: 48% (R00 + R01 + R10 + R11 + R12 + R13 + R14 + R15), not 100% of Chatarium's product development.**

## Execution and validation contract

1. Before each round inspect current HEAD and relevant changed files; preserve unrelated changes.
2. Use small, coherent, mechanical Rust module extractions. No feature or runtime behavior changes; no unnecessary new dependencies. Preserve original entry points, privacy and module boundaries.
3. For destructive whole-file edits check exact section boundaries, source/target sizes and sentinels; write atomically; read back after push and verify. Never assume write success means content survived.
4. Maintain a **single journal writer** and all existing safety invariants: user drafts committed before sending; incremental assistant durability; uncertain remote outcomes; idempotent recovery; explicit tool/user approval; isolated conversations; bounded, at-most-once OpenViking persistence. Keep network/I/O and expensive computation *off egui's frame thread*.
5. Use the existing CI checks: `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --workspace`, targeted Linux desktop/reader/search/MCP checks and Windows capture tests as applicable. Distinguish baseline failures from new regressions; never claim a build ran if it did not.
6. Commit/push verified slices. Keep roadmap statuses and objective evidence current. At each round report: visual overall bar, current %, previous %, delta in percentage points, current milestone %, completed tasks, CI/tests, commit(s), blockers and next legitimate work.
7. **Stop boundary:** Stop making changes at verified 100%. If asked to "continue" afterward, do nothing unless user explicitly expands scope. Never manufacture tasks or move the finish line.

## Round 1 — 2026-10-28

- **10% verified:** R00 and R01 reconnaissance/CI baseline.
- **Implementation prepared, verification pending:** R10 root tests, R11 egui App trait impl, R12 persistence command loop extracted as unchanged code into real modules. Do *not* add their 15 percentage points until the corresponding CI evidence passes.
- **Known blocker:** existing Linux sandbox ownership test; separate from module extraction.
- **Next:** Validate the extracted modules through CI and then split `update()` by UI responsibility and `process_notices()` by feature. Preserve paused transport code without exercising it.

### CI observation — relocated header in native QA

Following the first extraction, the long-reader GUI smoke failed with `IndexError`: it searched `main.rs` for the `conversation_header` height, which now resides in `src/ui/`. The QA script was updated to search the UI module tree and require one matching header. The separate synthetic reader smoke reported `Reader QA process ended`, cause still unverified. These failures do **not** justify marking R11/R40 verified before a rerun. Preserve the baseline Linux sandbox ownership failure separately.

## Round 2 — 2026-10-28

- **19% verified (+9pp from prior round's 10%):** R10 (root unit tests) and R12 (single-writer persistence-loop extraction) accepted with [run 40713023585](https://github.com/sguzman/chatarium/actions/runs/40713023585): Windows `rust` complete success including format, check, corpus and workspace tests. Linux workspace compilation passed and the same historical isolated sandbox launcher test failed: 486 passed, one failed, two ignored. No new failure has been observed in that test run.
- **R11 remains active:** [visual run 40713023575](https://github.com/sguzman/chatarium/actions/runs/40713023575) built production GUI and exercised normal conversation switching/draft recovery, but the synthetic reader process ended unexpectedly. Root cause not yet established; keep full reader acceptance open.
- **R13 active (not yet verified):** extract native conversation header renderer from the 9,414-line frame method into `ui/header.rs`, preserving the same panel ID, height, execution ordering and action callback behavior.
- **R14 active (not yet verified):** move the ordered journal notice dispatch into `app/notices.rs` without altering match arms. Further functional decomposition into smaller handlers is still outstanding.
- **Verification plan:** Latest Windows and Linux CI, native GUI smoke, synthetic reader QA; preserve the pre-existing sandbox failure classification; no refactor extraction earns points until accepted.

### Second desktop/UI extraction slice — awaiting CI

The 183-line conversation header is already extracted into `ui/header.rs`; this slice also extracts the ~297-line native composer panel into `ui/composer.rs`. Refactoring moves the same rendering/actions behind a typed method, and returns the two existing mirror/sync requests through mutable references. No active remote history transport is enabled. R13 remains active and receives no percentage credit until `update()` has been fully decomposed and verified.

The `app/notices.rs` module currently contains the original ordered ~1,331-line notice handler. This is organization only; R14 still requires actual responsibility-specific handler decomposition. An existing synthetic reader QA process exits early without diagnostic output, so the QA harness will report its exit status on failure; this improves evidence, not product functionality.

### Additional structural extraction slice — awaiting CI

Moved 53 method implementations from the root `ChatariumApp` implementation to four feature modules in `src/app/actions/`: `routing.rs`, `tools.rs`, `memory.rs` and `workers.rs`. Methods are `pub(crate)` during transitional reorganization so existing sibling egui modules can invoke them; R41 must review visibility after call sites settle. No journal/policy/transport implementation was rewritten. Also moved the coordination pop-up and navigation logic into `src/ui/coordination_window.rs`. R13 and R15 remain **active** until the rest of their accepted scope and verification passes. This changes no completion percentage.

### CI runner environment regression investigation

The last fully green visual QA run [39490896288](https://github.com/sguzman/chatarium/actions/runs/39490896288) used the `ubuntu-24.04` image. Recent `ubuntu-latest` runs resolved to `ubuntu-26.04`. The synthetic reader exits normally with status 0 during visual QA on the changed runner, rather than crashing or panicking; the single pre-existing Linux sandbox trust test also fails there. To reestablish the known test environment while the refactor is underway, **pin the Linux desktop CI job and native reader visual QA job to `ubuntu-24.04`**. This is *not* proof that the runner change is the sole cause until the pinned jobs are rerun. Do not relax host-path ownership checks.

Five line-wrap changes were applied verbatim from the latest Windows rustfmt output to extracted action methods. The actions and GUI extraction remain under CI verification.

### Runner-pinned acceptance and further decomposition — 2026-10-28

**R11 verified (+6 percentage points):** [visual run 40716101349](https://github.com/sguzman/chatarium/actions/runs/40716101349) completed with **success** on pinned Ubuntu 24.04. [CI run 40716101369](https://github.com/sguzman/chatarium/actions/runs/40716101369) has Windows `rust` success and Linux desktop compilation, desktop tests, local-first integration journey, and stdio-safety tests passed; remaining Linux isolation installation/stages were still running at this checkpoint. Prior screenshot failures occurred on `ubuntu-latest` resolving to Ubuntu 26.04 and were not accepted as refactor correctness evidence. **Verified roadmap total: 25%.**

**R13 remains active:** Move the native transcript reader/projection/virtualized scroll path from `ui/mod.rs` to `ui/reader.rs` without a semantic change, leaving the repaint footer in `update()`.

**R14 implementation prepared, verification pending:** Refactor the 1,331-line `process_notices()` into a ~100-line dispatcher and nine feature-owned completion handlers (OpenViking, durable draft/turn, remote, routing, MCP proposals, MCP outcomes, memory, workers, failure cleanup). The original dispatcher has no post-match work per notice. Its nine outer-loop `continue;` statements therefore become handler `return;` statements when extracted, preserving skip-to-next-notice behavior. All notice variants remain handled, with the original processing order and security decisions unchanged. No R14 points until CI verifies the extraction.

### CI follow-up: module-aware visual-QA triggering

The first nine-handler notice split reached Windows `cargo fmt -- --check` and produced 19 formatting-only diffs; the output of rustfmt was applied to the corresponding extracted modules. The native-reader visual-QA workflow's path filter now includes `apps/desktop/src/app/**` and `apps/desktop/src/ui/**` to ensure the freshly relocated code always triggers screenshot testing. R14 still needs CI verification; R13 remains partially implemented. No additional percentage points are counted at this stage.

## Recovered continuation — 2026-10-28

**R14 verified: 25% → 30% (+5 percentage points)**. Exact commit [3955c4f](https://github.com/sguzman/chatarium/commit/3955c4f7f413c53d8babcceb50f0aae9af677784) has [CI run 40718373736](https://github.com/sguzman/chatarium/actions/runs/40718373736) fully **success** (Linux and Windows), and [native reader visual-QA 40718373753](https://github.com/sguzman/chatarium/actions/runs/40718373753) fully **success**. The feature-owned dispatcher is ~101 lines, and all 57 PersistNotice variants are represented in the nine handlers; existing test and GUI coverage passed.

**R15 active (verification pending):** Extract 36 additional existing `ChatariumApp` methods into conversation, draft/composer, coordination wake, and inference/settings action modules. This complements the existing routing, tools, memory and worker action modules. No functional changes, no new dependencies, no paused remote-history track activation. Keep progress at 30% until this slice is verified.

**R13 ongoing:** The root frame update still contains an enormous sidebar surface. An extraction into `ui/sidebar.rs` is prepared separately and must be independently validated after R15; do not count it as done yet.

### CI parse regression — recovered continuation

The first action-extraction CI [run 40719854721](https://github.com/sguzman/chatarium/actions/runs/40719854721) failed Windows `cargo fmt --check` because a method doc comment was orphaned at the end of `app/actions/composer.rs`. A second orphaned comment existed at the end of `app/actions/coordination.rs`. Both comments were restored to the next methods they document (latest coordination wake status and OpenViking query observation). This is a mechanical correction of extraction boundaries; R15 stays active pending CI/build/test confirmation. No additional percentage points awarded.

## Round 3 — 2026-10-28 (recovery)

- **R15 verified (+6pp; 30% → 36%):** [commit b595a25](https://github.com/sguzman/chatarium/commit/b595a253ca6d3e9e754e32096b45727946ee4fbf) passed [full Windows/Linux CI 40720847862](https://github.com/sguzman/chatarium/actions/runs/40720847862) and [native reader visual QA 40720847812](https://github.com/sguzman/chatarium/actions/runs/40720847812). Conversation, composer, coordination, inference, routing, MCP tools, memory and worker action methods now live in focused modules. Original journal ordering and approval gates remain tested.
- **R13 active:** Mechanical extraction of ~7,889 lines (sidebar navigation, search, advanced controls and post-selection actions) from `ui/mod.rs` into `ui/sidebar.rs`. The frame calls `render_sidebar` once and receives a typed `SidebarOutcome`. Search, ID salts, callbacks, and action sequencing stay inside the extracted method. The sidebar's advanced subsections must still be split; `update()` must be reduced to ~150 lines, and CI + visual QA must pass before R13 receives its 12 points.
- **Verification pending for this extraction:** No points claimed for R13 until the exact commit builds and passes regression/visual checks.

### Recovery continuation — extracted frame orchestration (verification pending)

Starting from verified HEAD `4fb591a73b9eb184afca194c098f6cbcdc3c8c97` with full [CI 40722800209](https://github.com/sguzman/chatarium/actions/runs/40722800209) and [visual QA 40722800270](https://github.com/sguzman/chatarium/actions/runs/40722800270) passing, the remaining ~235-line egui `update()` implementation was mechanically reduced to ~74 lines. Frame polling, native keyboard navigation, and ordered panel/action orchestration were extracted to `ui/frame_lifecycle.rs`, `ui/navigation.rs` and `ui/panels.rs`. The same calls and their order are retained; this alone is **not** sufficient to complete R13, because the 7,900-line sidebar's advanced areas still require modularization. Verify the exact extraction with Windows/Linux CI and visual QA before adding percentage points.

### Recovery continuation: advanced sidebar decomposition (pending CI)

The frame `update()` was reduced from ~235 to ~74 lines in `ui/mod.rs` by moving polling, keyboard navigation and ordered panel dispatch to three small modules. Its first CI run reached the format gate, which reported only module declaration order and a method signature wrap; these were corrected. The 7,946-line `ui/sidebar.rs` was also split into a ~2,076-line sidebar and a separately owned advanced control module. Seven cohesive advanced subpanels were extracted without changing render order or widget salts: routing directory, routed inbox, local memory, MCP tools, worker inbox, admitted MCP evidence and exact request context. The advanced parent is now ~2,439 lines. This is substantial R13 implementation but **R13 remains active and earns +0 until its acceptance tests pass**, including the sidebar navigation, synthetic reader, and Windows/Linux checks. Remaining multi-thousand-line sections will be split in further bounded slices; no feature changes.

### R13 advanced panel split — extended (CI pending)

The large extracted advanced panel was further decomposed into independent modules for worker lifecycle (~316 lines) and controller supervision (~1,729 lines), alongside existing routed inbox, routing directory, local memory, MCP tools, worker inbox, admitted MCP evidence and exact request context. The advanced parent now has ~416 lines; `ui/sidebar.rs` has ~2,076 lines; `eframe::App::update` is ~74 lines. These are mechanical moves of existing egui closures with the same render order, permission gates, and widget identifiers. Only after full CI and native reader/production visual QA for this exact state passes should R13 be considered verified. Baseline progress remains 36% pending that evidence.

## Round 4 — UI acceptance and native search (2026-10-28)

- **R13 verified (+12 percentage points, 36% → 48%):** [commit 88fd0db](https://github.com/sguzman/chatarium/commit/88fd0db3a3afdfc557ceee9172fbbe7e7aa546d3) passed [full Windows and Linux CI 40739603354](https://github.com/sguzman/chatarium/actions/runs/40739603354) and [production/native reader visual QA 40739603363](https://github.com/sguzman/chatarium/actions/runs/40739603363). `eframe::App::update` is ~74 lines. The sidebar, advanced panels, reader, composer, coordination, header and frame lifecycle/navigation have named modules; existing behavior and egui IDs are covered by the verified suites.
- **R20 active, no credit yet:** Extract `native_conversation_search.rs` into dedicated matching, identity/reader navigation, sidebar/reader caches, incremental typed journal index, and tests. The parent becomes a small reexport/types façade. Preserve ownership boundaries, cached Arc identity and index invalidation; run Windows/Linux CI and native reader search/stress/visual QA before accepting the 4 points.
- **R16 and other roadmap items remain pending:** No bootstrap or unrelated service modifications in this slice; no new Cargo dependencies.
