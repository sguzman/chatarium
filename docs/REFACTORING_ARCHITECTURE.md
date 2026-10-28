# Chatarium refactor — module map and verification boundaries

**Scope:** structural Rust workspace decomposition begun 2026-10-28. No feature additions, new Cargo crates/dependencies, browser-transport reactivation, journal-authority changes, or permission loosening. See [the weighted roadmap](REFACTORING_ROADMAP.md) for acceptance evidence and scope stop.

## Where the code lives now

| Area | Ownership |
| --- | --- |
| `apps/desktop/src/main.rs` | Minimal application entry point, currently **113 lines**; no longer owns the egui frame or durable journal workers |
| `apps/desktop/src/app/model.rs`, `prelude.rs`, `app/bootstrap.rs` | Shared app model, initialization and dependency wiring |
| `apps/desktop/src/app/actions/` | Named user-command methods: conversations/catalog, composer, coordination, inference, routing, tools, worker lifecycle, memory, OpenViking, mirror |
| `apps/desktop/src/app/notices/` | Feature-specific outcomes behind a small ordered dispatcher; durable commit, memory, OpenViking, remote, routing, tool and worker notices |
| `apps/desktop/src/worker/`, `journal_ops.rs` | Single-writer persistence loop and checked journal/event admission helpers; preserve operation order and at-most-once gates |
| `apps/desktop/src/ui/` | egui frame orchestration, header, composer, reader, coordinator window, navigation, sidebar and nested advanced controls |
| `apps/desktop/src/native_conversation_search/` | Search/projection/index/cache and separate stress, performance, unit tests |
| `apps/desktop/src/context_composer/`, `mcp_dispatch_manifest/`, `openviking_memory_delivery/` | Context planning, MCP dispatch evidence/provenance, and permission-scoped memory delivery |
| `apps/desktop/src/coordination_workspace/` | Human-authorized coordination workspace model, storage, topology, wake, messages, identities and UI |
| `tools/capture/src/edge/` | Parked legacy Edge capture process/lifecycle/readiness/diagnostics, with tests and existing Windows-specific helper files |
| `tools/recorder/src/flight/` | Legacy flight ingestion, selection, sanitization/policy, derivation, stream and tests |
| `crates/core`, `crates/protocol`, `crates/store` | Existing shared domain, wire-format and durable-storage boundaries, unchanged as crates |

## Boundaries that must remain intact

- **One durable writer:** user draft/message persistence precedes dispatch. Never restart possibly dispatched tools or resume ambiguous remote requests speculatively.
- **Consent and evidence:** preserve explicit user approvals, actual recorded source/provenance, project memory sharing grants, read-after-write confirmation and at-most-once OpenViking operations.
- **No frame-thread heavy processing:** egui frame composition must reuse existing caches and delegate expensive work. This restructuring moves code, not work ownership. Some pre-existing UI-triggered local reads/saves (including reader position persistence) remain; the structural verification establishes no new frame-thread workload, **not** a guarantee of zero filesystem I/O or measured production frame-latency percentiles.
- **Paused paths remain paused:** tests for capture/recorder prove unchanged behavior; they do not authorize launching a browser, accessing personal profiles, or resuming experimental remote mirroring.

## Validation and interpretation

Acceptance uses existing `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --workspace`, protocol corpus validation, Linux sandbox/stdio integration, Windows tests, a full native visual-QA run (real production desktop, virtual reader, non-default scaling, draft recovery and long archive), and a synthetic native search profile. A workspace Clippy audit exercises `cargo clippy --workspace --all-targets` without suppressing warnings.

Synthetic performance numbers are useful regression diagnostics, **not** a wall-clock service-level guarantee; CI hardware varies. Final acceptance records the exact green commit/run IDs in the roadmap. A later `continue` past 100% does not expand this refactor's scope.
