# Observation 2026-09-30.001

This snapshot is Chatarium's first controlled C01 read-side observation from the official `chatgpt.com` client.

The operator armed Flight Recorder v0.7.0, exposed and scrolled the sidebar far enough to trigger older-entry loading, waited briefly for traffic to settle, then disarmed and exported. The selected run is bounded by the durable Arm event at sequence 67 and Disarm event at sequence 80.

## Selected read traffic

The selected run contains 12 successful `GET` responses with JSON-family content, totaling 460,268 captured bytes. No selected response was truncated and no selected read error was recorded.

Observed selected endpoint counts:

- `/backend-api/gizmos/snorlax/sidebar` — 4
- `/backend-api/pins` — 4
- `/backend-api/accounts/check/v4-2023-04-27` — 2
- `/backend-api/settings/user` — 2

The C01 semantic evidence in this snapshot comes from the sidebar surface. The other observed reads are retained as capture context only; this snapshot does not assign them conversation-list semantics.

## Sidebar pagination

The first observed sidebar request used query parameter names:

- `conversations_per_gizmo`
- `limit`
- `owned_only`

Later sidebar requests additionally used:

- `cursor`

Only parameter names are preserved. Query values were never captured by the protocol-read recorder.

The observed sidebar response is a top-level JSON object with `items` and `cursor`. Each sampled top-level item contains a `gizmo` object and a nested `conversations` object. The nested `conversations` object contains its own `items` and `cursor`.

The initial observed page contained nested conversation summary records. Representative summary structure included conversation identity, title, creation/update timestamps, current-node/mapping fields, archive/star/temporary flags, ownership/workspace fields, memory/context fields, and related metadata. Public fixtures retain only a minimal placeholder-only subset needed to demonstrate the relationship.

Subsequent top-level cursor pages were also observed. Those pages can contain top-level gizmo entries whose nested `conversations.items` arrays are empty. The final observed top-level page had a null top-level cursor.

## What this establishes

This capture establishes that, during the controlled C01 action, the official web client used a paginated `/backend-api/gizmos/snorlax/sidebar` JSON read surface and that this surface can contain nested conversation summaries.

It does **not** establish that this endpoint is Chatarium's complete `ConversationList` primitive. In particular, one capture does not prove that the surface enumerates every remote conversation, covers ordinary non-gizmo conversations uniformly, or remains stable across accounts and deployments.

For that reason, `LATEST_VALIDATED_CONVERSATION_LIST_OBSERVATION` remains `None`, and the production conversation-list compatibility result remains `NoBaseline`.

## Recorder boundary artifact

A sidebar response that began before Disarm completed afterward as sequence 81. Flight Recorder v0.7.0 also allowed that late completion to mutate its top-level in-memory response/byte counters.

The corrected legacy ingestion rule selects only protocol-read events from Arm through the first Disarm, inclusive. Sequence 81 and those post-Disarm counter mutations are therefore excluded from this observation.

## Remaining unknowns

- Which read surface, or composition of surfaces, yields the complete remote conversation set required by Chatarium.
- Whether this sidebar shape is stable across deployments and account configurations.
- How ordinary conversations outside the observed gizmo/project nesting are enumerated.
- C02 retrieval semantics for opening one existing conversation.
