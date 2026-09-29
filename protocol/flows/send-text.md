# Send text

This flow describes the official ChatGPT web client's observed behavior for a simple completed text turn.

Current evidence:

- `2026-09-29.001` — Microsoft Edge 153 on Linux, manual HAR, new conversation.

One observation is not a stability claim.

## Observed sequence in 2026-09-29.001

```text
Sentinel prepare
    ↓
Sentinel finalize
    ↓
POST /backend-api/f/conversation/prepare
    ↓  returns conduit token
POST /backend-api/conversation/init
    ↓
POST /backend-api/f/conversation
    ↓  HTTP 200 text/event-stream
    ├─ in-progress POST /backend-api/conversations/batch
    ├─ websocket: conversation-created
    ├─ websocket: conversation-turn-complete
    └─ final POST /backend-api/conversations/batch
```

### Token linkage observed

The conduit token returned by `/backend-api/f/conversation/prepare` exactly matched the value sent in the later `x-conduit-token` header of `/backend-api/f/conversation`.

The chat-requirements token returned by the preceding Sentinel finalize response exactly matched the later `openai-sentinel-chat-requirements-token` send header. The finalized proof and turnstile values also matched their corresponding send headers.

Only equality/linkage is recorded. Token values are secret-bearing evidence and are not fixtures.

### Mutation request

The mutation endpoint observed was:

```text
POST /backend-api/f/conversation
```

The request carries the user message directly in a `messages` array rather than referring only to the earlier partial query.

For a brand-new conversation, the observed request had:

```text
parent_message_id = "client-created-root"
action = "next"
client_prepare_state = "success"
supported_encodings = ["v1"]
```

The observed model was `gpt-5-6-thinking` with `thinking_effort = "extended"`.

Do not interpret these as universally required fields from one capture.

### Streaming

The mutation response used `text/event-stream`.

The manual HAR did not retain the stream body, so the transport is observed but its event grammar remains unknown.

This is a concrete reason the later automated recorder should preserve streaming bytes/events directly rather than relying exclusively on HAR export.

### Independent completion evidence

Even without the SSE body, completion was positively evidenced by two later surfaces:

- the `conversations` websocket topic emitted `conversation-turn-complete`;
- a subsequent `/backend-api/conversations/batch` response contained the final assistant message and made it the current node.

Chatarium should preserve those as separate observations rather than prematurely declaring one to be the sole authority.

## Failure/uncertainty implication

A future direct client must not blindly resend merely because the streaming connection is lost.

This snapshot demonstrates that conversation creation and turn completion can be visible through other channels after the mutation request. Reconciliation should therefore consult durable remote state before deciding that a send failed.

## Evidence caveat

The controlled input in snapshot `2026-09-29.001` contained literal Markdown backticks around the canonical marker prompt. Therefore it is not itself a canonical C03 fixture even though it exercises the same basic send path.
