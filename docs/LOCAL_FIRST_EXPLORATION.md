# Local-first exploration phase

Status: **ACTIVE** as of 2026-10-05.

This document records a deliberate project pause, not an abandonment of the self-sustaining transport requirement.

## What is paused

The following work is paused until the principal explicitly unpauses it:

- ChatGPT website conversation mirroring;
- Chromium/CDP/Playwright transport work for ordinary ChatGPT conversation reads or writes;
- remote-history discovery and account-wide mirror expansion;
- browser-extension protocol capture undertaken solely to advance remote conversation interoperability;
- direct first-party `chatgpt.com` transport experiments;
- Cloudflare/rate-limit/authentication investigation for that browser-assisted path;
- P3V native-transport viability experiments.

Do not restart any of those tracks because a later task merely mentions ChatGPT, synchronization, history, mirroring, Chromium, or protocol work. The principal must explicitly unpause the remote/browser track.

Existing code and evidence remain preserved. Do not delete the browser bridge, remote mirror queue, protocol corpus, local snapshots, or self-sustaining transport contract merely because active work has shifted.

## Active direction

The active product exploration is now **local-first Chatarium conversations** using the already-working Sign in with ChatGPT plan-usage / Responses path plus Chatarium's own durable local conversation state.

The purpose of this phase is to find out how much of the intended "fancy behavioral" product can be built and learned locally before returning to remote ChatGPT-history interoperability.

The exact current inference/control matrix lives in [LOCAL_INFERENCE_CAPABILITY_SURFACE.md](LOCAL_INFERENCE_CAPABILITY_SURFACE.md), and the machine-readable freeze gate is defined in [LOCAL_INFERENCE_CONTRACT.md](LOCAL_INFERENCE_CONTRACT.md). The near-term rule is: expose and understand the knobs we actually have before building elaborate lifecycle behavior. Empirical remote capabilities may enter behavior/lifecycle code only through a `ready` contract scoped to the active SIWC profile and model.

Priority areas include:

- multiple durable local conversations;
- model selection and supported inference controls;
- explicit per-conversation state;
- local conversation isolation;
- master/controller and worker relationships;
- worker lifecycle and continuation semantics;
- local routing and cross-conversation messaging;
- local memory/context artifacts under explicit Chatarium control;
- MCP/tool integration;
- session rollover and continuity;
- user-supervised policy and provenance;
- behavioral experiments that do not require access to the ChatGPT website history.

## Cost and account semantics

The current local inference path uses OpenAI Sign in with ChatGPT plan usage rather than a user-supplied API key.

For eligible Plus/Pro accounts, eligible inference requests consume the ChatGPT usage included in the user's plan. They are not ordinary separately metered API-key requests.

This is not "unlimited free inference": requests consume the user's ChatGPT plan allowance. If the user separately enables use of ChatGPT credits after plan limits, eligible requests may consume those credits. Do not silently enable or assume paid credit spillover.

Chatarium itself must not introduce a separate paid inference requirement for this local-first phase without explicit principal approval.

## Relationship to ChatGPT account history and memory

Sign in with ChatGPT plan usage does not, by itself, grant Chatarium access to the user's ChatGPT conversations or ChatGPT memory.

Therefore local-first conversations must be treated as their own Chatarium state.

Do not imply that a local conversation automatically knows:

- the user's chatgpt.com conversation history;
- other Chatarium conversations;
- ChatGPT memory;
- project context from the ChatGPT website;
- another local conversation's transcript.

Any cross-conversation knowledge must come from an explicit Chatarium mechanism: routing, shared local context, a user-authorized memory artifact, retrieval, or another documented state transfer.

## Current conversation-state implementation

The current Responses path is local-state-driven:

1. a user message is durably committed to Chatarium first;
2. Chatarium projects the visible transcript for the active `LocalConversationId`;
3. that conversation's user/assistant messages are assembled into the Responses `input`;
4. the selected account-visible model is invoked;
5. assistant deltas/completion are durably recorded back into Chatarium.

The remote request is therefore not relying on a persistent server-side Chatarium thread. Chatarium supplies the conversation context.

This is the desired local-first property: conversation continuity is locally owned and inspectable.

### Isolation rule

One local conversation must not automatically receive another local conversation's transcript.

Per-conversation state is isolated unless an explicit higher-level Chatarium feature routes or shares context.

