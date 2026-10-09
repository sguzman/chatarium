# Local Memory

Status: **ACTIVE**, explicit durable artifact/admission/supersession slice landed 2026-10-07.

Chatarium local memory is a distinct local data domain. It is not transcript
history, routed peer content, worker lifecycle state, controller coordination
output, or hidden prompt text.

## Identity and artifact model

`LocalMemoryId` identifies one immutable local memory artifact.

A `LocalMemoryArtifactRecorded` event stores:

- the memory identity;
- the source `LocalConversationId` from which the user recorded it;
- the exact memory text;
- the durable journal sequence.

Artifact text is immutable. Recording an artifact does not admit it to any
conversation's inference context.

The source conversation is provenance only. It does not automatically own
exclusive use of the artifact.

## Context admission

Memory existence and memory use are separate durable facts.

`LocalMemoryContextDecisionRecorded` records a reversible `Admit` or
`Exclude` decision for one:

`LocalMemoryId -> destination LocalConversationId`

pair.

The default is excluded.

This means an artifact may be explicitly reused across local conversations
without sharing either conversation's transcript. Cross-conversation use occurs
only because the user admits that exact memory artifact into that exact
destination conversation.

Replay validates that every context decision references an already-recorded
artifact. Decisions do not duplicate or rewrite memory text.

## Context Composer semantics

Currently admitted local memories become typed `LocalMemory` sources in
Context Composer.

They serialize at **user-level trust** inside an explicit Chatarium envelope
containing:

- memory identity;
- source conversation identity;
- artifact event sequence;
- context-admission event sequence;
- exact memory text.

The envelope explicitly states that memory is not human-authored transcript
content and not a developer/system instruction.

Admitted memory is merged chronologically by its latest admission sequence.

The admitted set is snapshotted when the user clicks Send. A later Admit or
Exclude action cannot mutate a request already crossing the local
authored-message durability gate.

## Staging a retrieved native message

Native conversation search now provides **Stage as memory** for a matched
user/assistant message. With the native search field focused, **Alt+M**
stages the highlighted matching message (or the first result when none is
highlighted); a title-only match cannot stage a message. Staging requires
an empty memory-editor draft and loads the exact projected visible message
text into that editor. Nothing
is durably recorded or admitted by staging alone. The UI displays its native
source conversation identity and message event sequence for review.

Only a separate **Record memory** action creates an immutable artifact. For
an unedited staged message, it records the *original* source conversation,
not whichever chat happens to be active. If the user edits the draft, the
staged exact-source binding is cleared and a manually authored artifact
uses the current conversation as its source. The origin event sequence is
shown for review but is not a new durable field in the memory artifact schema.

Later **Admit memory** or **Use once next request** remains an independent
per-destination user choice, with the existing durable gates. Staging and
recording never implicitly grant inference-context authority. Neither
a retrieved assistant statement nor a retrieved user statement becomes
verified truth merely by being staged.

## Desktop controls

**Context & inference controls -> Local memory** exposes:

- an exact-text memory draft;
- **Record memory**;
- all immutable memory artifacts with source-conversation provenance;
- **Admit memory** for the active destination conversation;
- **Exclude memory** for an admitted artifact;
- visible supersession state for stale predecessors;
- **Supersede with…** choices limited to compatible newer same-source artifacts.

The UI never auto-records a memory from conversation text.

It never auto-admits newly recorded memory or a supersession successor.

## Archive integrity

The artifact, context-decision, and supersession audits are authoritative
journal projections and participate in archive integrity checking. Archive
validation also exercises the effective admitted-memory projection.

Malformed memory artifacts, duplicate memory identities, context decisions
before artifact creation, invalid supersession lineages, scope mismatches, and
malformed typed payloads fail closed.

## Deliberate non-features

The first memory slice does **not** implement:

- automatic memory extraction from user or assistant text;
- model-authored memory writes;
- automatic cross-conversation sharing;
- semantic retrieval or embedding search;
- token-budget-driven memory selection;
- confidence scores;
- implicit memory promotion from routed messages or coordination results;
- in-place editing of immutable artifacts;
- deletion that rewrites journal history.

## Landed supersession policy

Immutable memory artifacts now have an explicit correction path through
`LocalMemoryArtifactSuperseded`.

Supersession is a forward-only lineage edge between two already-recorded
artifacts. Replay requires:

- predecessor and successor are distinct;
- both artifacts already exist;
- both share the same source-conversation provenance;
- the successor artifact was recorded later;
- one predecessor has at most one successor;
- one successor has at most one predecessor.

Chains are allowed. Branching, merging, backward replacement, cross-source
replacement, and self-supersession fail closed.

