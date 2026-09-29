# Product vision

Chatarium is not intended to stop at a reliable single-chat replacement UI.

Its long-term product is a **user-controlled desktop interaction surface for ChatGPT**: a native application that speaks to the consumer ChatGPT service, preserves its own durable local state, exposes a first-class tool/MCP integration surface, and can coordinate multiple ChatGPT sessions under visible user supervision.

This document records product intent. It does **not** claim that every capability below is implemented today.

## Product identity

The closest conceptual ancestor is the Braizen-style browser idea: a general programmable browser/control surface that gradually cohered around ChatGPT as the primary application.

Chatarium takes that convergence seriously.

The intended endpoint is not:

> a generic web browser with a ChatGPT tab

and not merely:

> a prettier ChatGPT client

It is:

> **a custom-made desktop workstation for interacting with ChatGPT, integrating tools, coordinating multiple ChatGPT sessions, and making the routing/control plane visible to the user.**

The current protocol observatory, durability core, remote adapter, and egui client are foundations for that workstation rather than the final scope.

## 1. Native ChatGPT interaction surface

Chatarium should ultimately replace the ordinary ChatGPT site as the user's preferred operational surface where practical.

It should own the local experience around:

- conversation browsing;
- durable composition;
- transcript rendering;
- remote/local reconciliation;
- attachments;
- search;
- model/reasoning controls when empirically supported;
- protocol compatibility diagnostics;
- local project/session organization;
- tool calls;
- multi-session orchestration.

The official ChatGPT service remains the remote inference/product backend. Chatarium supplies the local reliability, orchestration, observability, and extensibility layer.

## 2. MCP and tool integration surface

Chatarium should provide a first-class surface for integrating MCP tools and comparable local/external capabilities.

This is not an afterthought attached to one chat window. Tool invocation is part of the application's message/control architecture.

### XML compatibility

The user already has an XML-oriented format used in the Braizen concept and in a ChatGPT JavaScript shim for representing/dispatching MCP-style tool calls.

Chatarium should **reuse or adapt that existing XML envelope where practical rather than inventing an unrelated tool-call language by default**.

The exact schema is not yet imported into this repository, so its details remain an external design dependency until the existing format is recovered and versioned here.

When formalized, the format should preserve at least:

- message/tool-call identity;
- source session;
- destination/tool;
- operation name;
- arguments/payload;
- result;
- error;
- correlation between request and response;
- explicit completion/terminal state;
- provenance and timestamps where appropriate.

XML is a transport/representation choice, not permission authority. Permission decisions belong to Chatarium's routing/policy layer.

### Tool provenance

Every tool action routed through Chatarium should be attributable to:

- the session that requested it;
- the tool/provider that received it;
- the exact request envelope;
- the returned result/error;
- the user/policy decision that allowed, blocked, or modified the route.

Tool activity should be durable and inspectable rather than hidden inside transient UI state.

## 3. Multi-session orchestration

Chatarium should support multiple concurrent ChatGPT sessions whose relationships are explicit.

A user may designate one conversation/session as a **master/controller session** and others as **worker sessions**.

The master is allowed to coordinate work across workers through Chatarium's routing plane.

Examples include:

- assign or update a worker's goal;
- send a continuation instruction;
- ask a worker for status;
- deliver context or corrections;
- tell a worker to stop;
- redirect work to a different worker;
- collect worker results back into the master session.

The product should not require the human to manually copy/paste every coordination message between sessions.

### "Continue" as an orchestration primitive

A common case is intentionally simple:

- a worker is already executing a goal;
- the master determines more work remains;
- the master emits a continuation instruction;
- Chatarium routes the instruction to that worker.

The control plane should support this directly instead of forcing the master to recreate the worker's entire task context every turn.

## 4. Explicit worker lifecycle language

Autonomous sessions must not blindly loop each other.

Chatarium therefore needs a small, explicit orchestration vocabulary/state machine separate from ordinary conversational prose.

The exact wire syntax is still to be designed, but the semantic states should distinguish at least concepts such as:

- goal assigned / updated;
- working;
- progress/status;
- needs input;
- blocked;
- continue requested;
- stop requested;
- completed;
- failed.