This rule is especially important for future master/worker behavior: cross-session communication should be visible, attributable, durable, and policy-controlled rather than emerging from accidental shared hidden context.

## Current control surface vs. ChatGPT website

The local inference path is **not currently feature-identical to the ChatGPT website**.

Already present:

- ChatGPT account sign-in/plan usage;
- account-specific model discovery;
- local model selector;
- durable local transcript;
- streamed assistant output;
- local crash/restart persistence.

Not yet equivalent to the site:

- full ChatGPT history;
- ChatGPT memory;
- the site's complete model/reasoning control surface;
- the site's tool/file/project feature set;
- browser/site-specific conversation identity and branching behavior.

This phase should explore which useful controls can be added locally from supported Responses capabilities without pretending they already exist.

## Exit / unpause condition

Remote/browser transport work remains paused until the principal explicitly says to resume it.

The self-sustaining native transport contract in `SELF_SUSTAINING_TRANSPORT.md` remains the long-term viability requirement. Pausing that investigation does not weaken it.

During this phase, optimize for learning what Chatarium can become when its conversations, state, orchestration, and behavioral machinery are local-first.


## Landed local-first substrate

The first local conversation workspace/inference-control milestone is now implemented.

Current local-first substrate:

- multiple isolated local conversations;
- durable active-conversation selection;
- create/switch/rename/archive/restore workspace controls;
- per-conversation draft isolation;
- per-conversation persisted model choice;
- per-conversation top-level instructions;
- per-conversation developer context;
- streamed assistant output;
- Stop generation / active-request cancellation;
- an Exact next-request context inspector;
- an in-app fixed SIWC capability-probe suite with sanitized durable evidence;
- a derived, typed, profile/model-scoped Local Inference Contract with `ready`, `needs_review`, and `incomplete` states;
- local backup/restore coverage for conversation workspace metadata, inference settings, Behavior Profiles, probe evidence, and the derived contract.

This is intentionally enough substrate to begin serious context-policy and behavioral experiments once the empirical contract is `ready`.

## Empirical gate closed — 2026-10-06

The first real in-app fixed-suite run against `gpt-5.6-sol` completed with all
nine named probes classified `supported`, and Chatarium derived a `ready`
Local Inference Contract. The repository does not copy the user-specific SIWC
profile identifier; the local contract artifact remains the machine-readable
scope authority.

The speculative-capability phase is therefore closed for this model/profile
scope. Do not add more capability-probe scaffolding merely to postpone
higher-level product work.

## Context Composer and Behavior Profile landed

Context Composer is documented in
[CONTEXT_COMPOSER.md](CONTEXT_COMPOSER.md).

Both live post-durable-commit dispatch and the **Exact next-request context**
inspector consume the same typed local `ContextPlan`. The composer now has
explicit preview/dispatch inclusion policy, local source provenance, exact
byte/character/line accounting, and Local-Inference-Contract-gated capability
slots.

The first durable per-conversation Behavior Profile is documented in
[BEHAVIOR_PROFILE.md](BEHAVIOR_PROFILE.md). It currently exposes only exact
empirically accepted values for low reasoning, low verbosity, and web search.
Those settings are snapshotted at commit time and fail closed if the active
profile/model contract no longer admits them.

The first local-first Lifecycle State slice is now landed and documented in
[LIFECYCLE_STATE.md](LIFECYCLE_STATE.md). A local conversation can opt into the
existing durable WorkerLifecycle state machine through an explicit one-to-one
LocalConversationId→WorkerId journal binding. The desktop exposes manual,
validated lifecycle transitions only; no automatic continuation, prose-based
state inference, or hidden prompt injection is introduced.

The local-conversation identity bridge into the orchestration continuity
plane is now landed: each opted-in conversation can durably own a logical chat
container whose current local session is projected from the existing rollover
audit.

Current-session routing addressability
(`SessionId → RouteEndpointId`) and the deterministic read-only local routing
directory are now explicit and durable.

Manual local `SessionMessage` route proposals now consume that directory and
the existing `RouteGate` audit. Every proposal requires explicit approval;
Allow/Deny decisions are durable and fail closed when route endpoints become
stale.

Routes now carry a separate immutable exact-text payload with point-in-time
conversation provenance. Allow is blocked until that payload is durable.

