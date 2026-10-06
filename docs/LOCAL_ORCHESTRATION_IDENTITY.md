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

## Next implementation boundary

The next local-first edge is explicit
`current SessionId → RouteEndpointId` addressability using the existing
session audit.

That endpoint binding remains identity/correlation only. Only after it exists
should controller/worker route proposal and policy consume the existing
RouteGate machinery.

No hidden cross-conversation transcript sharing is authorized by topology or
addressability alone. Routed or shared context remains a separate explicit
policy/source.