Supersession does **not** rewrite old context decisions. A predecessor may still
have a historical Admit decision in the raw audit, but the effective admitted
projection mechanically excludes every superseded artifact. A new Admit attempt
for a superseded predecessor is rejected.

The successor starts with no inherited context authority. It must be explicitly
Admitted to each destination conversation where the user wants it used.

The desktop therefore keeps stale predecessors visible, marks them
`CONTEXT: EXCLUDED · SUPERSEDED`, shows the successor identity, and offers
explicit compatible **Supersede with…** choices.

## Landed read-only discovery

Deterministic local memory discovery is now implemented as a pure projection.

The first search slice:

- uses local case-insensitive literal substring matching over exact artifact
  text;
- treats an empty query as a browse operation;
- returns newest artifacts first;
- excludes superseded predecessors from the default current-memory view;
- can explicitly include superseded history;
- exposes immediate successor identity for stale results;
- performs no journal write and changes no context decision.

The desktop exposes a search field and **Show superseded history** toggle inside
the Local memory panel. Search results continue to use the same explicit
Admit/Exclude and Supersede controls. Finding a memory never admits it.

No embedding service, model-authored query rewrite, semantic ranking, or
automatic retrieval is implied by this search projection.

## Landed organization metadata and faceted discovery

Optional user-authored labels are now a separate durable metadata layer through
`LocalMemoryLabelAdded` and `LocalMemoryLabelRemoved`.

`LocalMemoryLabel` preserves exact user text while rejecting empty,
surrounding-whitespace, control-character, and overlong values. Label replay
requires the artifact to exist first and fails closed on duplicate Add or
removing an inactive label.

Labels do not change artifact text, supersession, or context admission. They may
be added, removed, and re-added without rewriting history.

The desktop now exposes the complete label workflow:

- active labels are visible on each memory;
- **Add label** validates and durably records exact user-authored metadata;
- each active label can be explicitly removed;
- free-text discovery searches both artifact text and active label text;
- an exact label facet narrows results without changing context state;
- an exact source-conversation facet narrows by durable artifact provenance;
- facet counts respect the **Show superseded history** boundary.

The search/facet joins also participate in archive-integrity validation.

No label, query, or facet action can Admit, Exclude, supersede, or otherwise
mutate memory authority. Model-generated labels remain non-authoritative unless a
future explicit acceptance layer is designed.

## Landed request-scoped manual use

Persistent Admit remains appropriate for memory that should participate in every
future request for a destination conversation. Next-request-only selection now
covers the weaker one-shot case without changing persistent context authority.

The desktop exposes **Use once next request** / **Remove one-shot** on eligible
memory artifacts. Selection is local UI state until Send.

At Send, Chatarium freezes:

- the exact sorted unique LocalMemoryId set;
- the current journal high-water sequence;
- the destination LocalConversationId.

The persistence worker validates that frozen snapshot **before** committing the
authored user message. A one-shot selection fails closed if it is empty but
carries a snapshot marker, missing its snapshot marker, oversized, unsorted,
duplicated, absent from the frozen journal prefix, superseded at that prefix, or
already persistently admitted for the destination conversation.

After the authored message is durably committed, Chatarium appends one
LocalMemoryTurnSelectionRecorded fact scoped to that exact LocalTurnId. Replay
requires the selection snapshot to predate the authored commit and revalidates
the exact memory set against the frozen historical prefix.

Context Composer then reconstructs those memories as explicit
LocalMemoryOneShot user-level sources. Their envelope identifies the memory,
source conversation, artifact event, frozen snapshot high-water, and durable
turn-selection event. The envelope marks the memory **NEXT REQUEST ONLY** and
does not claim user authorship or developer/system authority.

The pre-send selection is cleared after the durable commit/selection
acknowledgement. A later authored turn receives no one-shot memory unless the
user explicitly selects it again.

One-shot use never changes persistent Admit/Exclude state. Later supersession
also does not rewrite the historical fact that an earlier turn used the memory
when it was valid at its frozen snapshot.

## Memory frontier

The explicit manual memory stack is now vertically complete enough for
dogfooding:

artifact recording -> correction/supersession -> labels/facets -> read-only
discovery -> persistent per-conversation admission -> next-request-only manual
selection -> typed Context Composer provenance.

Automatic semantic retrieval, embeddings, model-authored extraction, confidence
scoring, and token-budget-driven selection remain deliberately deferred. None
should be introduced as a hidden extension of the manual memory authority model.

The active local-first product frontier now moves to the first MCP/tool
integration substrate. Memory should evolve further only when a concrete
dogfooding need justifies another explicit boundary.
