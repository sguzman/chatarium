# Local-First Orchestration Identity

Status: **ACTIVE**, first durable local-conversation topology slice landed 2026-10-06.

Local-first Chatarium now has enough independent identity domains that accidental
conflation would become expensive. This document freezes how those domains
relate before live local routing/controller work begins.

## Identity domains

### `LocalConversationId`

The existing user-facing durable local conversation/workspace identity.

It owns today's local transcript, drafts, inference settings, Behavior Profile,
and the optional local worker-lifecycle association.

It is not a remote ChatGPT conversation identifier.

### `ChatContainerId`

The existing orchestration continuity identity for one logical chat lineage.

A container outlives individual `SessionId` leaves and preserves explicit
rollover provenance.

When local-first orchestration is activated for a conversation, the intended
relationship is one local conversation to one logical chat container.

The types remain distinct even if the first implementation is one-to-one.

### `SessionId`

One replaceable local execution/session surface inside a chat container.

For SIWC local-first inference this **does not claim a persistent server-side
Responses conversation exists**. Chatarium still resends locally owned context
with `store:false`.

A root SessionId is therefore local execution/orchestration identity. Future
rollover may retire it and install a successor without replacing the
`LocalConversationId`.

### `WorkerId`

Machine-readable orchestration worker identity.

Worker lifecycle and goal state are separate from conversation transcript and
session lifecycle.

The current local-first bridge may associate a worker with a local conversation,
but that does not make WorkerId equal to LocalConversationId or SessionId.

Worker identity now explicitly **persists across session rollover**. SessionId is
the replaceable execution leaf. Active WorkerId→SessionId ownership may move
only through a durable worker-session successor handoff; it is never inferred
from numeric order, container membership, or creation order.

### `RouteEndpointId`

Addressability in the routing plane.

An endpoint is not itself a conversation, worker, or session. Existing session
bindings are one way of correlating an endpoint to a session.

Tool adapters may also be endpoints.

## Required shape

The local-first orchestration topology should preserve this conceptual layering:

```text
LocalConversationId
        |
        | explicit durable ownership/correlation
        v
ChatContainerId
        |
        | current leaf
        v
SessionId
        |
        | explicit routing correlation
        v
RouteEndpointId

LocalConversationId
        |
        | explicit lifecycle association
        v
WorkerId
```

No edge in this diagram is permission authority by itself.

## Why LocalConversationId is not SessionId

A local conversation is intended to survive execution/session rollover.

Equating it directly with one SessionId would make a future saturated session
look like the conversation itself had ended, contradicting the existing
chat-container model.

## Why LocalConversationId is not ChatContainerId

They currently have similar logical lifetimes, but they belong to different
layers and use different identity representations.

Keeping an explicit edge lets Chatarium:

- preserve today's UUIDv7 local conversation identities;
- reuse the already-tested chat-container lineage machinery;
- migrate or evolve orchestration internals without rewriting transcript
  identity;
- detect missing/ambiguous topology instead of relying on implicit equality.

## SIWC-specific rule

The Sign in with ChatGPT Responses path does not create a durable remote
conversation/session for Chatarium.

Therefore a local SessionId used with SIWC must never be described as a remote
ChatGPT session identity.

It is a local orchestration/execution leaf only.

## Landed worker-session rollover semantics

WorkerId is the durable logical worker identity. SessionId is its replaceable
execution leaf.

The initial `WorkerSessionBound` fact remains one-time and one-to-one. Ordinary
binding still cannot attach one WorkerId to two sessions.

Rollover uses the separate typed `WorkerSessionSuccessorBound` fact naming:

- WorkerId;
- predecessor SessionId;
- successor SessionId.

Replay requires the named predecessor to be the worker's currently active
session and the successor to be registered and not already worker-bound.

Historical predecessor bindings remain on their old session records. They are
not erased or rewritten. The active WorkerId→SessionId projection is instead
the most recent valid binding/handoff.

Historical control validation resolves the worker session that was active
**before the event being validated**. A later rollover therefore cannot
retroactively make older controller provenance or orchestration routes point at
the successor.

