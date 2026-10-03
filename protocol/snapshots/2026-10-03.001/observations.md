# Observation 2026-10-03.001

This snapshot records a manually exported Microsoft Edge 153 HAR from the user's own authenticated ChatGPT web session on Linux. The raw HAR remains private and outside Git because it contains account, conversation, and ephemeral security material.

The capture was broader than a canonical one-action C02 run. Only requests whose user-visible action and response could be correlated unambiguously are promoted here.

## Existing-conversation read

A successful existing-conversation load used:

- method: `GET`
- path: `/backend-api/conversations/<id>`
- exact observed query occurrence order: `num_turns=10&include_has_versions=true`
- status: `200`
- content type: `application/json`

This closes the previously unknown C02 query-literal gap without backfilling the older 2026-10-01.001 observation.

The returned body retained the message-array envelope, `current_node`, and `page_info` established on 2026-10-01.001. This response contained 22 messages. It additionally contained two messages whose content object used `content_type`, `thoughts`, and `source_analysis_msg_id`; that content shape was not present in the October 1 fixture.

## New-conversation write survey

The same HAR also contains a successful first text turn of a newly created conversation:

- `POST /backend-api/f/conversation/prepare`
- `POST /backend-api/f/conversation`
- HTTP 200 `text/event-stream` from the mutation
- the mutation body carried one user message and used the client root parent
- a later prepare call carried the newly created remote conversation identity and current parent message identity
- `POST /backend-api/conversation/id/<id>/rename` successfully renamed that remote conversation

Ephemeral conduit/Sentinel/challenge values are intentionally excluded.

Crucially, this HAR does **not** contain a second message sent into that already-existing test conversation. Therefore this observation does not establish the exact same-thread write mutation for an existing remote conversation.

## Rate limiting

One earlier existing-conversation fetch returned HTTP 429. Two account conversation-list GETs also returned HTTP 429. These are recorded as failure observations only; they do not establish a C01 list baseline and must not be treated as permission to bypass or retry around service protections.

## Implementation consequence

The protocol layer may now freeze the C02 request resource with `num_turns=10` followed by `include_has_versions=true`, and the 2026-10-03.001 response parser may accept the newly observed thoughts/source-analysis content shape. The older 2026-10-01.001 revision remains valid for its narrower observed body shape.
