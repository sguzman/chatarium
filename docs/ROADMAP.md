# Roadmap

Chatarium is ordered by reliability value, not by visual completeness.

## P0 — stop losing work

The immediate objective is a browser-side flight recorder that can be deployed before the native client is ready.

Deliverables:

- autosave composer mutations locally;
- commit the exact outgoing user text before submit;
- retain conversation identity/URL with each local record;
- snapshot assistant output incrementally;
- journal reloads, disconnects, visible errors, and completion observations;
- export/recover a conversation-independent local transcript;
- never store reusable authentication secrets in exported evidence.

Exit criterion: a browser/network failure may interrupt a turn, but it cannot erase text already authored or already observed on the machine.

## P0.5 — establish the protocol baseline

Create controlled observation sets of the official ChatGPT web client while minimizing human QA.

The protocol baseline now has two complementary empirical snapshots:

- `2026-09-29.001`: manual Edge/Linux HAR of a completed text turn, establishing prepare/send/token-linkage/persisted-state and websocket completion surfaces;
- `2026-09-29.002`: canonical C03 captured by Flight Recorder, establishing the actual `text/event-stream` body, `delta_encoding = "v1"`, incremental message operations, explicit completion patching, `message_stream_complete`, and terminal `[DONE]`.

Flight Recorder v0.6.0 then passed live protocol-backed reconciliation: a parsed user `input_message` confirmed the pending send under the canonical conversation identity and the final-channel assistant stream populated durable assistant state without depending on current DOM selectors.

Manual/browser-local evidence remains acceptable when it produces protocol knowledge faster and more safely than finishing legacy capture automation first.

The legacy Windows CDP harness remains documented in `docs/CAPTURE_HARNESS.md`, but it is no longer a prerequisite for protocol progress. The active cross-platform path is:

```text
Flight Recorder export
        ↓
chatarium-recorder snapshot-flight
        ↓
sanitized selected-run evidence + structural inventory
        ↓
protocol snapshot / diff / typed interpretation
```

Longer-term automation should preserve the useful harness properties:

- dedicated Chatarium Edge profile with one-time normal login;
- read-only diagnostics (`chatarium-capture doctor`);
- automated canonical experiment execution;
- incremental private capture journal;
- reusable centralized sanitization;
- one portable sanitized artifact per run;
- explicit ambiguous-outcome semantics;
- no automatic retry after a mutating experiment becomes ambiguous.

Canonical experiments:

1. clean page load with no action;
2. conversation list load;
3. open an existing conversation;
4. create a new conversation;
5. send a deterministic text prompt;
6. observe a complete streamed response;
7. stop generation mid-stream;
8. regenerate/retry;
9. edit or branch from a previous user turn where supported;
10. upload a tiny non-sensitive text attachment;
11. switch model/settings where exposed;
12. repeat selected experiments under an intentionally interrupted connection.

The first machine-executable experiment definitions live under `protocol/experiments/`. Each experiment should produce its own capture or clearly delimited action log. Human QA is reserved for observations the harness genuinely cannot obtain itself.

Current state: the basic canonical text-turn stream is directly observed and typed in `crates/protocol`; browser-local capture and protocol-backed reconciliation work on Linux. Flight Recorder export ingestion, fail-closed selected-run sanitization, deterministic sanitization reports, HAR/Flight structural inventories and diffs, evidence-scoped field classification, committed-corpus validation, frontend asset identity manifests, and frontend asset manifest diffs are automated.

P0.5 exit criterion: equivalent controlled captures can be repeated and transformed into deterministic sanitized evidence/inventory without Windows-specific bootstrap machinery or manual protocol archaeology from zero.

## P1 — recorder and diff tooling — COMPLETE

Build reproducible tooling for turning captures into protocol evidence.

Deliverables:

- HAR and Flight Recorder capture ingestion;
- fail-closed secret/content sanitization and sanitization report;
- frontend asset manifest/hashes;
- request/response/event shape extraction;
- stable-vs-ephemeral field annotations;
- structural diff between protocol snapshots;
- fixture validation in CI.

Exit criterion met: a new ChatGPT deployment can be captured, sanitized, inventoried, classified, fixture-validated, and structurally compared with the last working observation without manual archaeology from zero.

## P2 — durable application core — COMPLETE

Implement the local event model and persistence substrate.

Deliverables:

- typed local IDs;
- user-message commit transaction;
- turn evidence state machine;
- append-oriented event journal;
- SQLite projections and migrations;
- projection rebuild tests;
- interrupted-turn recovery tests.

