# Observation 2026-09-30.002

This snapshot is a controlled C02 attempt to observe the read path used when the official chatgpt.com client opens existing conversations.

The operator armed Flight Recorder v0.7.1, navigated among existing conversations without sending or mutating a turn, then disarmed and exported. The selected run contains 11 read requests, 8 captured responses, and one read timeout.

## Observed conversation-fetch requests

Three captured reads used the same structural shape:

- method: GET
- path: /backend-api/conversations/<id>
- query keys: include_has_versions, num_turns

The concrete conversation identifiers are replaced with <id> in the public evidence.

The three conversation-fetch responses were all:

- HTTP status 429
- content type application/json
- body present
- not truncated
- top-level JSON type: object

The private response body was reduced to placeholder structure only. Its concrete error text is not committed.

## Causal association

The selected conversation-fetch reads are not inferred merely from endpoint naming.

- One request/response pair followed navigation from another existing conversation to an existing target conversation.
- A second pair followed navigation to another existing conversation.
- A third pair followed return navigation to the first existing conversation.

The repeated request shape across distinct existing-conversation navigations makes /backend-api/conversations/<id>?include_has_versions&num_turns a directly observed read path for this C02 action.

## What this establishes

This snapshot establishes the request path and query-key shape used by the official client while opening existing conversations during this controlled run.

It also establishes that the observed server behavior for those requests was HTTP 429 rate limiting in this run.

It does not establish the successful conversation-fetch response schema, conversation tree shape, message representation, or any importable conversation semantics.

Therefore:

- LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION remains None.
- conversation-fetch compatibility remains NoBaseline.
- concrete remote conversation parsing/import remains blocked on a successful response observation.

## Background traffic

The run also captured unrelated successful reads from automations, the sidebar, notifications, and user settings. Those are retained as capture context but are not assigned C02 conversation-fetch semantics.

One /backend-api/wham/usage read timed out. It is recorded as a transport error and is not treated as evidence about conversation retrieval.

## Remaining C02 unknown

A successful response from /backend-api/conversations/<id> is still required to establish the response schema. No additional endpoint should be guessed in its place.
