# Architecture

Chatarium is organized around one asymmetry: local state can be made durable and inspectable; the remote ChatGPT service and the network path to it cannot be assumed to be either.

## System model

```text
                         observed evidence
                              |
                              v
+----------------+      +--------------+      +----------------+
| protocol corpus|----->| protocol     |----->| remote adapter |
+----------------+      | interpretation|      +-------+--------+
                        +--------------+              |
                                                      |
+----------------+      +--------------+              |
| desktop / P0 UI|<---->| application  |<-------------+
+----------------+      | core         |
                        +------+-------+
                               |
                               v
                        +--------------+
                        | durable store|
                        +--------------+
```

The protocol corpus is not generated from Rust types. Rust types are generated or written from protocol evidence.

## Durable authority

The authoritative local record consists of append-oriented events plus materialized projections for fast UI access. A future schema may evolve, but the semantic split is fixed:

- **events** answer what was observed or attempted and when;
- **projections** answer what the UI should currently render;
- **remote identifiers** allow later reconciliation without making remote state authoritative over local authorship.

The first persistent implementation is a newline-delimited JSON event journal in `crates/store`. Each complete record has a stable event-kind name, monotonic sequence, local Unix-millisecond timestamp, and exact textual payload. A successful persistent append does not return until the line has been written, flushed, and `sync_data()` has succeeded.

Startup treats only an **unterminated final fragment** as a torn last write and truncates it back to the previous newline. A malformed complete record is a hard integrity error; it is not silently skipped.

SQLite now exists as a rebuildable projection/index layer in `crates/store`. It does not replace the append journal as the evidence history. The projection database is versioned independently, can be deleted and rebuilt entirely from durable journal events, and is updated/rebuilt transactionally so a failed rebuild cannot replace a previously valid projection.

Projection schema v2 also materializes typed locally authored turns derived from typed `UserMessageCommitted` events plus same-turn durable evidence. Each row preserves local conversation/turn/message identities, exact committed user text, commit/last-applied sequences, and replayed local/remote/assistant evidence. Legacy text-only commits remain in the event projection but are intentionally absent from the typed turn table because Chatarium does not invent local IDs retroactively. After a projection-schema migration, the projection is not considered current until a full journal rebuild materializes the new schema.

The minimum event vocabulary includes:

- draft changed;
- user message committed;
- remote dispatch attempted;
- remote acceptance observed;
- assistant stream started;
- assistant delta observed;
- assistant completion observed;
- connection interrupted;
- remote failure observed;
- reconciliation attempted;
- reconciliation result observed.

## The local-commit gate

Remote mutation has a hard precondition:

```text
exact user text
    |
    v
append UserMessageCommitted
    |
    v
flush + sync_data
    |
    v
local acknowledgement
    |
    v
ONLY NOW may a remote adapter dispatch
```

A UI click is not a commitment. Queueing a disk write is not a commitment. Starting an HTTP request is not a commitment. `LocalEvidence::MessageCommitted` means the persistence boundary positively acknowledged the exact user text.

The desktop shell already exercises this contract without networking. Composer edits are sent to a background persistence worker so render work never waits on disk. The UI visibly distinguishes `saving…` from `durable`; the local commit action waits for a journal acknowledgement before changing turn evidence.

The store crash matrix independently verifies the same boundary through process-style reopen/rebuild tests: once a typed commit has returned, later torn-tail recovery or stale SQLite state cannot erase its exact text or local identities.

## Turn state is evidence, not optimism

A turn is not modeled with a single `sent` boolean. Local and remote knowledge are separate.

A representative lifecycle is:

```text
Draft
  |
  v
CommittedLocal
  |
  v
Dispatching
  |--------------------------+
  |                          |
  v                          v
AcceptedObserved        OutcomeUnknown
  |                          |
  v                          +--> ReconciliationPending
Streaming                         |
  |                               +--> AcceptedObserved
  |                               +--> FailedObserved
  |                               +--> StillUnknown
  +--> CompletedObserved
  +--> Interrupted
  +--> FailedObserved
```

`OutcomeUnknown` is a valid state. A socket closing after request bytes were written does not prove that the remote service rejected the request.

## Crate boundaries

### `crates/core`

Owns domain identifiers, local/remote evidence state, events, commands, and recovery decisions. Local conversation, turn, and message identities are distinct UUIDv7-backed Rust types; their UUID ordering is not treated as semantic chronology. Stable event names used by durable storage are defined here. It knows nothing about egui and should know as little as possible about concrete HTTP shapes.

