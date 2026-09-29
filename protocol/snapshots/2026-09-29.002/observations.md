# Observation 2026-09-29.002

This snapshot is Chatarium's first canonical C03 observation with the actual text-turn response stream preserved.

Exact user text:

```text
respond with exactly CHATARIUM_PROTOCOL_TEST_001
```

Observed final assistant text:

```text
CHATARIUM_PROTOCOL_TEST_001
```

The source was Chatarium Flight Recorder v0.5.0 on the official `chatgpt.com` page.

## Transport

The captured mutation response was HTTP 200 with:

```text
Content-Type: text/event-stream; charset=utf-8
```

The first SSE frame declared:

```text
event: delta_encoding
data: "v1"
```

The stream contained 29 SSE frames reconstructed from 17 browser response chunks totaling 17,054 bytes. Browser chunk boundaries were not SSE frame boundaries.

## Frame families

Named SSE frames used `event: delta` with JSON payloads. Observed delta shapes included full message objects and path operations using `p`, `o`, and `v`.

Observed operation names included:

- `add`
- `append`
- `patch`
- `replace`

Observed paths included:

- `/message/content/parts/0`
- `/message/status`
- `/message/end_turn`
- `/message/metadata`
- `/message/metadata/conversation_followup_suggestions_eligible`

Data-only frames omitted the SSE `event` line and encoded a JSON object directly in `data:`.

Observed data-only types included:

- `resume_conversation_token`
- `input_message`
- `title_generation`
- `message_marker`
- `server_ste_metadata`
- `message_stream_complete`
- `conversation_detail_metadata`

The terminal frame was:

```text
data: [DONE]
```

## Assistant lifecycle

The final assistant message first appeared as a full message object with:

- role `assistant`
- content type `text`
- empty first text part
- status `in_progress`
- channel `final`

The visible answer then arrived as:

```json
{
  "p": "/message/content/parts/0",
  "o": "append",
  "v": "CHATARIUM_PROTOCOL_TEST_001"
}
```

The next delta patched the message to:

- `status = "finished_successfully"`
- `end_turn = true`
- metadata containing `finish_details`
- `can_save = true`
- `is_complete = true`

Observed message markers around completion included first-token markers for reasoning/user-visible/final-channel activity and a final `last_token` marker.

Near the end of the stream the recorder observed, in order:

1. server metadata
2. `message_stream_complete`
3. conversation detail metadata
4. `[DONE]`

## Identity transition

The browser route changed from a temporary `local-chatgpt:` conversation identity to the canonical conversation identity while the stream was active.

This supports Chatarium's existing rule that temporary and canonical identities are separate observations whose relationship must be preserved rather than inferred from syntax alone.

## Recorder v0.5.0 terminal artifact

After durably recording `[DONE]`, recorder v0.5.0 performed one additional read on the cloned body and Chromium reported `BodyStreamBuffer was aborted`.

This is classified as a recorder artifact rather than a remote turn failure because the final text, successful message patch, `message_stream_complete`, and `[DONE]` had all already arrived.

Flight Recorder v0.5.1 treats `[DONE]` as a clean local terminal condition.

## Reliability implication

A direct Chatarium parser can reconstruct the final assistant message incrementally from the stream.

Completion has several separately observed signals:

- message status becomes `finished_successfully`;
- `end_turn = true`;
- metadata `is_complete = true`;
- final-token marker;
- `message_stream_complete`;
- `[DONE]`;
- separately, snapshot `2026-09-29.001` observed websocket and persisted-conversation completion.

Chatarium should preserve these as distinct evidence until repeated captures establish their relative authority and failure behavior.

## Remaining unknowns

This snapshot does not establish which frames are mandatory, whether all models use the same encoding, cancellation/error grammar, or resume behavior.
