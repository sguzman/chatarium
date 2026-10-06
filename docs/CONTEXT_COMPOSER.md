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
3. the active local conversation's durable user/assistant transcript in durable
   sequence order;
4. the current draft only for the inspector preview, never for live dispatch
   before the normal durable-commit gate completes.

The resulting wire input contains only the Responses role/content shape.
Chatarium-local provenance stays local.

For durable transcript items the plan retains the journal sequence that supplied
the content. It can therefore distinguish:

- conversation developer context;
- durable transcript context;
- current non-durable draft preview.

Those provenance labels are not sent to the model.

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
- current draft content may appear in preview but cannot enter a live remote
  request until the authored-message durability gate has acknowledged it;
- local provenance does not leak into the remote request;
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
- shared memory;
- cross-conversation context;
- lifecycle state;
- routing;
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
- explicit routing/memory sources;
- attachment sources;
- structured-output requirements;
- real context-budget policy once a trustworthy model-token accounting contract
  exists.

Do not add hidden automatic context mutation merely because a higher layer needs
more information.