Current state: typed local identities, fsync-backed typed user-message commits, replayable turn evidence, the append-only JSONL journal, and SQLite projection schema v2 are implemented. SQLite rebuilds both generic durable events and typed authored-turn state (exact text + local IDs + replayed evidence) transactionally from the journal, while legacy text-only commits remain event-only.

The persistent crash matrix now reopens the real JSONL journal and SQLite projection across pre-commit, committed, dispatching, ambiguous interruption, reconciliation, accepted, streaming, partial-interruption, completion-before-projection-update, and final rebuild boundaries. A typed commit also survives an fsynced unterminated next-record tail without fabricating dispatch evidence.

Exit criterion met: simulated crashes across the durable turn lifecycle do not lose committed authorship, erase typed identity, fabricate remote certainty, or corrupt recoverable projection state.

## P3 — read-only remote integration

Consume the observed ChatGPT protocol without sending mutations first.

Deliverables:

- authenticated-session boundary;
- conversation listing;
- conversation fetch;
- remote/local identity mapping;
- import into the local durable model;
- protocol mismatch diagnostics.

Current state: Flight Recorder v0.7.0 now has an explicit, bounded protocol-read evidence mode that is off by default and captures only explicitly armed same-origin `GET`/`HEAD` responses under `/backend-api/` with JSON-family content types. Query values, request headers, cookies, authorization values, request bodies, browser storage, and third-party traffic are not captured. Offline `snapshot-flight` reduces private read bodies to deterministic object/array/key/type structure and stable identity placeholders before public evidence is written; malformed declared JSON fails closed and truncation remains explicit. C01/C02 experiment definitions and corpus guards exist. Local and remote conversation identity are also now separate first-class domains, with restart-safe one-to-one binding that preserves the protocol observation revision supporting the correlation. No conversation-list/fetch endpoint or response semantics are considered supported until controlled C01/C02 evidence is actually observed and committed.

Exit criterion: Chatarium can mirror selected existing conversations into local durable state and explain incompatibilities against a named protocol snapshot.

## P4 — direct text turns

Add controlled remote mutation support.

Deliverables:

- create conversation;
- send text message;
- stream assistant response;
- stop generation;
- reconcile uncertain outcomes;
- avoid duplicate sends after ambiguous disconnects;
- preserve raw observed events needed for debugging.

Exit criterion: ordinary text conversation is usable without the official site UI for the validated protocol revision.

## P5 — native desktop workstation

Complete the egui experience around the reliable core.

Deliverables:

- conversation browser;
- durable composer;
- transcript renderer;
- explicit local/remote/recovery status;
- attachments;
- search;
- export;
- diagnostic/event inspector;
- protocol compatibility indicator.

Exit criterion: Chatarium is the preferred daily interaction surface rather than merely a recovery utility.

## P6 — feature expansion

Only after the text path is reliable:

- branches/edit/retry semantics;
- model and reasoning controls;
- richer attachments;
- tool/event rendering;
- project-like organization;
- local full-text search and indexing;
- optional interoperability with other local project-state systems.

## P7 — supervisory orchestration and MCP workstation

Build the multi-session/tool control plane described in `docs/PRODUCT_VISION.md`.

This phase is intentionally sequenced after reliable direct session interaction. It is part of the intended product endpoint, not a redefinition of Chatarium into a generic browser.

Deliverables:

- first-class MCP/tool integration surface;
- import/adaptation of the existing user-owned XML tool/message envelope into a versioned Chatarium contract;
- multiple concurrent ChatGPT sessions with explicit local session identity;
- user-designated master/controller and worker relationships;
- durable goal assignment/update and continuation/stop messages;
- explicit worker lifecycle states such as working, blocked, needs-input, completed, and failed;
- correlation rules that prevent accidental unbounded master/worker `continue` loops;
- local routing/message bus with source/destination/provenance;
- egui supervisory console showing routed session/tool traffic;
- user policy controls for allow, block/forbid, require approval, redirect, and interrupt;
- durable/auditable orchestration and tool-call history.

Exit criterion: the user can supervise several ChatGPT sessions and MCP/tools from one Chatarium surface, delegate/continue work without manual copy-paste, positively recognize worker completion, and inspect or veto cross-session/tool traffic before or after dispatch according to policy.

## Continuous work — protocol revision response

Whenever the official client changes materially:

```text
capture -> sanitize -> snapshot -> diff -> document -> adapt -> test -> release
```

A protocol break is a maintenance event, not a reason to erase history or silently broaden assumptions.
