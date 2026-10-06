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

Exactly how worker identity follows a future session rollover must remain
explicit; do not silently infer it from numeric or creation order.

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

## Worker rollover question

The existing repository has both conversation-level lifecycle goals and
WorkerId↔SessionId machinery from the broader orchestration design.

The local-first worker bridge currently attaches WorkerId to the durable local
conversation so worker lifecycle can be explored without fabricating a remote
session.

Before controller-issued routing is activated, Chatarium must explicitly decide
and enforce whether a worker identity:

- persists across a chat-container session successor; or
- is replaced and handed off to a successor worker identity.

Do not resolve this implicitly by adding a second contradictory binding.

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

## Next implementation boundary

The next safe substrate is an explicit durable context-admission decision for
one delivered routed inbox item.

Admission must be separate from delivery. A delivered message should remain
visible even when it is never admitted to model context.

After durable admission exists, Context Composer can gain a typed routed-context
source with explicit provenance and an intentionally chosen request role/trust
policy. Do not silently serialize routed text as user-authored or developer
instructions.

No hidden cross-conversation transcript sharing is authorized by topology,
addressability, route proposal, approval, payload attachment, dispatch, or
delivery alone.
