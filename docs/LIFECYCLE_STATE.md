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
Manual route policy, immutable routed payloads, one-shot permit-gated
dispatch, and explicit local delivery provenance are now separate durable
layers.

Delivered routed content projects into a destination-side routed inbox, not the
ordinary transcript. Context use is now separately user-controlled through
durable Admit/Exclude decisions.

Currently admitted routed items enter Context Composer at user trust level with
explicit route/payload/source provenance and are snapshotted at Send. They never
become authored transcript messages and create no automatic controller action.

WorkerId rollover behavior is now explicit: the logical WorkerId persists,
while active execution moves between SessionId leaves only through a durable
`WorkerSessionSuccessorBound` handoff. Historical predecessor bindings remain
available for point-in-time control provenance.

Local controller→worker supervision is now desktop-integrated. A local WorkerId
must first be explicitly bound or handed off to its conversation's current
SessionId. Another conversation's current non-worker session may be explicitly
controller-designated and durably bound to that active worker session.

Supervision remains provenance only: it does not itself authorize lifecycle
mutation, continuation, routing, dispatch, or hidden prompts.

Controller-issued typed `WorkerControl` actions now use the existing
admission, issuer-provenance, control-route, freshness, explicit-approval, and
one-shot dispatch machinery.

Dispatch now records a separate durable worker-control delivery fact and the
worker conversation exposes a read-only control inbox. Delivery is intentionally
not lifecycle evidence: Start/Resume, Stop, and Status Request commands do not
change WorkerLifecycle merely because they arrived.

Worker-side acknowledgement is now explicit and durable. A delivered control may
be acknowledged from the worker conversation without changing lifecycle,
transcript, or inference context.

Acknowledged `StatusRequest` controls now have a separate durable,
non-mutating status-result path. The worker conversation explicitly records a
snapshot of its already-replayed current goal and WorkerPhase. Result replay
requires the delivered control to be a StatusRequest, durably acknowledged,
owned by the destination worker conversation, and exactly consistent with the
authoritative WorkerLifecycle state at result time.

Status reporting therefore does not fabricate lifecycle evidence: it reports
existing durable state and cannot change that state.

Acknowledged Start/Resume and Stop controls now have an explicit worker-side
application path. The application audit durably separates action start, the
ordinary WorkerLifecycle transition carrying control correlation, and the final
action result. Restart can finish a partially applied action without repeating a
completed transition, while conflicting unrelated lifecycle changes fail
closed.

Continue remains deliberately outside that lifecycle-action audit.

Bounded Continue authority is now desktop-integrated end to end on the
controller side: explicit ContinuationLease creation, durable permit issuance,
permit consumption into one Continue control, approval, dispatch, delivery, and
worker acknowledgement all remain distinct.

The worker side can explicitly start continuation execution. That durable start
validates the acknowledged delivered Continue control, the exact consumed
lease/permit, current WorkerId/goal ownership, and a continuation-eligible
Working phase. It allocates a fresh non-authored LocalTurnId for the execution
and does not mutate WorkerLifecycle or append transcript content.

Non-authored continuation transport is now landed as well. Context Composer
adds a typed, user-level bounded-controller-continuation source that explicitly
states it is orchestration context rather than user authorship or a
developer/system instruction. The execution turn then uses the normal durable
remote observation kinds under its own LocalTurnId without ever creating a
UserMessageCommitted event.

Restart recovery preserves this distinction: an in-flight continuation turn is
durably interrupted, and terminal completion/failure/interruption is projected
into a separate WorkerContinuationExecutionResultRecorded fact. Terminal
continuation results do not mutate WorkerLifecycle.

Controller-side result visibility is unified through a result inbox over
StatusRequest results, completed Start/Resume/Stop applications, and terminal
Continue executions. Continue results include the latest durable worker output
text.

Controller result visibility and model-context use are now separate durable
layers. Every result is context-excluded by default. The controller conversation
may explicitly Admit or Exclude an individual terminal result, and that
reversible decision is validated against the exact control/route/result
provenance.

Currently admitted results enter Context Composer only as user-level
orchestration-result context wrapped in an explicit Chatarium provenance
envelope. The envelope states that the result is not user-authored and is not a
developer/system instruction. The admitted set is snapshotted when Send is
clicked so a later context decision cannot mutate an in-flight authored turn.

Non-authored controller coordination is now explicit and durable.

