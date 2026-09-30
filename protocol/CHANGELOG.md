# Protocol observation changelog

This file records changes in **our observations** of the ChatGPT web client. It is not a changelog published by OpenAI.

## Unreleased

- Established the protocol corpus structure and capture discipline.
- Added first empirical snapshot `2026-09-29.001` from a manual Edge/Linux HAR of a completed new-conversation text turn.
- Observed `/backend-api/f/conversation/prepare` -> `/backend-api/f/conversation` conduit-token linkage, Sentinel material linkage, `text/event-stream` mutation response transport, conversation-topic websocket completion notifications, and persisted reconciliation through `/backend-api/conversations/batch`.
- Recorded that exported HAR did not preserve the SSE body and that DevTools "sanitized" output still contained secret-bearing and private material; raw HAR remains outside Git.
- Added a sanitized structural send-request fixture and cross-snapshot send-text flow document.
- Added canonical C03 snapshot `2026-09-29.002` from Flight Recorder v0.5.0 with the full `/backend-api/f/conversation` response stream.
- Observed SSE `delta_encoding = "v1"`, incremental delta operations, final-message completion patching, `message_stream_complete`, and terminal `[DONE]`.
- Flight Recorder v0.5.1 now treats `[DONE]` as a clean local terminal condition instead of reading once more and reporting Chromium's post-terminal clone abort.
- Added partial controlled C01 snapshot `2026-09-30.001` from Flight Recorder v0.7.0: observed a paginated `/backend-api/gizmos/snorlax/sidebar` JSON surface with nested conversation summaries during sidebar scrolling. The evidence is committed with placeholder-only structure, while `ConversationList` remains `NoBaseline` because the capture does not prove complete account-wide enumeration.
- Added controlled C02 snapshot `2026-09-30.002` from Flight Recorder v0.7.1: three existing-conversation navigations produced the same `GET /backend-api/conversations/<id>` request shape with `include_has_versions` and `num_turns`, but every captured conversation-fetch response was HTTP 429. The request path is now evidence-backed; the successful response schema and `ConversationFetch` baseline remain `NoBaseline`.
