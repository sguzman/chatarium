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
- explicitly admitted tool result/error evidence;
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

## Explicit tool-result evidence source

An adapter-observed terminal tool result/error is **not** automatically included
in a subsequent model request. Its original local conversation may explicitly
Admit or Exclude that outcome after the journal has independently validated
the one-shot approved ToolCall route and terminal observation.

Context Composer exposes a dedicated typed `ToolResult` source at user-level
trust. It wraps the exact observed adapter output with call, route, provider,
source-session, terminal-outcome and context-admission provenance, and identifies
the content as untrusted tool evidence rather than a user-authored or
developer/system instruction.

No context decision changes a tool's execution authority or retroactively
edits the original terminal observation. The current admitted set is frozen at
Send and the Exact next-request inspector uses the same composition policy.
Results over 64 KiB are never truncated into model context.

See [LOCAL_TOOL_INTEGRATION.md](LOCAL_TOOL_INTEGRATION.md).

## Durable outgoing-context evidence manifest

Every **new** Chatarium Responses dispatch path now derives a compact manifest
from the *same typed `ContextPlan`* that supplies its outgoing input. The
manifest is appended inside the existing scoped, durable
`DispatchAttempted` event, **before** the bridge command is issued.
It records request class, owning local conversation, total composed source
counts/bytes, and separate counts for tool results **admitted at the frozen
snapshot** versus actually **included in the composed input**. It retains
bounded call, route, provider, source-session, outcome and admission identity
for the newest 32 entries; if there are more, the full counts remain exact
and the omitted detail rows are explicitly labeled as truncated. The raw
tool output and request body are not copied into this metadata.

The three dispatch kinds are ordinary authored Send, non-authored controller
coordination, and bounded worker continuation. Authored Send uses the
admission set frozen when Send was clicked, not the live selection after
the journal acknowledges the user message. Specialized dispatches use
their **historical start-event prefix** to report eligible results. They do
not silently import those results merely because they were admitted for
ordinary conversation turns. Capturing the manifest validates that every
included tool result belongs to the eligible set; inconsistency blocks
dispatch rather than silently reporting a false record.

Under **Context & inference controls → Recent outgoing context snapshots**,
Chatarium displays the latest manifested attempts for the selected local
conversation, the included/eligible distinction, bounded evidence identities,
and whether a later remote-acceptance observation exists. Legacy attempts
without a manifest are not retrospectively invented.

### Reverse MCP result provenance

Under **Context & inference controls → Recent outgoing context snapshots →
Reverse MCP provenance**, enter an immutable numeric tool-call ID or choose
one of the recent IDs found in manifested dispatches. Chatarium projects the
recorded requests in the *selected local conversation* and distinguishes
three materially different cases: **INCLUDED** in the composed input,
**admitted but OMITTED**, and **UNKNOWN because the 32-result manifest detail
list was truncated**. Complete manifests that do not name the call are
counted separately as **not eligible at that request snapshot**; the viewer
does not flood the list with ordinary negative matches.

The **Admitted MCP evidence** inventory and the per-provider **Recent MCP
results** review rows also offer **Where used? / Find historical use**
shortcuts. These select the verified call ID for the reverse inspector
without admitting a result, changing permissions or rerunning the tool.
Open the **Recent outgoing context snapshots** panel to see the selected
call's history; manual numeric lookup remains available for older calls.

The lookup validates that a call retains its exact route, provider, source
session, terminal outcome sequence and outcome kind across requests. A
later explicit readmission may legitimately change the admission decision
sequence without changing that immutable terminal result. Contradictory
identity or mixed-conversation input fails closed.

The result view is paginated, newest-first, and every positive or
truncated-unknown request links its dispatch sequence and transport
observation status. **Copy this lookup as JSON** exports a versioned,
content-free report with counts, exact matches and unresolved truncated
coverage. Only manifested requests are covered: requests predating the
manifest feature remain unknown rather than inferred. An INCLUDED
disposition is evidence about the locally composed request, not proof of
delivery, model attention or a successful remote answer.

### Correlated transport history and navigation

The manifest history inspector replays journal events **once in chronological
order** and joins later transport observations by the exact local-turn scope,
request identity, and typed Responses observation envelope. It never assumes
that an event elsewhere in the journal belongs to the selected request.
Before displaying the records, it independently checks manifested conversation
ownership against the typed authored-message commit or the durable
controller-coordination/worker-continuation start.