A worker's **completed** state must be machine-recognizable so a controller does not keep sending `continue` after the work is actually done.

Likewise, absence of a completion marker must not automatically mean "keep looping forever."

Future design should prefer explicit bounded state transitions and correlation IDs over prompt-text heuristics such as searching ordinary prose for the word "done."

## First executable orchestration invariant

Chatarium now treats worker lifecycle as a machine-readable domain concept separate from ordinary assistant prose. The first core invariant is intentionally small: automatic continuation is permitted only while a worker is actively `Working` and only while an explicit bounded continuation lease still has allowance.

`NeedsInput` and `Blocked` halt automatic continuation until an explicit resume decision occurs. `Completed`, `Failed`, and `Stopped` are terminal for the current goal. Goal-correlated continuation authority cannot be reused against a replacement goal, so stale controls do not silently operate on a new lifecycle.

This is not live multi-session routing yet. It is the pure domain contract that later master/worker routing, GUI supervision, and durable orchestration must preserve.

Worker identity and lifecycle are now also restart-safe through the authoritative journal. Each local worker's current goal and machine-readable phase can be reconstructed independently after a crash/restart, including attention-required and terminal states. Durable replay does not recreate continuation leases; any new continuation authority after restart must come from a fresh explicit decision.

Orchestration controls are now also modeled as typed semantic commands separate from ordinary prose. Start/resume, continue, stop, and goal-correlated status requests are admitted against a worker lifecycle snapshot before any future transport is involved. A continue command consumes one opaque bounded continuation permit, so that authority cannot be trivially copied/reused. Command admission itself never mutates worker state: Chatarium changes lifecycle only when later evidence explicitly records the transition.

Those admitted commands are now restart-safe audit facts as well. Chatarium can reconstruct which control it admitted and its worker/goal/continuation correlation after a crash without confusing admission with dispatch or confusing dispatch with a worker transition.

An admitted worker control can now also be explicitly correlated to exactly one `OrchestrationControl` route, with the reciprocal rule that one route carries at most one admitted control. That binding is durable provenance only: it does not mean the route was approved, dispatched, delivered, executed, or reflected in worker lifecycle. Worker-to-session/endpoint identity remains a separate future layer.

## 5. User-supervised routing plane

Cross-session and tool communication must remain visible and contestable by the user.

The egui application should eventually expose a supervisory surface where the user can watch messages move through the system.

Conceptually:

```text
master session
      |
      v
+-----------------------+
| Chatarium routing     |
| / policy plane        |
+-----------------------+
   |        |        |
   v        v        v
worker A  worker B   MCP/tool
```

For each routed item the UI should be able to show, as appropriate:

- source;
- destination;
- message/control type;
- correlation/goal identity;
- queued/dispatched/acknowledged/completed state;
- whether a policy allowed it automatically;
- whether user approval is pending;
- result/error.

## 6. Allow / block / forbid / approve controls

The user must remain sovereign over the routing plane.

Chatarium should support policy decisions such as:

- **allow** — permit this route/action;
- **block/forbid** — prevent this route/action;
- **require approval** — pause before dispatch and ask the user;
- future scoped rules such as allow/deny by session, tool, operation, project, or message class.

The exact policy language is future work, but the architecture must not make cross-session automation an opaque unstoppable loop.

A master session is a coordinator, not an authority above the user.

The GUI must provide a way to interrupt, inspect, and override orchestration.

## Second executable orchestration invariant

Cross-session/tool dispatch is now modeled as a one-shot policy gate before any transport exists. A route begins under one of three explicit policy requirements: automatic allow, automatic deny, or require user approval. No dispatch permit can be produced while approval is pending or the route is denied, and a permit can be consumed only once.

The user may approve a pending route, deny a pending route, veto an automatically allowed route, or explicitly override an automatic denial before dispatch. A master/controller session has no special authority to bypass that gate. After dispatch, later policy changes do not rewrite history into "never dispatched"; the dispatched state remains observable.

This remains pure domain state. The future egui supervisor, MCP/tool adapters, and master/worker transport must use this gate rather than inventing a hidden bypass.