Approved routes can now consume one-shot dispatch authority and durably record
delivery to the destination's current local session leaf. Delivered items
project into a separate routed inbox with source/route/payload/session
provenance. They never become user-authored transcript messages.

Routed context admission is now explicit and reversible. The destination user
may Admit or Exclude each delivered item. Currently admitted items are
snapshotted when Send is clicked and enter Context Composer as user-level peer
content wrapped in visible Chatarium routing provenance. Delivery alone remains
context-inert.

The manual local routing stack is therefore vertically complete enough for
behavioral experiments without hidden cross-conversation state.

WorkerId semantics across session rollover are now frozen: WorkerId persists,
SessionId is replaceable, and active execution moves only through an explicit
durable worker-session successor handoff. Historical controller provenance and
orchestration routes are validated against the worker session active at their
own event sequence.

Explicit local controller→worker supervision is now landed in the desktop:
WorkerId is bound/advanced to the worker conversation's current SessionId,
controller designation is explicit, and controller→worker-session correlation
is durable and inspectable. Supervision alone grants no mutation or dispatch
authority.

The active next architectural boundary is controller-issued typed worker
controls through the existing control admission/provenance/routing/dispatch
stack. Durable local memory remains a separate later source. Do not reopen
browser/history work as part of this phase.


## Controller control delivery landed

Controller-issued typed worker controls now pass through durable admission,
controller provenance, route correlation, explicit approval, one-shot dispatch,
and a separate worker-side delivery fact. Delivered controls project into a
read-only worker control inbox and do not mutate lifecycle, transcript, or
inference context.

Worker-side control acknowledgement is now landed as a separate durable fact and
does not mutate lifecycle.

Acknowledged StatusRequest controls now have an explicit durable worker-side
result. Recording that result snapshots the already-existing WorkerLifecycle
goal/phase; it does not mutate lifecycle. The worker control inbox exposes the
recorded phase and result-event provenance.

Explicit worker-side application of acknowledged Start/Resume and Stop controls is
now landed. Application is crash-recoverable and split into separate durable
facts for action intent, the control-correlated normal WorkerLifecycle
transition, and the final action result. Acknowledgement alone never implies
execution, and an unrelated lifecycle transition cannot satisfy a control
action.

Bounded Continue authority is explicit through a finite
ContinuationLease and one consumed ContinuationPermit per Continue control.
Worker acknowledgement can advance to a durable worker-side continuation
execution with its own non-authored LocalTurnId.

Continuation transport is now end to end: a typed controller-continuation
Context Composer source feeds the worker request without fabricating user
authorship, remote transport evidence is durable under the execution turn,
restart recovery preserves interrupted execution, and terminal outcomes are
recorded in a separate continuation-result audit without mutating
WorkerLifecycle.

Controller conversations now have a read-only worker result inbox that unifies
StatusRequest snapshots, Start/Resume/Stop action results, and terminal Continue
results including durable worker output. Results remain outside controller
inference context by default.

Controller-result context admission is now explicit and reversible. Selected terminal
worker results enter controller Context Composer only after an Admit decision,
at user-level trust with control/route/worker/goal/result provenance. The
admitted set is snapshotted at authored Send.

A deliberate non-authored controller coordination turn is now landed on top of
that substrate. Starting the turn snapshots the exact currently admitted worker
result routes; Dispatch is a second explicit user action. The coordination
request contains an explicit non-authored orchestration marker and cannot by
itself issue controls, mutate WorkerLifecycle, or consume continuation
authority. Terminal coordination output is durable and restart-recoverable but
does not become ordinary transcript content.

Completed controller coordination results now have the same visible context-admission
boundary. A terminal coordination synthesis is excluded by default and can be
explicitly Admitted or Excluded. Admitted coordination results enter ordinary
future controller requests at user-level trust with coordination-turn/outcome/
result/admission provenance, are snapshotted at Send, and never become authored
transcript content.

This admission does **not** recursively feed old coordination output into the
special non-authored coordination workflow. That remains intentionally separate.

Typed, non-authoritative coordination suggestions are now durable and exposed in
the controller UI.

A completed coordination turn can record suggested Start/Resume, Continue, Stop,
or StatusRequest actions only against worker-result routes that were frozen into
that turn's coordination snapshot. Suggestions have their own identity and carry
worker/goal/basis-result provenance. They are explicitly labeled **NO AUTHORITY**
and create no control, route, lifecycle, continuation, or dispatch state.

