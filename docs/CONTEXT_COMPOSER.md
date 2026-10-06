# Context Composer

Status: **ACTIVE**, first executable slice landed 2026-10-06.

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

## Explicitly not implemented yet

This first slice does **not** silently introduce:

- token-budget trimming;
- automatic summarization;
- retrieval;
- shared memory;
- cross-conversation context;
- behavior profiles;
- lifecycle state;
- routing;
- master/worker inheritance;
- tool selection;
- web-search policy;
- file/image attachment policy;
- reasoning/verbosity policy;
- structured-output policy.

Those must become explicit typed inputs to composition rather than hidden
mutations of the transcript.

## Next Context Composer work

The next composer slice should make inclusion policy visible before adding
higher-level behavior:

- explicit source inventory/provenance in the inspector;
- deterministic inclusion/exclusion policy;
- an inspectable context-size/budget ledger without pretending byte/character
  counts are model-token counts;
- typed attachment/tool/control slots admitted only when the active Local
  Inference Contract says the capability is supported;
- tests proving that one local conversation cannot acquire another
  conversation's transcript unless an explicit future routing/memory source is
  added.

Only after that boundary is explicit should Behavior Profile begin to add
higher-level policy.
