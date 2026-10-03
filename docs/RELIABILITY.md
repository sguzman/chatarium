# Reliability contract

This document defines behavior that is more important than feature parity.

## R1 — authorship survives transport

Before any remote send attempt, the exact user-authored payload must be durably committed locally. If that commit fails, the remote send must not begin.

## R2 — observation survives rendering

Assistant output is persisted as it is observed, independent of whether the current UI successfully renders it. A renderer crash or reload must not erase already received content.

## R3 — uncertainty is durable

Transport failures can produce an unknowable remote outcome. For example, request bytes may reach the server even if the acknowledgement never reaches the client. Chatarium records this as uncertainty and preserves the evidence needed for reconciliation.

It must not automatically resend a potentially accepted mutation merely because the connection failed.

## R4 — local event history is append-oriented

Corrections and reconciliations add facts; they do not rewrite the historical record of what the client previously observed. Materialized views may change, but the event history explains why.

## R5 — recovery is testable

Crash tests should cover at least:

- before local message commit;
- after local commit but before network dispatch;
- during dispatch;
- after remote acceptance is observed;
- during streamed output;
- after completion arrives but before projection update;
- during reconciliation.

Each test must have an unambiguous expected local result.

This contract is exercised by the persistent store crash matrix in `crates/store/tests/crash_recovery.rs`. The test repeatedly drops/reopens the real JSONL journal and schema-v2 SQLite projection across the turn lifecycle, including the boundary where a completion event is durable in JSONL but SQLite has not yet been rebuilt. It also verifies that an unterminated would-be next event is truncated without losing or altering the preceding typed user-message commit.

## R6 — identity is explicit

Local IDs exist independently from remote IDs. Remote identifiers are evidence used for mapping/reconciliation; they are not primary keys for local authorship.

## R7 — no hidden destructive normalization

The persisted authored body is exact. Presentation layers may normalize whitespace for display, but a recoverable original representation remains available.

## R8 — protocol mismatch fails visibly

If an observed remote shape violates the currently supported interpretation, preserve enough raw sanitized diagnostic data to explain the mismatch and surface the incompatibility. Do not discard unknown fields merely to make parsing succeed.

## R9 — UI responsiveness is a correctness property

Disk, network, parsing, capture processing, indexing, and reconciliation work must not block egui rendering. A frozen interface during a network incident is itself a reliability failure because it removes user visibility and control.

## R10 — backup is not synchronization

The P0 browser flight recorder provides local survivability. It does not claim its DOM observations are a complete canonical replica of remote state. Later direct protocol integration adds stronger reconciliation semantics.

## R11 — status labels are evidence claims

A green/healthy status is an assertion about evidence, not decoration.

The UI must distinguish at least these browser-integration levels when applicable:

- desktop listener exists;
- browser component is alive;
- browser/desktop roundtrip succeeded;
- exact ChatGPT tab was found;
- main-world execution succeeded;
- ChatGPT session is authenticated;
- required account/workspace context is present;
- target HTTP request succeeded;
- response schema validated;
- semantic sanity checks passed;
- durable local mirror committed.

A lower-level success must never be labeled as a higher-level success. In particular, `listener ready`, `browser authenticated`, HTTP 200, and parse success are not synonyms for synchronization.

## R12 — contradiction invalidates apparent success

A structurally valid remote response can still be semantically wrong for the intended account/context.

If local/visible evidence contradicts a remote result, preserve the contradiction and fail/warn visibly rather than normalizing it into success.

Examples:

- a conversation list reports zero while the first-party account visibly contains ordinary conversations;
- a conversation response identity does not match the requested remote ID;
- first-party evidence requires account context but the reproduced request lacks it;
- pagination proves older content exists while the UI presents the current page as complete.

Contradiction detection is part of correctness.

## R13 — critical browser transport must be promotable, observable, and replaceable

A browser-side prototype is not production infrastructure merely because one roundtrip succeeded.

Any browser integration on the critical account-history path must have:

- explicit version identity;
- deterministic transport diagnostics;
- a stable execution-world model;
- request-context parity;
- a browser-version compatibility story;
- a generated or visible failure-stage trace;
- a documented fallback/replacement path.

The 2026-10-03 Tampermonkey bridge incident is the canonical counterexample; see `docs/postmortems/2026-10-03-chatgpt-history-bridge.md`.

## Suggested durability tiers

Chatarium may expose these diagnostic labels:

- `draft-memory` — only transiently edited; not yet durably flushed;
- `draft-durable` — latest draft body persisted;
- `message-committed` — immutable outgoing body committed locally;
- `remote-unknown` — dispatch attempted; acceptance not established;
- `remote-observed` — remote identity/acceptance observed;
- `stream-partial` — assistant content observed but not completed;
- `complete-observed` — completion observed;
- `reconciled` — later remote observation resolved prior uncertainty.

These names describe evidence available to Chatarium, not metaphysical truth about the server.
