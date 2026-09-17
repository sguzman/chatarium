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

SQLite is intended as a later projection/index layer. It does not replace the append journal as the evidence history.

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

Owns domain identifiers, local/remote evidence state, events, commands, and recovery decisions. Stable event names used by durable storage are defined here. It knows nothing about egui and should know as little as possible about concrete HTTP shapes.

### `crates/protocol`

Owns typed interpretations of empirically observed ChatGPT request/response/event shapes. Every supported interpretation identifies the protocol observation revision from which it was derived.

### `crates/store`

Owns durable persistence, migrations, event append, projection rebuild, and transactional guarantees. The first implementation is the crash-recoverable JSONL journal; SQLite projections come later. No network logic belongs here.

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

## Failure philosophy

Chatarium prefers visible uncertainty over fabricated certainty. If a failure cannot be distinguished from a successful remote action whose acknowledgement was lost, the UI should say so and offer reconciliation instead of resending blindly.