The user may explicitly **Promote suggestion**. Promotion fails closed when the
worker conversation or current goal no longer matches the suggestion. A valid
promotion uses the existing controller-control proposal machinery and records a
separate suggestion→control/route correlation only after the real control path
validates. The promoted route still waits for explicit Allow/Deny and one-shot
dispatch.

Machine-readable coordination suggestion candidates are now landed.

New coordination turns durably declare the versioned
`suggestion_candidates_v1` output contract. Context Composer asks the model for
exactly one JSON object containing a human-readable summary plus candidate
`basis_result_route_id`/action pairs. Model output is parsed as a **read-only,
untrusted projection**:

- only routes frozen into the coordination turn are accepted;
- action names must be one of the four typed suggestion actions;
- WorkerId, WorkerGoalId, and worker-conversation identity are re-derived from
  authoritative frozen worker-result provenance rather than trusted from model
  output;
- extra fields, unknown actions/routes, duplicate route/action candidates,
  wrappers, malformed JSON, and oversized candidate sets fail closed;
- parsing creates no journal event, suggestion identity, control, route,
  lifecycle transition, continuation authority, or dispatch.

The controller UI exposes those rows as **UNTRUSTED** candidates. **Accept
candidate** is the explicit user boundary that records the existing powerless
durable suggestion. Promotion remains a second explicit action, and the promoted
control route still requires ordinary Allow/Deny plus one-shot dispatch.

This completes the first controller reasoning loop without giving model output
authority over workers.

Explicit durable local memory is now landed and documented in
[LOCAL_MEMORY.md](LOCAL_MEMORY.md).

Memory is its own immutable artifact domain with `LocalMemoryId`, exact text,
source-conversation provenance, reversible per-destination Admit/Exclude
decisions, archive-integrity replay, and a typed Context Composer source.
Recording memory never auto-admits it. Cross-conversation reuse requires an
explicit Admit decision for the destination conversation, and the admitted set
is snapshotted at Send.

No transcript sharing, routed-message delivery, coordination result, lifecycle
state, or model output silently becomes memory.

Memory supersession, read-only discovery, durable user-authored labels, exact
label facets, and exact source-conversation facets are now landed. Superseded
predecessors remain historically visible but mechanically excluded from
effective context; labels and facets never grant context authority.

Next-request-only manual memory selection is now landed.

A discovered eligible memory can be selected for exactly one authored request
without changing persistent Admit/Exclude state. The exact sorted memory set and
journal high-water are frozen at Send, validated before the authored message
commit, durably correlated to that LocalTurnId after commit, exposed in the
exact-request inspector, and reconstructed as typed user-level one-shot memory
context. The pre-send selection is then cleared and does not leak into later
turns.

Superseded artifacts and already-persistently-admitted artifacts are ineligible
for redundant one-shot selection.

The explicit manual memory stack is therefore complete enough for dogfooding.
Automatic semantic retrieval and embedding search remain later.

The first **MCP/tool integration substrate** is now landed: typed
provider/tool-call identities, durable provider endpoint correlation, immutable
call argument/provenance records, explicit RequireApproval ToolCall routing,
manual Allow/Deny controls, and a separate terminal tool result/error audit.
The original XML-like tool envelope has been recovered from ChatGPT Tool
Shim and ported to a bounded pure Rust parser/formatter. A side-effect-free
local `chatarium.builtin` / `hello` adapter requires the immutable call,
explicit Allow, and a separate dispatch click.

Explicit reversible tool-result context admission is implemented separately
from execution authority. The MCP 2026 codec, durable external stdio provider
configuration and activation, read-only preflight, executable inspection, and
one-shot Linux sandboxed runner are landed. Configuring and activating a
provider never executes it. Individual user-approved calls require a further
one-shot Run action and launch inside a strict, network-isolated Linux sandbox.
The subprocess now runs off the persistence worker, with its terminal
observation separately appended through the durable journal.

The next boundary is wider Linux-host compatibility testing and incremental
adapter functionality without weakening explicit permission or sandbox
constraints. Arbitrary host commands, automatic model-driven tool execution,
browser access, network access, and hidden tool-result context admission are
not authorized. See [LOCAL_TOOL_INTEGRATION.md](LOCAL_TOOL_INTEGRATION.md).
Do not reopen browser/history work as part of this phase.
