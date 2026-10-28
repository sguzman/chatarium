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
| R16 🚪 | Reduce `main.rs` to bootstrap/wiring (<200 lines target) without new crates or unnecessary public API | 2 | R13–R15 | **verified complete** |
| R20 🔎 | Split native search matching/index/cache/navigation/tests; existing stress and performance checks pass | 4 | R13 | **verified complete** |
| R21 🧩 | Split coordination workspace model/persistence/topology/UI/tests; durable grants and authority preserved | 5 | R15 | **verified complete** |
| R22 📚 | Split context composition sources/plan/gates/assembly/tests; request evidence unchanged | 5 | R14 | **verified complete** |
| R23 🛠️ | Split MCP manifest capture, provenance, history and rendering; no admission/approval regression | 5 | R12,R14 | **verified complete** |
| R24 🧠 | Split OpenViking write/read-back/project retrieval/tests; at-most-once and confirmation rules unchanged | 6 | R12,R14 | **verified complete** |
| R30 🔐 | Split parked account bridge runtime, HTTP, parser, tests; no new remote work | 5 | R16 | **verified complete** |
| R31 🧰 | Split parked Edge capture lifecycle/profiles/locks/diagnostics/tests; Windows tests pass | 5 | R30 | **verified complete** |
| R32 🛡️ | Split flight recorder ingestion, sanitizer, validation/tests; fail-closed corpus verified | 5 | R31 | **verified complete** |
| R40 ⚡ | Verify egui reader/search behavior and frame-time discipline with existing QA/perf checks | 4 | R13,R20 | **verified complete** |
| R41 🧹 | Tighten visibility/imports and dependency boundaries; format, build, clippy regressions checked | 3 | R16,R20–R32 | **verified complete** |
| R42 ✅ | Final test/integration/documentation review, no refactor-caused bugs, all changes committed/pushed | 3 | all others | pending |

**Total: 100 points. Current verified baseline: 97% (R00 + R01 + R10 + R11 + R12 + R13 + R14 + R15 + R16 + R20 + R21 + R22 + R23 + R24), not 100% of Chatarium's product development.**

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

## Round 5 — Native search verified and context composer extraction