Tests cover both sides of the boundary: controls/routes before handoff resolve
the predecessor; later controls/routes resolve the successor.

## Landed topology substrate

The `LocalConversationId → ChatContainerId → current SessionId` path is now
durable and inspectable.

`LocalConversationChatContainerBound` is a one-to-one durable identity edge.
Replay joins that edge to the existing chat-container lineage, so a future
session successor changes the projected current `SessionId` without rewriting
the `LocalConversationId → ChatContainerId` ownership edge.

Desktop initialization writes three durable facts in order:

1. register a fresh local `SessionId`;
2. create a fresh `ChatContainerId` around that root session;
3. bind the local conversation to the container.

The final binding is the activation point. A crash before it may leave an
unclaimed local session/container identity, but that orphan has no
conversation/routing/context authority.

The desktop inspector shows container identity, current session identity,
session lifecycle phase, lineage size, and root identity when rollover has
occurred.

Topology creation does **not**:

- bind a routing endpoint;
- bind the conversation worker to the session;
- designate a controller;
- create a route;
- share transcript/context;
- alter inference requests.

Archive integrity replay validates this topology along with the rest of the
authoritative journal.

## Landed route addressability

The current local session can now acquire an explicit durable
`SessionId → RouteEndpointId` binding through the existing session audit.

The desktop exposes this as **Bind routing endpoint** only after a local
conversation topology exists. The checked append requires:

- the requested session is the conversation's current chat-container leaf;
- the current session has no endpoint already;
- the endpoint is not bound to another session;
- the endpoint has never appeared as the source or destination of a durable
  historical route.

Fresh endpoint allocation therefore scans both session bindings and route
history. A session cannot retroactively claim an endpoint identity previously
used by another routing surface.

A future chat-container rollover does not inherit the predecessor endpoint
implicitly. The successor current session must acquire its own explicit
addressability edge.

Binding an endpoint still creates no route proposal, policy decision, dispatch
permit, payload transfer, shared context, or controller authority.

## Landed local routing directory

The deterministic local routing directory is now implemented as a read-only
projection over:

`LocalConversationId → ChatContainerId → current SessionId → RouteEndpointId`

Only the current session leaf is eligible. A topology without a current-session
endpoint is absent from the directory. A rollover does not inherit the
predecessor endpoint.

The directory creates no route, permission, payload transfer, worker binding,
or controller authority.

## Landed manual route policy

The desktop can now propose an identity-only `SessionMessage` route from the
current addressable local conversation to another addressable local
conversation.

Every such proposal starts under `RoutePolicy::RequireApproval`. The
persistence worker resolves both conversation IDs through the current routing
directory immediately before append, then records the existing typed
`RouteProposed` audit fact.

The user may explicitly Allow or Deny the route. Those decisions use the
existing durable `RouteGate` replay semantics and remain reversible until
dispatch. A decision fails closed if either endpoint is no longer the current
leaf or can no longer accept ordinary turns.

This slice deliberately carries **no routed message payload** and exposes **no
dispatch action**. An allowed route therefore means only that policy would
permit a future dispatch once a separately durable payload and dispatch path
exist.

## Landed immutable route payloads

Local `SessionMessage` routes now carry a separately durable immutable payload
through `RoutePayloadId` and the `RoutePayloadAttached` journal fact.

Payload replay validates the route and endpoint-to-conversation ownership using
the journal prefix that existed immediately before attachment. Historical
payload provenance therefore survives later session rollover without being
reinterpreted against today's current leaf.

The payload layer enforces:

- one payload identity per route;
- one route per payload identity;
- exact text preservation;
- prior route proposal;
- session-message route class;
- no payload attachment after dispatch;
- point-in-time source/destination conversation provenance;
- ordinary-turn-capable endpoint state at attachment time.

Payload text remains outside the generic routing audit. Archive integrity checks
replay the payload audit independently.

The desktop exposes an exact payload draft for each route. Once attached, the
payload is immutable and inspectable. A route may be denied without a payload,
but **Allow is blocked until the immutable payload is durable**.

## Landed one-shot local dispatch and delivery