A controller coordination turn can start only from a currently
controller-designated local conversation with at least one already-admitted
terminal worker result. The start event snapshots the exact admitted result-route
set and allocates a fresh non-authored LocalTurnId.

**Start coordination turn** and **Dispatch coordination** are separate user
actions. Dispatch composes from the journal prefix frozen at coordination start,
so later result Admit/Exclude changes cannot mutate the coordination request.
Context Composer adds a user-level orchestration marker that explicitly says the
turn is not user-authored, is not a developer/system instruction, and has no
authority to issue controls, mutate lifecycle, or consume continuation permits.

Remote transport evidence is durable under the coordination LocalTurnId.
Completion, observed failure, or interruption produces a separate durable
controller-coordination result. Restart recovery marks an in-flight coordination
turn interrupted and records that terminal result without fabricating an authored
message.

Coordination-result context admission is now explicit and reversible.

A completed coordination synthesis remains visible but context-excluded by
default. The controller conversation may explicitly Admit or Exclude the exact
terminal coordination result. Admission references the durable coordination turn
and terminal-result sequence; it does not duplicate output text.

Currently admitted coordination results enter **ordinary future controller
inference** as user-level orchestration-result context with turn/outcome/result/
admission provenance. They never become authored transcript messages. The
admitted set is snapshotted at authored Send. Special non-authored coordination
dispatch deliberately does not recursively ingest prior coordination results;
that would require a separate explicit policy rather than emerging from ordinary
context admission.

Typed, non-authoritative coordination suggestions are now landed.

A completed coordination result may be manually encoded into one or more durable
typed suggestions. Each suggestion has its own `CoordinationSuggestionId` and
records:

- the exact completed coordination turn/result;
- one exact worker-result route frozen into that coordination turn as its basis;
- the worker conversation, WorkerId, and goal carried by that basis result;
- one proposed action: Start/Resume, Continue, Stop, or StatusRequest.

Suggestion replay requires the basis route to have actually participated in the
coordination snapshot. Recording a suggestion appends only the suggestion fact:
it creates no WorkerControl, route, policy decision, lifecycle transition,
continuation authority, dispatch, or inference-context item.

Promotion is a separate explicit user action. Before promotion, persistence
revalidates that the target conversation still owns the same WorkerId and that
the worker is still on the suggestion's frozen goal. Stale suggestions therefore
fail without creating a control.

A valid promotion then traverses the existing controller-control proposal path:
typed control admission, controller issuer provenance, RequireApproval route,
and control↔route correlation are written first. Only after that path replays as
valid does Chatarium append a suggestion-promotion correlation. The resulting
route remains **PendingApproval**; promotion never approves or dispatches it.

Structured coordination suggestion candidates are now landed.

New coordination starts record the versioned `suggestion_candidates_v1`
contract. The coordination marker requests a strict JSON summary plus
`basis_result_route_id`/action candidate pairs.

Candidate parsing is deliberately **not** an audit. It is a read-only projection
over terminal model output. Chatarium rejects malformed/extra fields, unknown or
non-frozen routes, unsupported actions, duplicate route/action candidates, and
oversized candidate sets. The model never supplies trusted WorkerId/goal
provenance; those identities are joined back from the frozen worker-result route.

The UI labels candidates **UNTRUSTED**. Explicit **Accept candidate** is required
before the existing durable `CoordinationSuggestionId` layer is touched.
Acceptance still creates no WorkerControl. Promotion remains separate, and route
approval/dispatch remain separate after promotion.

The controller reasoning/control loop is therefore end-to-end while preserving
human authority at every state-changing boundary.

Durable local memory is now a distinct provenance/admission domain; see
[LOCAL_MEMORY.md](LOCAL_MEMORY.md).

It remains deliberately separate from WorkerLifecycle, routing, supervision,
coordination, and transcript history. A lifecycle transition never writes
memory, and a memory artifact never changes lifecycle merely because it is
recorded or admitted.

Memory supersession is now explicit and forward-only. Superseded artifacts remain
historically visible, are mechanically excluded from effective context, and do
not transfer admission to their successors.

Read-only local memory discovery/search is now landed. It is a pure projection:
searching creates no memory, context admission, lifecycle change, route, or
control.

The next memory boundary is optional explicit organization metadata such as
user-authored labels. Lifecycle semantics remain unchanged by memory recording,
admission, supersession, discovery, or labeling.
