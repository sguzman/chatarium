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
| R10 🧩 | Move main's root test module to `src/tests.rs`; desktop tests pass unchanged | 4 | R01 | **active** |
| R11 🖥️ | Move `eframe::App` implementation into `src/ui/`; build and reader tests pass unchanged | 6 | R01 | **active** |
| R12 💾 | Move single-writer persistence command loop into `src/worker/`; tests prove journal/order/recovery unchanged | 5 | R01 | **active** |
| R13 🧱 | Break `update()` into sidebar, advanced controls, coordination, header, composer and reader modules; <150 orchestration lines targeted; preserve widget IDs, focus, scroll and latency | 12 | R11 | pending |
| R14 📨 | Break `process_notices()` into feature-owned outcome handlers and a small dispatcher (~100 lines target); preserve order | 5 | R10,R12 | pending |
| R15 🧭 | Separate app actions for conversations, routing, tools, memory, workers and coordination; no new coupling | 6 | R13,R14 | pending |
| R16 🚪 | Reduce `main.rs` to bootstrap/wiring (<200 lines target) without new crates or unnecessary public API | 2 | R13–R15 | pending |
| R20 🔎 | Split native search matching/index/cache/navigation/tests; existing stress and performance checks pass | 4 | R13 | pending |
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

**Total: 100 points. Current verified baseline: 10% (R00 + R01), not 100% of Chatarium's product development.**

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
