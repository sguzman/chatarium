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

Create the first controlled observation set of the official ChatGPT web client.

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

Each experiment should have its own capture or clearly delimited action log.

Exit criterion: snapshot `2026-09-17.001` (or the actual first observation ID) documents enough of the request/event lifecycle to explain a basic text turn without guessing.

## P1 — recorder and diff tooling

Build reproducible tooling for turning captures into protocol evidence.

Deliverables:

- HAR/capture ingestion;
- secret scanner and sanitization report;
- frontend asset manifest/hashes;
- request/response/event shape extraction;
- stable-vs-ephemeral field annotations;
- structural diff between protocol snapshots;
- fixture validation in CI.

Exit criterion: a new ChatGPT deployment can be captured and compared with the last working observation without manual archaeology from zero.

## P2 — durable application core

Implement the local event model and persistence substrate.

Deliverables:

- typed local IDs;
- user-message commit transaction;
- turn evidence state machine;
- append-oriented event journal;
- SQLite projections and migrations;
- projection rebuild tests;
- interrupted-turn recovery tests.

Exit criterion: simulated crashes at every transition do not lose committed authorship or corrupt the recoverable history.

## P3 — read-only remote integration

Consume the observed ChatGPT protocol without sending mutations first.

Deliverables:

- authenticated-session boundary;
- conversation listing;
- conversation fetch;
- remote/local identity mapping;
- import into the local durable model;
- protocol mismatch diagnostics.

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

## Continuous work — protocol revision response

Whenever the official client changes materially:

```text
capture -> sanitize -> snapshot -> diff -> document -> adapt -> test -> release
```

A protocol break is a maintenance event, not a reason to erase history or silently broaden assumptions.