Routing audit facts are now also designed to survive restart through the authoritative append-only journal. A proposed route, explicit user decision, consumed dispatch authorization, and a generic post-dispatch result/error observation can be reconstructed without treating SQLite as authority. The audit layer intentionally records no arbitrary chat/tool payload and does not equate transport/result observations with worker-goal completion.

## 7. Message bus, not hidden automation

The preferred architecture is an explicit local message/control bus.

Ordinary ChatGPT messages, orchestration messages, MCP/tool envelopes, approvals, denials, results, and lifecycle transitions should be distinguishable message classes while sharing:

- durable identity;
- provenance;
- routing;
- correlation;
- policy evaluation;
- inspectability.

This gives Chatarium one coherent substrate rather than separate ad-hoc automation systems.

A future implementation may look conceptually like:

```text
                    +----------------------+
                    | supervisory egui UI  |
                    | inspect / allow /     |
                    | block / redirect      |
                    +----------+-----------+
                               |
                               v
+-----------+        +----------------------+       +-----------+
| master    |<------>| durable routing /    |<----->| worker(s) |
| session   |        | policy bus           |       |           |
+-----------+        +----+-------------+---+       +-----------+
                          |             |
                          v             v
                     MCP/tools     ChatGPT remote
```

The durable local event model should eventually extend naturally into this plane rather than treating orchestration traffic as ephemeral side-channel state.

## 8. Relationship to current architecture

The current roadmap remains valid and is prerequisite work:

- P1 gives Chatarium empirical knowledge of ChatGPT's mutable protocol.
- P2 gives it durable local authority and crash recovery.
- P3 gives it read-only remote synchronization.
- P4 gives it direct reliable text turns.
- P5 turns that reliable substrate into the native desktop workstation.

The capabilities in this document become increasingly relevant once the direct text/session path is trustworthy.

They must not be used as justification to skip the reliability/protocol foundations.

## 9. Architectural consequences

Future work should preserve the following now, even before orchestration is implemented:

1. **Session identity must be first-class.** Do not assume there is only one active conversation.
2. **Message provenance must be first-class.** Human, master-session, worker-session, tool, and system-generated actions must be distinguishable.
3. **Routing must be explicit.** A message should have a knowable source and destination.
4. **Control messages must not masquerade as ordinary user prose.**
5. **Terminal state must be explicit.** Automation cannot depend on vague textual inference that work is complete.
6. **Policy evaluation must be interceptable.** The user must be able to stop or forbid a route before dispatch where policy requires it.
7. **Automation state must be durable.** A Chatarium restart must not erase which goal was assigned, which message was dispatched, or whether work had already completed.
8. **No infinite mutual prompting by default.** Cross-session automation requires bounded state/lifecycle rules.
9. **MCP/tool calls must be auditable.** Hidden tool invocation is contrary to the supervisory product model.
10. **The user remains the highest authority.** Master/worker hierarchy is an orchestration convenience, not a transfer of control away from the operator.

## 10. Scope boundary

Chatarium may eventually resemble a specialized browser, agent router, MCP host, and conversation workstation at the same time.

That breadth is intentional **when it serves interaction with ChatGPT and the user's local control plane**.

The project does not need to return to being a general-purpose browser merely to justify those capabilities. The browser concept has cohered around ChatGPT.

Likewise, Chatarium should not be artificially narrowed to "just send text to ChatGPT." The long-term system includes the surrounding coordination and tool surface described here.

## Definition of long-term success

Chatarium succeeds as a product when the user can:

- use it as the preferred desktop surface for ChatGPT;
- retain durable local authority over authored/observed conversation state;
- attach MCP/tools through a familiar versioned envelope;
- run several ChatGPT sessions concurrently;
- appoint a master/controller session to coordinate worker sessions;
- safely issue goal updates and continuation instructions;
- detect worker completion without accidental infinite loops;
- watch cross-session/tool traffic from one GUI;
- allow, block, forbid, approve, or interrupt routes;
- reconstruct afterward what was sent, where, why, and what happened.

That is the intended product shape.