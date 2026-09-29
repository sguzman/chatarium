# Text-turn endpoint observations

This document accumulates endpoint observations for the basic text-send path.

Current evidence: `2026-09-29.001` only.

Nothing here is a supported-API claim.

## POST /backend-api/f/conversation/prepare

Observed immediately before a new text send.

Observed request fields in `2026-09-29.001`:

- `client_prepare_state`
- `action`
- `is_do_not_remember`
- `model`
- `parent_message_id`
- `thinking_effort`
- `timezone`
- `timezone_offset_min`
- `local_function_names`
- `partial_query`
- `client_prepare_dispatch`
- `client_prepare_source`

Observed response fields:

- `status`
- `conduit_token`

In this snapshot, the returned conduit token exactly matched the later `x-conduit-token` request header used by `/backend-api/f/conversation`.

## POST /backend-api/f/conversation

Observed mutation endpoint for the controlled text turn.

The request directly contained the user message in a `messages` array plus model/turn/client metadata.

Observed response:

- status: 200
- MIME: `text/event-stream; charset=utf-8`
- response header names included `x-conduit-token`, `x-oai-is-update`, `x-oai-request-id`, and `x-build`

The exported HAR did not contain the SSE body, so event names and chunk schema remain unknown.

The response `x-conduit-token` value differed from the token used on the request. No semantic interpretation is assigned yet.

## POST /backend-api/conversation/init

Observed around conversation creation and again while the turn was in flight.

For the initial new-conversation call, `conversation_id` was null and `requested_default_model` was the selected model.

For later calls, `conversation_id` contained the new conversation identifier.

This single snapshot does not establish whether this endpoint is required for sending, metadata hydration, or frontend prefetch.

## POST /backend-api/conversations/batch

Observed both during generation and after completion.

During generation, the returned conversation still had the user message as current node and included numeric `async_status = 3`.

After completion, the returned conversation had the assistant final message as current node and contained the deterministic marker text.

The full response is intentionally not a public fixture because it also contained hidden/private context material.

## POST /backend-api/sentinel/chat-requirements/prepare

Observed before the send as the first half of an anti-abuse/security preparation flow.

Response bodies contain security-sensitive challenge material and are not fixtures.

## POST /backend-api/sentinel/chat-requirements/finalize

Observed after Sentinel prepare.

The finalize response yielded a chat-requirements token later sent as a header on the mutation request. Proof and turnstile values supplied to finalize were also later supplied on the send.

Only field/value linkage is documented; reusable values are not retained.

## wss://ws.chatgpt.com/.../ws/user/...

An already-open websocket subscribed to the `conversations` topic emitted:

- `conversation-created`
- `conversation-turn-complete`

The completion event carried a conversation identifier and final current-message identifier.

The websocket also carried presence/subscription traffic unrelated to the mutation itself.

## Stability status

All observations above are single-snapshot facts. Required/optional status, long-term stability, and authority ordering remain unknown.
