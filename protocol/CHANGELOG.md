# Protocol observation changelog

This file records changes in **our observations** of the ChatGPT web client. It is not a changelog published by OpenAI.

## Unreleased

- Established the protocol corpus structure and capture discipline.
- Added first empirical snapshot `2026-09-29.001` from a manual Edge/Linux HAR of a completed new-conversation text turn.
- Observed `/backend-api/f/conversation/prepare` -> `/backend-api/f/conversation` conduit-token linkage, Sentinel material linkage, `text/event-stream` mutation response transport, conversation-topic websocket completion notifications, and persisted reconciliation through `/backend-api/conversations/batch`.
- Recorded that exported HAR did not preserve the SSE body and that DevTools "sanitized" output still contained secret-bearing and private material; raw HAR remains outside Git.
- Added a sanitized structural send-request fixture and cross-snapshot send-text flow document.
