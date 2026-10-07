# Context Composer

Status: **ACTIVE**, source-policy/capability-admission boundary landed 2026-10-06.

The Context Composer is the deterministic local boundary between Chatarium's
durable conversation state and a concrete inference request.

It is downstream of the Local Inference Contract and upstream of behavior
profiles, lifecycle state, routing/memory, controller/worker orchestration, and
tool policy.

## Admission boundary

The local-first sequence is:

`LOCAL CONVERSATIONS → CAPABILITY EVIDENCE → READY LOCAL INFERENCE CONTRACT → CONTEXT COMPOSER → BEHAVIOR PROFILE → LIFECYCLE STATE → ROUTING/MEMORY → CONTROLLER↔WORKERS → MCP/LOCAL TOOLS`

The first real in-app capability run on 2026-10-06 against
`gpt-5.6-sol` produced a `ready` Local Inference Contract. Every named probe
in the fixed nine-probe matrix was classified `supported`:

- baseline;
- image input;
- file input;
- function tools;
- additional tools;
- web search;
- reasoning controls;
- verbosity control;
- structured output.

The user-specific SIWC profile identifier is intentionally not copied into this
repository document. The local `local-inference-contract.json` remains the
machine-readable scope authority.

## Current executable slice

`apps/desktop/src/context_composer.rs` owns a typed `ContextPlan`.

The current plan composes:

1. optional top-level Responses `instructions`;
2. optional per-conversation developer context as the first input message;
3. explicitly admitted routed inbox items, ordered by their durable admission
   decision alongside durable conversation events;
4. the active local conversation's durable user/assistant transcript in durable
   sequence order;
5. the current draft only for the inspector preview, never for live dispatch
   before the normal durable-commit gate completes.

The resulting wire input contains only the Responses role/content shape.
Chatarium-local provenance stays local.

For durable transcript items the plan retains the journal sequence that supplied
the content. It can therefore distinguish:

- conversation developer context;
- durable transcript context;
- explicitly admitted routed inbox context;
- current non-durable draft preview.

Routed context is different from ordinary local provenance: the model must know
that it is peer-routed content. Context Composer therefore serializes each
admitted routed item as a **user-role** message wrapped in an explicit Chatarium
provenance envelope containing source conversation identity, route identity,
payload identity, delivery event, and context-admission event. The envelope
states that the peer content is not a developer/system instruction.

The exact immutable peer payload remains inside that envelope. Routed content is
never serialized as developer context and never becomes a user-authored
transcript message.

## Single-source-of-truth rule

The live post-commit dispatch path and the desktop's
**Exact next-request context** inspector must use the same Context Composer.

Do not maintain a second hand-built JSON assembly path for previews.

This prevents a UI inspector from claiming one request while transport sends a
different context ordering.

## Current invariants

- conversation isolation is inherited from the active `LocalConversationId`;
- top-level instructions remain separate from message input;
- developer context precedes transcript messages when present;
- durable transcript ordering is preserved;
- exact transcript text is not normalized or rewritten by composition;
- routed inbox delivery alone does not enter context;
- routed context requires the latest durable decision to be `Admit`;
- admitted routed context is user-role, provenance-wrapped peer content, never
  developer/system context;
- the admitted routed-context set is snapshotted when Send is clicked so later
  Admit/Exclude changes cannot mutate a request crossing the local durability
  gate;
- current draft content may appear in preview but cannot enter a live remote
  request until the authored-message durability gate has acknowledged it;
- internal Chatarium bookkeeping provenance does not leak into the remote request;
- routed-peer provenance is the deliberate exception: source/route/payload/
  delivery/admission metadata is serialized explicitly so the model can identify
  that content as routed peer context rather than authored or developer content;
- request preview preserves `store: false` and `stream: true`.

## Source policy and size ledger

The second executable slice makes context inclusion explicit.

The plan records an inventory entry for instructions, developer context, every
durable transcript item, and the current draft preview. Each item carries:

- local source/provenance;
- role when applicable;
- `included`, `omitted · empty`, or `excluded · policy`;
- exact UTF-8 byte count;
- exact Unicode scalar count;
- exact line count.

These are intentionally content-size units, **not model-token estimates**.

Dispatch and preview use separate typed policies. Preview may include the
current draft, while dispatch mechanically excludes `CurrentDraft` even if a
future caller accidentally supplies one before the durability gate.

## Controller orchestration sources

Context Composer now has four explicit orchestration source types in addition
to ordinary transcript and routed peer context:

- bounded controller continuation on the worker side;
- explicitly admitted terminal worker results on the controller side;
- a non-authored controller coordination marker;
- explicitly admitted terminal controller-coordination results.

All four serialize at **user-level trust**, never as developer/system
instructions and never as human-authored transcript messages.

For controller coordination, the worker-result set is frozen by the durable
coordination-start event. Dispatch recomposes from that historical prefix and
appends the coordination marker last. The marker explicitly denies authority to
issue or execute worker controls, mutate lifecycle, or assume continuation
authority.

New coordination turns also declare `suggestion_candidates_v1`. The marker
requests exactly one JSON object with a `summary` string and a
`suggestion_candidates` array. Candidate objects may contain only
`basis_result_route_id` and one typed action name. WorkerId/goal identity is
intentionally omitted from model-supplied schema and re-derived locally from the
frozen route when candidate output is projected.

Coordination output is durable remote-turn evidence plus a separate terminal
coordination result. It is context-excluded by default. A later explicit
coordination-result Admit decision may make that exact terminal result eligible
for ordinary future controller requests; the admitted set is snapshotted at
authored Send.

The special non-authored coordination dispatch path does not consume admitted
prior coordination results. This prevents an implicit coordination recursion
policy from appearing merely because ordinary controller context can use the
result.

## Capability admissions

Context Composer also projects the active Local Inference Contract into typed
request slots for image input, file input, function tools, additional tools,
web search, reasoning, verbosity, and structured output.

A slot is `available`, `unsupported`, or `blocked · contract`.

This is admission information only. Context Composer does not automatically
enable any capability.

The first consumer is [BEHAVIOR_PROFILE.md](BEHAVIOR_PROFILE.md), which uses
the admitted reasoning, verbosity, and web-search slots.

## Explicitly not implemented yet

Context Composer does **not** silently introduce:

- token-budget trimming;
- automatic summarization;
- retrieval;
- automatic shared memory;
- automatic cross-conversation context outside explicit routed delivery +
  admission;
- lifecycle state injection;
- automatic routing;
- master/worker inheritance;
- arbitrary tool selection;
- file/image attachment policy;
- structured-output policy.

Those must become explicit typed inputs to composition rather than hidden
mutations of the transcript.

## Next Context Composer work

Context Composer is now sufficiently explicit for higher-level local policy to
build on it.

Future composer work should be driven by concrete downstream needs, especially:

- lifecycle-provided local context;
- durable memory sources distinct from routed peer messages;
- controller/worker-provided context with explicit provenance;
- attachment sources;
- structured-output requirements;
- real context-budget policy once a trustworthy model-token accounting contract
  exists.

Do not add hidden automatic context mutation merely because a higher layer needs
more information.
