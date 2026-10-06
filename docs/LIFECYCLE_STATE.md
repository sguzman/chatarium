# Local-First Lifecycle State

Status: **ACTIVE**, first desktop-integrated slice landed 2026-10-06.

Chatarium already had a durable worker lifecycle domain before the current
local-first conversation phase. The local-first lifecycle work therefore reuses
that state machine rather than inventing a second set of lifecycle labels.

## Existing authoritative lifecycle

`crates/core/src/orchestration.rs` defines the worker phases:

`Unassigned → Ready → Working → NeedsInput / Blocked / Completed / Failed / Stopped`

The exact transition rules remain owned by `WorkerLifecycle`:

- a new goal enters `Ready`;
- `Ready`, `NeedsInput`, and `Blocked` may explicitly start/resume to
  `Working`;
- only `Working` may request input, become blocked, or complete;
- failure and stop remain explicit terminal outcomes subject to the core
  transition rules;
- `Completed`, `Failed`, and `Stopped` are terminal for the current goal;
- a replacement goal receives a new `WorkerGoalId`;
- bounded continuation authority remains the separate existing
  `ContinuationLease` / `ContinuationPermit` mechanism.

`crates/store/src/worker_audit.rs` remains the authoritative durable replay
layer for those lifecycle facts.

## Local-conversation bridge

The missing local-first identity edge is now explicit.

`crates/store/src/local_conversation_worker_audit.rs` records a durable,
one-to-one:

`LocalConversationId → WorkerId`

binding in the authoritative append-only journal using
`LocalConversationWorkerBound`.

A local conversation cannot silently change worker identity, and one worker
cannot be bound to multiple local conversations through this bridge.

This binding does not itself:

- assign a goal;
- change worker lifecycle;
- create routing endpoints;
- designate a controller;
- create continuation authority;
- send an inference request;
- add anything to model context.

It is identity/provenance only.

## Persistence-thread validation

Desktop lifecycle actions are commands to the background persistence worker.
The render thread does not append lifecycle events directly.

Immediately before each append, the persistence worker replays authoritative
journal state and validates the requested operation through the existing core
state machine.

This prevents a stale UI action from writing an illegal transition.

The checked path also refuses to create lifecycle history for a WorkerId that
is not durably bound to a local Chatarium conversation.

Tests cover the fail-closed boundary: for example, attempting `Complete`
while a worker is `NeedsInput` is rejected without appending a journal event.
After explicit resume, completion succeeds.

## Desktop controls

Under **Context & inference controls → Worker lifecycle**, a local conversation
may explicitly opt into worker lifecycle.

The first UI slice is deliberately manual:

- enable worker lifecycle;
- assign first/new goal identity;
- start;
- resume;
- mark Needs input;
- mark Blocked;
- complete;
- fail;
- stop.

The UI is a projection of durable journal state. It does not maintain a second
mutable lifecycle model.

One lifecycle mutation may be in flight at a time, preventing repeated clicks
from queuing duplicate transitions before the durability acknowledgement
returns.

## Deliberate non-features

This slice does **not** implement:

- automatic `continue` prompts;
- machine inference of lifecycle state from assistant prose;
- automatic completion detection;
- controller-issued lifecycle commands;
- routing between local conversations;
- hidden lifecycle instructions or developer-context injection;
- lifecycle-dependent ordinary-user-message blocking;
- automatic goal text extraction from the conversation;
- automatic continuation leases.

Ordinary local chat remains ordinary local chat. Lifecycle is explicit
orchestration metadata layered beside it.

## Relationship to existing orchestration architecture

The repository already contains stronger downstream primitives:

- bounded continuation leases and permits;
- `SessionId` and WorkerId↔SessionId bindings;
- routing endpoints and one-shot route policy gates;
- controller designation and controller→worker supervision;
- durable orchestration-control admission/correlation/freshness;
- logical chat-container rollover.

The local conversation → chat-container → current-session bridge, explicit
current-session → route-endpoint addressability, read-only local routing
directory, and manual `SessionMessage` route approval/denial are now landed;
see [LOCAL_ORCHESTRATION_IDENTITY.md](LOCAL_ORCHESTRATION_IDENTITY.md).

In particular, do not assume that `LocalConversationId`, `SessionId`,
`ChatContainerId`, `WorkerId`, and `RouteEndpointId` are interchangeable.
Manual route policy and immutable routed payloads are now separate durable
layers. Approval requires the exact payload to be attached first, but there is
still no dispatch, delivery, automatic controller action, or hidden context
sharing.

The next safe substrate is one-shot permit-gated local delivery with explicit
routed-message provenance.
