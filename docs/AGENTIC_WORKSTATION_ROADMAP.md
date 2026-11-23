# 🟧 Agentic workstation — resumption roadmap

**Decision, 2026-11-23:** Prioritize Chatarium as a local-first, user-controlled agentic workstation: native conversations, local tools, persistent memory, and supervised multi-agent project work. Website feature parity is not the immediate development priority. Existing remote-history and native-browser-transport research remains paused; the long-term transport contract is preserved, not silently superseded.

**Audit baseline:** main at `af4b6c21a9a99de233ebbea508b32086bf80a0db` (2026-10-29 UTC). The original structural refactoring roadmap is 100% accepted and is not reopened. The last recorded [CI](https://github.com/sguzman/chatarium/actions/runs/40800057918), [native visual QA](https://github.com/sguzman/chatarium/actions/runs/40800057998), and [release search profile](https://github.com/sguzman/chatarium/actions/runs/40800058057) passed. This does not establish current live account, service, or workstation compatibility.

## Bounded acceptance roadmap

Weights measure completion of this resumption scope, not the overall product. A phase earns its weight only when its listed acceptance evidence is recorded. No feature expansion is implied by completion.

| ID | Phase | Weight | Status | Acceptance |
| --- | --- | ---: | --- | --- |
| A | Reproducible Nix environment | 15 | Pending | Locked flake, crate2nix Cargo dependency build, pinned Rust and DevKit/Node runtime, documented native runtime dependencies, clean build and synthetic launch; no startup package installation in the packaged path |
| B | Current inference compatibility | 20 | Pending | Inspect pinned/upstream DevKit delta; verify offline bridge regressions before any upgrade; real authorized account model discovery, text streaming, cancellation and continuation; capability evidence scoped to actual model/profile, no invented support |
| C | Complete Q30 read-side acceptance | 25 | Pending | Inventory active render/read surfaces, move costly replay off the frame thread, preserve exact effective journal revisions and ownership; measure durable-writer latency during read load; synthetic quarantine, stale reply and large archive tests; document any remaining parked surfaces |
| D | Targeted module decomposition | 10 | Pending | Split journal operations by domain, worker command handlers, large tests and UI panels as justified by ownership; combine UI changes with C; preserve public behavior, durable ordering and existing gates |
| E | Live OpenViking acceptance | 15 | Pending | Authorized normal-turn checkpoint, exact read-back confirmation, restart and verified recall; no duplicate uncertain writes or cross-conversation leakage; use disposable test content |
| F | Tool trust boundaries | 10 | Pending | Review current automatic host-shell and fixed-provider defaults, distinguish trusted development from untrusted content contexts, test approval and explicit policy preservation; no silently broadened authority or unilateral policy migration |
| G | Workstation acceptance and next-scope decision | 5 | Pending | One bounded, supervised project-work journey through native conversation, tools, context/memory and controller/worker lifecycle; verified completion/blocked states, durable recovery; record remaining limitations and stop for an explicit next goal |

**Baseline: 0/100 verified points for this new scope.** Earlier CI and structural milestones remain valid historical evidence, not newly earned points.

## Execution order

Start A, then establish B's live viability as early as practicable. Review F's permissions before enabling live tool execution; use text-only or isolated synthetic tests until the intended authority is confirmed. C and D should be coordinated so render separation does not create another redundant refactor. E depends on working inference and an authorized reachable local OpenViking service. G is the final gate, not a license to add new orchestration features.

### Findings to carry forward

- Desktop entry point is 113 lines; no further extraction is justified there.
- Large remaining modules include `tests.rs` (4,444 lines), `journal_ops.rs` (4,010), `worker/mod.rs` (2,524), `ui/sidebar.rs` (2,093), and advanced controller/tools panels (1,634/1,419). These counts are audit-baseline observations, not targets in themselves.
- Active advanced UI panels still perform synchronous audit replay. Native search and inline MCP projections now execute on the persistence worker; measure potential writer contention before choosing the read-projection design.
- Historical account-archive search is gated by the paused mirroring flag. Do not reactivate it to finish the active local-first scope.
- Effective journal and SQLite views preserve quarantined raw evidence, original sequences, and fail-closed validation. Complete active read-surface acceptance without deleting raw evidence.
- DevKit baseline `f723814abdccec135b519c451fb6e1992ee5e933` is one commit behind the audited upstream head `0a36fefeb913055c8c7a1a29b63d82b96b2e841a`. The change moves Responses streaming to the OpenAI Node SDK; inspect retry, redirect, cancellation and capability-probe interception behavior before upgrading.
- Native host shell is enabled and automatic by default; it executes with the invoking user's privileges. The blacklist is not a sandbox. Treat this as a trust-boundary decision, not a request to remove legitimate development tools.
- The repository has no Nix flake at the baseline. Use crate2nix, not Crane; retain the upstream lockfile semantics for the Node sidecar. Avoid introducing npm as a user-facing setup requirement.

## Change and verification contract

Preserve all existing AGENTS.md reliability, consent, provenance, isolation and single-writer invariants. Do not launch personal browser profiles, reopen the paused transport track, expose private data in Git/CI, or reinterpret ambiguous remote/tool execution as retryable. No user journal, credentials or actual workstation configuration are needed for synthetic CI. Account login and live service tests require genuine authorized access; report blockers rather than fabricate evidence. Routine regression testing remains automation-owned.

Record exact commits, test runs, scoped completion evidence and remaining blockers. Report verified percentage and delta only. At 100%, stop; new product work requires an explicit scope decision.

## Related specifications

- [Product vision](PRODUCT_VISION.md)
- [Local-first exploration](LOCAL_FIRST_EXPLORATION.md)
- [Completed refactoring roadmap](REFACTORING_ROADMAP.md)
- [Local archive integrity / Q30](LOCAL_ARCHIVE_INTEGRITY.md)
- [Inference capability surface](LOCAL_INFERENCE_CAPABILITY_SURFACE.md)
- [OpenViking live acceptance](OPENVIKING_FIRST_LIVE_QA.md)
- [Host shell MCP](HOST_SHELL_MCP.md)
