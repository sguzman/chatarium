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

Owns domain identifiers, local/remote evidence state, events, commands, and recovery decisions. It knows nothing about egui and should know as little as possible about concrete HTTP shapes.

### `crates/protocol`

Owns typed interpretations of empirically observed ChatGPT request/response/event shapes. Every supported interpretation identifies the protocol observation revision from which it was derived.

### `crates/store`

Owns durable persistence, migrations, event append, projection rebuild, and transactional guarantees. No network logic belongs here.

### `tools/recorder`

Owns offline capture ingestion, sanitization checks, schema extraction, snapshot manifests, and revision diffs. Browser/CDP capture integration can be added behind this boundary.

### `apps/desktop`

Owns presentation and user interaction. The render thread emits commands and consumes cheap state snapshots; it does not perform blocking network or storage operations.

## P0 browser flight recorder

Before the full native client is capable of direct interaction, Chatarium will provide a browser-side reliability layer. Its purpose is narrow:

- persist composer state continuously;
- snapshot the exact user message before submit;
- capture conversation identity and URL;
- persist assistant text incrementally as it becomes observable;
- record timestamps and failure/reload events;
- provide an export/recovery surface independent of the current DOM.

This layer is intentionally disposable once the native client supersedes it, but its event vocabulary should align with `crates/core` so recovered histories can be imported later.

## Protocol revisions

A protocol revision is an observation, not a semantic version of ChatGPT. IDs use a timestamp-oriented form such as `2026-09-17.001`. Multiple observations on the same day may increment the suffix.

The implementation advertises the newest observation it was validated against. Compatibility with later revisions is unknown until tested.

## Authentication boundary

Authentication will initially remain an explicit boundary rather than guessed code. Captures may show how the official client proves an authenticated session, but committed fixtures must remove reusable credentials. Chatarium may use a user's own authenticated session where technically appropriate; it must not bypass authentication or protections.

## Failure philosophy

Chatarium prefers visible uncertainty over fabricated certainty. If a failure cannot be distinguished from a successful remote action whose acknowledgement was lost, the UI should say so and offer reconciliation instead of resending blindly.