The inspector distinguishes **no observed outcome**, **remote acceptance**,
**completion**, **observed remote failure**, **transport interruption**, and
**conflicting terminal observations**. A late interruption never erases
previously observed acceptance; a completion after an interruption is labeled
as such. These are journaled observation types, **not** claims about model
attention, server persistence, or guaranteed delivery. Unexpectedly
mis-correlated typed transport observations fail the history projection
closed instead of silently fabricating an outcome.

All manifested attempts remain navigable oldest-to-newest through paginated
history (12 visible per page, newest page first), even when a conversation
has many previous requests. Pagination limits rendering, **not audit replay**
or historical retention. Pre-feature dispatches without manifests remain
unmanifested.

Individual recorded attempts expose an ordered, event-numbered transport
timeline and an explicitly requested **Copy this audit as JSON** action.
The expanded history inspector holds a transient, per-conversation
in-memory projection cache keyed by the append-only journal's observed
sequence high-water. It replays when a new durable event arrives or the
selected local conversation changes, but does not rebuild an unchanged
history on every egui frame. Cache entries never persist across application
restarts, and failed-closed projections are not silently replaced by a
partially successful list.

The export is a portable, versioned, content-free evidence report containing
the owning conversation/turn, transport observation sequences, composed
source counts and bytes, and the listed per-call provenance (including
provider, route, source session, terminal observation and admission event).
It never duplicates user text, model prompts or adapter-result bodies.
If more than 32 tool identities were eligible, the export marks its
per-call detail as incomplete while preserving exact aggregate counts.

**Precision:** `DispatchAttempted` means Chatarium durably prepared an
outgoing request, not that the service received or used it. Even observed
remote acceptance is distinct from model attention. The manifest describes
Chatarium's composed input before any bridge/provider transformations; it
is **not** a cryptographic digest of the full wire request. UTF-8 byte
counts are not token estimates, and no admission or execution permissions
are changed by the audit.

## Explicit local memory source

Context Composer now has a first-class `LocalMemory` source distinct from
transcript, routed peer messages, and orchestration context.

A memory artifact enters a conversation request only when that destination
conversation currently has a durable Admit decision for the exact
`LocalMemoryId`. The memory source serializes at **user-level trust** inside an
explicit envelope containing memory identity, source conversation identity,
artifact sequence, admission sequence, and exact immutable memory text.

Memory is never serialized as developer/system instruction content and never
becomes human-authored transcript history.

The admitted memory set is snapshotted when Send is clicked, so later
Admit/Exclude changes cannot mutate an already-committed request.

Supersession is also part of effective admission. A raw historical Admit for a
superseded predecessor remains auditable, but the effective admitted-memory
projection excludes it. Successor artifacts receive no inherited context
authority and enter Context Composer only after their own explicit Admit
decision.

See [LOCAL_MEMORY.md](LOCAL_MEMORY.md) for the durable artifact/admission
ontology.

### Next-request-only local memory

Context Composer also has a distinct `LocalMemoryOneShot` source for manual
request-scoped memory use.

The pre-send selection is frozen against a journal high-water and validated
before the authored message commit. After commit, the exact selection is
durably correlated to that LocalTurnId. Dispatch reconstructs one-shot memory
from the journal instead of trusting mutable UI selection state.

One-shot memory serializes at user-level trust and carries separate provenance:
memory identity, source conversation, artifact event, frozen snapshot
high-water, and durable turn-selection event. Its envelope explicitly marks the
source **NEXT REQUEST ONLY**.

It does not change persistent memory admission, and later turns do not inherit
the selection automatically.

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
- automatic memory extraction or semantic retrieval beyond explicitly recorded
  artifacts and explicit persistent/one-shot selection;
- automatic cross-conversation context outside explicit routed delivery,
  admitted local memory, and their separate admission boundaries;
- lifecycle state injection;
- automatic routing;
- master/worker inheritance;
- arbitrary or automatic tool selection/execution (explicit tool-result
  evidence admission does not perform any tool action);
- file/image attachment policy;
- structured-output policy.

Those must become explicit typed inputs to composition rather than hidden
mutations of the transcript.

## Next Context Composer work

Context Composer is now sufficiently explicit for higher-level local policy to
build on it.

Future composer work should be driven by concrete downstream needs, especially:

- lifecycle-provided local context;
- optional explicit memory organization metadata and later semantic retrieval
  policy that remain distinct from context admission;
- controller/worker-provided context with explicit provenance;
- attachment sources;
- structured-output requirements;
- real context-budget policy once a trustworthy model-token accounting contract
  exists.

Do not add hidden automatic context mutation merely because a higher layer needs
more information.