Approved local `SessionMessage` routes can now consume the existing typed
`DispatchPermit` exactly once.

The persistence worker revalidates the current local routing directory,
immutable payload provenance, ordinary-turn-capable session leaves, and durable
RouteGate state immediately before dispatch.

Successful local delivery records a separate `LocalRouteDelivered` fact with:

- route and payload identity;
- source/destination local conversation identity;
- source/destination current SessionId at delivery time;
- the durable dispatch sequence;
- the durable delivery sequence.

A crash after durable dispatch but before durable delivery can be recovered by
finishing delivery without consuming a second dispatch permit.

Delivery does not append a user-authored message and does not alter inference
context.

## Landed routed inbox projection

Successfully delivered payloads now project into a read-only routed inbox for
the destination local conversation.

The inbox joins immutable payload text with route/payload/session provenance.
It creates no new journal fact and does not masquerade as ordinary transcript
content.

The desktop exposes **Routed inbox** separately from the conversation
transcript and explicitly marks routed items as excluded from inference
context.

## Landed routed context admission

Delivery and inference-context use are now separate durable decisions.

Each destination inbox item defaults to excluded from context. The user can
explicitly **Admit to context** and later **Exclude from context**. Those
decisions are durable, reversible, destination-conversation-scoped, and survive
session rollover.

Context Composer consumes only currently admitted items. It serializes them at
user trust level inside an explicit Chatarium routed-peer envelope carrying
source conversation, route, payload, delivery, and admission provenance.

The admitted routed-context set is snapshotted when the destination user clicks
Send. A later Admit/Exclude change cannot mutate that pending request while the
authored message is crossing its local durability gate.

Routed peer text never becomes a user-authored transcript event and is never
elevated to developer/system instructions.

## Landed local controller supervision

The desktop can now bridge a local conversation's durable WorkerId to its
current SessionId explicitly.

For the initial leaf this records `WorkerSessionBound`. After chat-container
rollover, the UI shows that the WorkerId is still active on its predecessor and
offers **Advance worker to current session**. Persistence accepts that handoff
only when the new current leaf is the predecessor's direct chat-container
successor, then records `WorkerSessionSuccessorBound`.

A separate local conversation current session may be explicitly designated as a
controller through `ControllerSessionDesignated`. A worker-bound current
session cannot be designated as a controller.

A designated controller may then supervise another local conversation only
when that conversation:

- has a durable LocalConversationId→WorkerId binding;
- has orchestration topology;
- has that WorkerId actively bound to its current SessionId leaf.

The resulting `ControllerWorkerBound` fact is coordination provenance only.
The desktop shows current role and supervision state and fails closed when a
worker is unbound, stale after rollover, or already supervised elsewhere.

This slice grants no route policy, dispatch, continuation, lifecycle mutation,
or inference authority.

## Landed controller-issued typed controls

A designated local controller can now construct typed worker controls against a
durably supervised worker. The checked path records the existing control
admission, explicit controller issuer provenance, an `OrchestrationControl`
route, and control↔route correlation as separate durable facts.

Every controller control route starts under `RequireApproval`. Allow/Deny is
explicit user policy, and dispatch revalidates control freshness immediately
before consuming the one-shot route permit.

## Landed worker control delivery

Controller-control dispatch now has a destination-side durable boundary.

A successful dispatch records `RouteDispatched` and then a separate
`WorkerControlDelivered` fact containing the control/route/worker identity,
the durable local worker conversation owner, the worker SessionId targeted at
dispatch, the controller SessionId, and dispatch sequence.

If Chatarium crashes after `RouteDispatched` but before delivery, retry finishes
only `WorkerControlDelivered`; dispatch authority is not consumed twice.

Delivered controls project into a read-only **Worker control inbox** on the
worker conversation. The projection joins delivery provenance to the immutable
admitted control so the worker side can inspect action kind and goal identity.

Delivery does **not**:

- append an ordinary transcript message;
- enter inference context;
- mutate WorkerLifecycle;
- acknowledge that the worker acted on the command.

## Landed worker control acknowledgement

