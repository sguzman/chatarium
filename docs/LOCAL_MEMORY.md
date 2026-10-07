# Local Memory

Status: **ACTIVE**, first explicit durable artifact/admission slice landed 2026-10-07.

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

## Desktop controls

**Context & inference controls -> Local memory** exposes:

- an exact-text memory draft;
- **Record memory**;
- all immutable memory artifacts with source-conversation provenance;
- **Admit memory** for the active destination conversation;
- **Exclude memory** for an admitted artifact.

The UI never auto-records a memory from conversation text.

It never auto-admits newly recorded memory.

## Archive integrity

Both memory audits are authoritative journal projections and participate in
archive integrity checking.

Malformed memory artifacts, duplicate memory identities, context decisions
before artifact creation, scope mismatches, and malformed typed payloads fail
closed.

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

## Next memory boundary: explicit supersession

Immutable artifacts need a correction path.

The next safe memory operation is explicit supersession: one memory artifact may
be durably marked as replaced by a newer artifact while preserving both
historical records.

Supersession must not silently mutate old context decisions. Context projection
must define whether an admitted superseded artifact is excluded, remains visible
with a stale marker, or requires explicit migration to the successor. That
policy must be explicit before automatic retrieval is introduced.
