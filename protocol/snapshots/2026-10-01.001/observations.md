# Observation 2026-10-01.001

This snapshot is a controlled C02 observation of the read path used when the official ChatGPT client opens one existing conversation.

Flight Recorder v0.7.1 was armed explicitly, one existing conversation was opened from another existing conversation, the target fetch completed, and the recorder was disarmed before export. The selected run contains exactly one observed read request, one captured response, and no read errors or truncation.

## Observed conversation fetch

The selected read used:

- method: `GET`
- path: `/backend-api/conversations/<id>`
- query keys: `include_has_versions`, `num_turns`
- status: `200`
- content type: `application/json`
- body present: yes
- truncated: no
- top-level JSON type: object

The concrete conversation identifier and query values are not committed.

## Causal association

The request was observed while transitioning from one existing conversation to the controlled target conversation. The recorder then observed the target navigation event, followed by the HTTP 200 response. The request, navigation, and response are retained as bounded sequence evidence without concrete conversation identifiers.

## Successful response structure

The response is a top-level JSON object containing conversation metadata, a `messages` array, `current_node`, and `page_info`. The observed test response contained five message records. Message records share a common envelope with `id`, `author`, timestamps, `content`, status/end-turn/weight fields, metadata, recipient, and channel. The content object had two observed structural variants: `content_type` + `parts`, and `content_type` + `content`. The page-info object contained start/end cursors and previous/next-page booleans.

The public fixture preserves this shape with placeholder-only scalar values and deterministic identity placeholders. It does not preserve the controlled conversation title, user text, assistant text, model identifiers, timestamps, URLs, message identifiers, or other private scalar values.

## What this establishes

This snapshot establishes a successful C02 conversation-fetch response schema for protocol revision `2026-10-01.001` and makes that revision the validated baseline for the `ConversationFetch` read flow.

It does **not** establish a stable public API contract, cross-deployment compatibility, complete conversation enumeration, or semantic import rules for message content. Those remain evidence-gated.

C01 remains `NoBaseline`; the successful C02 fetch observation does not promote the conversation-list flow.

## Remaining C02 work

The next P3 step is to define and test semantic parsing/import of the observed conversation envelope without committing private conversation content. Any future parser must remain gated by the named observation revision and must preserve uncertainty when the remote shape changes.
