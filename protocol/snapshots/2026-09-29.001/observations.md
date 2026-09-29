# Observation 2026-09-29.001

This snapshot is the first committed empirical protocol observation in Chatarium.

It was derived from a manually exported HAR captured in Microsoft Edge 153 on Linux while creating a new ChatGPT conversation and sending one controlled text turn.

The raw HAR is intentionally **not** committed. Its SHA-256 is recorded in `manifest.json`.

## Experiment classification

This observation is adjacent to canonical `C03-send-text`, but it is **not** a canonical C03 run.

The exact observed user text was:

```text
`respond with exactly CHATARIUM_PROTOCOL_TEST_001`
```

The literal backticks are part of the captured message. Canonical C03 is defined without those backticks.

The final assistant text was:

```text
CHATARIUM_PROTOCOL_TEST_001
```

## High-confidence observations

### Pre-send anti-abuse material

Before the send, the frontend completed one pair of:

- `POST /backend-api/sentinel/chat-requirements/prepare`
- `POST /backend-api/sentinel/chat-requirements/finalize`

The finalize response yielded a chat-requirements token. The proof and turnstile values submitted to finalize were then present by value in the corresponding send request headers.

Values are omitted because they are reusable or security-sensitive.

A second prepare/finalize pair began while the turn was already in progress. This snapshot does **not** establish its purpose.

### Conversation prepare

At `2026-09-29T10:25:33.189Z` the frontend sent:

```text
POST /backend-api/f/conversation/prepare
```

Observed request fields included:

- `client_prepare_state = "sent"`
- `action = "next"`
- `model = "gpt-5-6-thinking"`
- `parent_message_id = "client-created-root"`
- `thinking_effort = "extended"`
- `partial_query` containing the pending user message
- `client_prepare_dispatch = "debounced"`
- `client_prepare_source = "composer_editor_state"`

The response was JSON with:

- `status = "ok"`
- `conduit_token = <redacted>`

The exact conduit token returned here was then supplied as the `x-conduit-token` request header on the send.

### New-conversation initialization

Before the send completed, the client also called:

```text
POST /backend-api/conversation/init
```

with `conversation_id = null` and `requested_default_model = "gpt-5-6-thinking"`.

The response described conversation metadata and limits. This snapshot does not establish whether this request is strictly required for creating a conversation or is frontend initialization/prefetch behavior.

### Send

At `2026-09-29T10:25:34.931Z` the frontend sent:

```text
POST /backend-api/f/conversation
```

The JSON request contained the complete user message plus turn/model/client metadata. The sanitized structural fixture is in:

```text
protocol/fixtures/2026-09-29.001/send-text-request.json
```

Observed security/session-bearing header *names* included:

- `chatgpt-account-id`
- `oai-did`
- `openai-sentinel-chat-requirements-token`
- `openai-sentinel-proof-token`
- `openai-sentinel-turnstile-token`
- `x-conduit-token`
- `x-oai-turn-trace-id`

Their values are intentionally not retained in Git.

The send returned HTTP 200 with:

```text
Content-Type: text/event-stream; charset=utf-8
```

The HAR export did not retain the streamed response body. Therefore this snapshot establishes the streaming response MIME type, but **not** the SSE event schema.

The response also contained a new `x-conduit-token` value distinct from the token used on the request. This snapshot records rotation as an observation but does not infer token semantics.

### Persisted conversation while generation was active

A `POST /backend-api/conversations/batch` during generation returned the new conversation with:

- title `New chat`
- the controlled user message as the current node
- `async_status = 3`

The meaning of numeric `async_status = 3` is unknown from this single capture.

### Completion notifications

The existing `wss://ws.chatgpt.com/.../ws/user/...` connection was subscribed to the `conversations` topic.

Near completion it received, in order:

1. `conversation-created` with the new conversation identifier;
2. `conversation-turn-complete` with the conversation identifier and final current-message identifier.

Identifiers are redacted in committed evidence.

The turn-complete event arrived about 4.7 seconds after the send request began.

### Final persisted conversation

A final `POST /backend-api/conversations/batch` returned the conversation with:

- generated title `Protocol test response`
- final current node pointing at the assistant message
- final assistant text `CHATARIUM_PROTOCOL_TEST_001`
- `async_status` no longer present as the in-progress numeric value

The full batch payload is **not** committed because it also contained hidden/system/context material unrelated to the controlled experiment.

## Important unknowns

This snapshot does not establish:

- the SSE event/frame schema;
- which send fields are mandatory versus optional;
- which request/header fields are stable across frontend revisions;
- whether the numeric `async_status` values have stable semantics;
- the purpose of the second Sentinel prepare/finalize pair;
- whether `conversation/init` is required for mutation;
- whether websocket completion is authoritative, advisory, or merely a frontend notification;
- whether the response `x-conduit-token` is intended for a subsequent turn.

Those remain hypotheses until repeated or more complete evidence exists.
