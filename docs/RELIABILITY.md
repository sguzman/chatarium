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