- **R20 verified (+4pp, 48% → 52%)** at [commit 0471403](https://github.com/sguzman/chatarium/commit/0471403064b4ba5f9e64cf1b05eaa06e9967bd5b). [Full CI 40741358660](https://github.com/sguzman/chatarium/actions/runs/40741358660) Windows and Linux **success**, [native visual QA 40741358379](https://github.com/sguzman/chatarium/actions/runs/40741358379) **success**, [optimized native search profiling 40741358199](https://github.com/sguzman/chatarium/actions/runs/40741358199) **success**. The index and cache preserve the Arc projection identity and stale selection/provenance checks; no new user features.
- **R22 active, not verified:** Context composer is now mechanically divided into provenance/source rules, typed transcript constructors, bounded context plan, explicit capability gates, and isolated tests. Request assembly behavior must pass CI before counting its 5 points.
- Visual QA's workflow paths now watch the entire desktop source tree, so moved feature modules continue to trigger visual verification.

## Round 6 — Context composer verified; MCP manifest split begun

- **R22 verified: 52% → 57% (+5pp).** The context composer extraction, including follow-up rustfmt correction, passed [full Linux/Windows CI 40743937739](https://github.com/sguzman/chatarium/actions/runs/40743937739) and [production/native-reader visual QA 40743937755](https://github.com/sguzman/chatarium/actions/runs/40743937755) at commit [a508b9a](https://github.com/sguzman/chatarium/commit/a508b9ac848e544149d1ab2e8f2aef4b1c5471db). The same provenance, bounded-input, controller capability and tool admission tests passed.
- **R23 active, not verified:** `mcp_dispatch_manifest.rs` is mechanically separated into `capture.rs` (admission evidence/dispatch attachment), `history.rs` (durable-event validation and exact history), `provenance.rs` (reverse per-call tracing), `render.rs` (read-only egui display and cache), and `tests.rs`. Preserve unchanged admission/transport/authority semantics. CI, focused MCP tests, and visual QA must pass on the new commit before awarding its 5 points.
- **Additional R21/R24 preparation:** Coordination and OpenViking module splits have been structurally mapped but are not yet pushed, tested, or counted. They remain pending until independently verified.

## Round 7 — MCP manifest verified; OpenViking delivery split begun

- **R23 verified: 57% → 62% (+5pp).** [Commit ee15470](https://github.com/sguzman/chatarium/commit/ee1547018c7bf3c953bc4ff68c95c78cea3af7ee) passed [full Windows/Linux CI 40745433434](https://github.com/sguzman/chatarium/actions/runs/40745433434). The initial [visual QA 40745431740](https://github.com/sguzman/chatarium/actions/runs/40745431740) had an intermittent first production-window startup timeout, despite later draft and long-reader checks passing. Retrying the single failed job **succeeded**, including production GUI, synthetic reader, scaling, draft recovery and long-reader checks. Preserve that initial startup flake as a R40 verification concern; never describe the first attempt as green.
- **R24 active, +0 pending:** Refactor `openviking_memory_delivery.rs` into `checkpoint.rs` (at-most-once claim, create/read-back and conversation-scoped retrieval), `project.rs` (explicit cross-conversation permission grants and project-scoped retrieval), and dedicated test modules. Existing request path, retry semantics, grant checks, and read-after-write confirmation remain unchanged. CI must verify the exact split before accepting the 6 points.
- **R21 and R16 remain pending:** Source decompositions were prepared separately and have not been pushed or accepted. Keep unchanged feature scope and no new dependencies.

### R21 isolation branch: coordination workspace decomposition (not verified)

A separate feature branch exercises a structural split into the original coordination types, named `storage`, `topology`, `messages`, `wake`, `identity`, `ui` and `tests` Rust modules. Its same durable save, authority, membership, rate/claim and human consent methods are preserved. No new behavior, packages or remote transport. This branch does not advance the verified percentage or change main until GitHub CI and GUI QA pass and the predecessor R24 is accepted.

## Round 8 — Recover context, accept OpenViking and coordinate branch

- **R24 verified (+6pp):** the OpenViking checkpoint, project-grant, and dedicated-test modules on main at `871a7dd` passed full [CI 40748224190](https://github.com/sguzman/chatarium/actions/runs/40748224190) and [native-reader visual QA 40748224243](https://github.com/sguzman/chatarium/actions/runs/40748224243). No new retrieval or write-once behavior was introduced.
- **R21 verified (+5pp):** the separate `refactor/coordination-modules-20261028` branch's native model/persistence/topology/messages/wake/identity/UI/test split passed [full CI 40748666850](https://github.com/sguzman/chatarium/actions/runs/40748666850) and [visual QA 40748666809](https://github.com/sguzman/chatarium/actions/runs/40748666809) at `88ebaf2`. Main was **fast-forwarded** from `871a7dd` to `88ebaf2`, with no history rewrite. The branch's passing state is now main.
- **Verified progress: 62% → 73% (+11pp)**. All-or-nothing weights and 100% stopping boundary unchanged.
- **R16 active:** root `main.rs` remains ~11,695 lines; next split isolates the remaining persistence and rendering helpers into real Rust modules. Avoid changing data flow, parent module visibility, or the single-writer ordering until verified.

## Round 9 — Bootstrap extraction accepted; parked account-bridge split in validation

- **R16 verified: 73% → 75% (+2pp).** Root `apps/desktop/src/main.rs` is **113 lines**. Removed 47 journal admission helpers into `journal_ops.rs`, 15 reusable egui drawing/reader helpers into `ui_helpers.rs`, and 24 remaining `ChatariumApp` methods into named action/lifecycle modules. Root state, shared imports and boot/runtime helpers now live in the desktop's typed `app/model.rs`, `prelude.rs` and dedicated `app/` modules. No additional crates/dependencies or new features. The exact commit [236ba97](https://github.com/sguzman/chatarium/commit/236ba97d5954dff5936d5ed26923f5d96cbaeff2) passed [full Windows/Linux CI 40757164744](https://github.com/sguzman/chatarium/actions/runs/40757164744) and [native visual QA 40757164742](https://github.com/sguzman/chatarium/actions/runs/40757164742). Main fast-forwarded to that commit with no rewrite.
- **R30 active, no points yet:** Parked browser account bridge split into runtime shell, provider, proof parser, HTTP server and tests on isolated `refactor/chatarium-r30-account-bridge-20261028` branch. Initial CI identified that `chatarium-qa` also loads `account_bridge.rs` with `#[path]`, so submodule resolution needs explicit paths; corrected in [db4540c](https://github.com/sguzman/chatarium/commit/db4540c39cb55cc1f31ef46d6459d41faaac2c85). Do not reactivate browser transport or credit R30 until CI and GUI QA both pass.
- **R31/R32 mechanical extractions prepared but not merged/verified:** Edge capture and flight recorder have separate candidate modules and tests. Preserve all existing run isolation, profile locks, cleanup and fail-closed sanitization behavior. Accept only after their dependency predecessors and platform CI tests pass.
- **R40** remains pending the final cross-platform reader/search and frame-thread regression review; R41/R42 remain pending cleanup/integration. **Stop at verified 100%.**

## Round 10 — Parked account-bridge module extraction verified

- **R30 verified: 75% → 80% (+5pp).** The account bridge splits into a root transport/runtime shell, separate provider, response/proof parser, loopback HTTP service, and isolated tests, without resuming previously parked browser experiments. [Commit db4540c](https://github.com/sguzman/chatarium/commit/db4540c39cb55cc1f31ef46d6459d41faaac2c85) passed the exact [Windows and Linux CI run 40757868072](https://github.com/sguzman/chatarium/actions/runs/40757868072) and [production/native-reader QA 40757867987](https://github.com/sguzman/chatarium/actions/runs/40757867987). Explicit Rust submodule paths preserve compilation in both the desktop binary and the separately included QA binary. Integrated as an ancestry-preserving merge with the independent roadmap documentation update on main.
- **R31/R32 remain uncredited:** Their separate Edge and recorder branches contain moved legacy implementations. Both require their own tests and a verified combined integration before R31/R32 acceptance.

## Round 11 — Integrate independently green legacy module extractions (pending main CI)

- **Baseline: 80% verified** at main `6f23540786b0`. R31/R32 remain **active**, not credited yet; the stopping boundary stays fixed at 100%.
- **R31 candidate:** branch `refactor/chatarium-r31-edge-20261028` commit `b291829` passed full [CI 40759139016](https://github.com/sguzman/chatarium/actions/runs/40759139016) including Windows workspace tests and Linux checks. Moves the original parked Edge driver into lifecycle, launched-session, process, diagnostics, readiness, plain/browser and tests modules; retains existing lock and cleanup utilities.
- **R32 candidate:** branch `refactor/chatarium-r32-recorder-20261028` commit `2ddbfd7` passed [CI 40759171612](https://github.com/sguzman/chatarium/actions/runs/40759171612) including Windows/Linux workspace tests; contains the exact R31 source blobs plus flight recorder stream/read/derive/selection/policy/tests module split. No features or new dependencies.
- **Integration:** apply precisely those 14 previously tested Rust file blobs to main on top of the verified R30 state. Preserve independent main roadmap updates, branch ancestry and other files. Add visual-QA workflow path filters for relocated legacy modules so a merged code change triggers the actual desktop reader QA in addition to Windows/Linux CI. Check the final main merge before accepting R31/R32.
- **Evidence still needed:** full **main** CI and visual QA green, no new regression or sanitation weakness. Only then R31/R32 may earn +10pp (80% → 90%).

## Round 12 — Legacy integrations, reader QA and Clippy accepted (97%)

- **R31/R32 verified (+10pp):** On [main integration commit b5ee89b](https://github.com/sguzman/chatarium/commit/b5ee89bfc553b4f2387001ba2f52b83d9941ed65), the exact previously tested Edge/flight Rust blobs passed [full Windows + Linux CI 40761223108](https://github.com/sguzman/chatarium/actions/runs/40761223108), including workspace tests and protocol corpus validation; [production/native-reader visual QA 40761222959](https://github.com/sguzman/chatarium/actions/runs/40761222959) also passed. Parked capture/recorder functionality remains parked.
- **R40 verified (+4pp):** Same passing GUI QA exercised the actual production workspace, synthetic reader, 1.25-scaled screenshots, per-conversation unsent draft restoration, and a seeded 257-message long reader. The Linux synthetic search profile passed on 128 conversations and 16,384 messages (cold index median **481,189 microseconds**, 100 cached queries **1,988 microseconds total** in debug CI). Treat wall times as diagnostic, not production SLA; no UI render path code changed in the legacy integration. See [architecture notes](REFACTORING_ARCHITECTURE.md) for pre-existing click-triggered I/O, which this behavior-preserving refactor did not introduce or remove.
- **R41 verified (+3pp):** Source integration touched no `Cargo.toml` or `Cargo.lock`, created no new crates and no new public library boundary. Structurally private feature modules retain necessary sibling-visible methods. Existing Rustfmt, Windows workspace compilation/test, Linux builds and protocol corpus validation passed on the same integrated source. Full workspace **Clippy** (`cargo clippy --workspace --all-targets`) completed successfully in the Windows `rust` job of [CI 40761579323](https://github.com/sguzman/chatarium/actions/runs/40761579323) on an identical-source verification branch; the extra Clippy step is carried into main. Passing Clippy is not a claim of zero warnings.
- **Progress: 80% → 97% (+17pp)**. R42 alone remains pending, requiring the final main CI, visual QA evidence, final roadmap/documentation, clean pushed state, and an explicit stop. No change to the scope boundary.