A delivered worker control may now be explicitly **Acknowledged** from the
destination worker conversation.

`WorkerControlAcknowledged` is a separate durable fact referencing the already
delivered control/route/worker/conversation provenance. Acknowledgement is
conversation-scoped, survives later SessionId rollover, and is idempotence-gated:
the same delivered control cannot be acknowledged twice.

The worker control inbox shows the durable acknowledgement event.

Acknowledgement does **not**:

- execute the command;
- mutate WorkerLifecycle;
- create a status result;
- append transcript content;
- enter inference context.

## Landed non-mutating StatusRequest results

Acknowledged `StatusRequest` controls can now produce a durable
`WorkerControlStatusResultRecorded` fact.

The result is destination-conversation-scoped and records:

- control and route identity;
- WorkerId and worker conversation identity;
- current WorkerGoalId;
- current WorkerPhase;
- acknowledgement sequence;
- result sequence.

The result is accepted only when the delivered inbox item is a StatusRequest,
has already been durably acknowledged, belongs to the worker conversation, and
the reported goal/phase exactly match replayed WorkerLifecycle state.

The worker control inbox exposes **Record status result** only after
acknowledgement and displays the resulting durable phase snapshot.

Status reporting does not mutate WorkerLifecycle, transcript, routing, or
inference context.

## Landed worker-side mutating control application

Acknowledged `StartOrResume` and `Stop` controls can now be explicitly
applied from the worker conversation.

Application preserves seven distinct durable boundaries:

1. command admission;
2. user-approved route dispatch;
3. durable delivery;
4. worker acknowledgement;
5. explicit worker-side action intent;
6. the normal durable WorkerLifecycle transition carrying control correlation;
7. a separately replayable control-correlated action result.

The path is crash-recoverable. Restart after action start can still perform the
missing correlated lifecycle transition; restart after that transition can add
only the missing result. An unrelated lifecycle transition after action start
causes recovery to fail closed rather than being misidentified as execution.

## Landed worker-side continuation execution and terminal result

The bounded `ContinuationLease → ContinuationPermit → Continue control` chain
is exposed through the desktop and preserved durably.

After delivery and acknowledgement, the worker conversation may explicitly
**Start continuation execution**. The persistence worker revalidates:

- Continue control identity and route ownership;
- acknowledgement provenance;
- the exact lease and permit ordinal consumed by the control;
- WorkerId and current goal identity;
- that WorkerPhase is still continuation-eligible.

A successful start records `WorkerContinuationExecutionStarted` and allocates
a fresh non-authored `LocalTurnId`. It does not mutate WorkerLifecycle, append
a user message, or enter authored-turn history.

The worker may then explicitly dispatch that execution through the existing
ChatGPT Responses transport. Context Composer supplies a typed bounded
controller-continuation source at user trust level with explicit
control/route/worker/goal/lease provenance. The request never fabricates an
`AuthoredUserMessage` such as "Continue".

Remote dispatch, acceptance, assistant output, completion, definitive failure,
and interruption remain durable turn-scoped evidence. Restart recovery records
an interruption for stranded continuation turns and backfills a terminal result
when terminal transport evidence exists.

`WorkerContinuationExecutionResultRecorded` correlates the terminal outcome
back to the original control, route, worker, goal, lease/permit, non-authored
turn, and terminal transport event. It does not mutate WorkerLifecycle.

## Landed controller worker result inbox

A read-only controller result projection now unifies terminal results for:

- StatusRequest phase snapshots;
- completed Start/Resume and Stop applications;
- bounded Continue executions, including the latest durable worker output.

The projection joins each result to the validated controller-issued route and
maps historical controller SessionId provenance back to the owning local
controller conversation lineage when available. Controller session rollover
therefore does not erase earlier results.

User-issued controls are excluded from this controller inbox. Viewing a result
does not create a route, acknowledgement, lifecycle transition, transcript
message, or inference-context item.

## Next implementation boundary

The next safe boundary is explicit controller-context admission of selected
worker results.

Controller result visibility must remain separate from model-context use.
Nothing in result replay should silently inject worker output into a controller
request or elevate it to developer/system authority.
