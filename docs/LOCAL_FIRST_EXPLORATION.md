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

The active next architectural boundary is explicit current-session routing
addressability (`SessionId → RouteEndpointId`), then visible routing/memory and
controller/worker orchestration. Do not reopen browser/history work as part of
this phase.