### `crates/protocol`

Owns typed interpretations of empirically observed ChatGPT request/response/event shapes. Every supported interpretation identifies the protocol observation revision from which it was derived.

### `crates/store`

Owns durable persistence, migrations, event append, projection rebuild, and transactional guarantees. The authoritative implementation is the crash-recoverable JSONL journal. SQLite is a subordinate query projection with explicit schema migration, sequence validation, scope/kind indexes, a typed authored-turn materialization, and transactionally safe rebuild semantics. No network logic belongs here.

### `tools/recorder`

Owns offline capture ingestion, sanitization checks, structural request inventories, snapshot manifests, schema extraction, and revision diffs. Browser/CDP capture integration can be added behind this boundary.

### `apps/desktop`

Owns presentation and user interaction. The render thread emits persistence/network commands and consumes cheap state snapshots; it does not perform blocking network or storage operations. A background persistence worker currently owns the journal file.

## P0 browser flight recorder

Before the full native client is capable of direct interaction, Chatarium provides a browser-side reliability layer. Its purpose is narrow:

- persist composer state continuously into a per-conversation emergency WAL;
- snapshot outgoing send intent into a separate synchronous journal before the site's submit path;
- capture conversation identity and URL;
- persist the latest rendered assistant output into an independent emergency WAL and IndexedDB projection;
- record connectivity, navigation, visible error/toast observations, and unresolved sends;
- provide copy/export recovery surfaces independent of the current DOM.

This layer is intentionally disposable once the native client supersedes it, but its evidence vocabulary should align with `crates/core` so recovered histories can be imported later.

## Protocol revisions

A protocol revision is an observation, not a semantic version of ChatGPT. IDs use a timestamp-oriented form such as `2026-09-17.001`. Multiple observations on the same day may increment the suffix.

The implementation advertises the newest observation it was validated against. Compatibility with later revisions is unknown until tested.

## Authentication boundary

Authentication will initially remain an explicit boundary rather than guessed code. Captures may show how the official client proves an authenticated session, but committed fixtures must remove reusable credentials. Chatarium may use a user's own authenticated session where technically appropriate; it must not bypass authentication or protections.

## Future orchestration and tool plane

The direct ChatGPT adapter is not the final architecture boundary. The long-term workstation adds a **local routing/policy plane** above reliable single-session primitives.

That plane will coordinate:

- human-authored conversation messages;
- master/controller-session instructions;
- worker-session status/results;
- goal updates and continuation/stop control messages;
- MCP/tool request/result envelopes;
- user approval/deny decisions.

The intended design is an explicit durable message bus, not hidden prompt automation. Every routed action should have a source, destination, identity/correlation, lifecycle state, provenance, and policy decision where applicable.

A controller/master session may coordinate worker sessions, but it is subordinate to user policy. The desktop UI must eventually let the operator inspect queued/dispatched/completed traffic and allow, block/forbid, require approval for, redirect, or interrupt routes.

Cross-session orchestration must use explicit lifecycle/control states rather than infer completion from ordinary prose. In particular, the system needs machine-recognizable completion/blocked/input-needed states so controller and worker sessions cannot accidentally enter unbounded mutual `continue` loops.

MCP/tool integration should reuse or adapt the user's existing XML-oriented envelope from the Braizen/ChatGPT JavaScript-shim work where practical. Until that schema is recovered and committed, its concrete wire fields remain unspecified; Chatarium must not invent them from memory.

The durable event model should be extensible to this plane. Orchestration state must survive restart, and tool/session actions must remain auditable. See [`PRODUCT_VISION.md`](PRODUCT_VISION.md).

The first durable routing audit now uses the same append-only JSONL authority as ordinary turn evidence. Typed route proposal, explicit user decision, one-shot dispatch, and generic result/error observations replay after restart through the core route gate semantics. SQLite carries these routing events through its existing generic projected-events table; there is intentionally no dedicated routing materialization or schema bump yet. This is still local control-plane state only: no live master/worker or MCP transport is implied.

## Failure philosophy

Chatarium prefers visible uncertainty over fabricated certainty. If a failure cannot be distinguished from a successful remote action whose acknowledgement was lost, the UI should say so and offer reconciliation instead of resending blindly.
