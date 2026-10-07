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

## Next memory boundary: read-only discovery

The next safe memory feature is deterministic local discovery/search over the
artifact corpus.

Discovery must remain separate from context admission:

- search results create no journal mutation;
- finding a memory does not Admit it;
- superseded artifacts are excluded from the default current-memory view but
  remain inspectable as history;
- no embedding service, model-authored query rewrite, or automatic retrieval is
  implied by the first search slice.

Only after deterministic discovery is visible and inspectable should semantic or
automatic retrieval policy be considered.
