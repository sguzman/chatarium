# Capture playbook

This playbook creates protocol evidence that can later be compared across ChatGPT frontend revisions.

## Preparation

Use a controlled, non-sensitive test conversation. Open browser developer tools before the experiment and enable network log preservation. Capture one behavior at a time where practical.

Do not intentionally expose reusable credentials for the sake of documentation. Raw captures may contain them incidentally; keep raw evidence outside Git and sanitize before committing.

## Baseline captures

### C00 — idle page load

Action: load `https://chatgpt.com/` and do nothing until network activity settles.

Purpose: distinguish boot/session/configuration traffic from conversation actions.

### C01 — conversation list

Action: expose/load the conversation list without opening a new thread if possible.

Purpose: identify list/pagination traffic.

### C02 — open existing conversation

Action: open a controlled existing thread.

Purpose: identify conversation retrieval and any lazy secondary requests.

### C03 — new text turn, complete

Use a deterministic harmless prompt such as:

```text
respond with exactly TEST123
```

Purpose: identify create/send/stream/completion behavior with minimal semantic noise.

### C04 — stop generation

Send a prompt that produces enough output to permit pressing Stop, then stop it once.

Purpose: identify cancellation semantics and final remote state.

### C05 — retry/regenerate

Perform exactly one retry/regenerate action after a completed controlled turn.

Purpose: identify branching/replacement semantics.

### C06 — edit/branch previous user message

Where supported, edit a controlled previous message and submit the edit.

Purpose: establish parent/branch identity behavior.

### C07 — tiny attachment

Upload a tiny text file containing only a known marker such as `CHATARIUM_ATTACHMENT_TEST_001`, then ask the model to repeat it.

Purpose: isolate upload metadata, transfer, attachment association, and message references.

### C08 — intentional transport interruption

Under a controlled setup, interrupt connectivity after a send begins and record both browser-visible behavior and network evidence. Restore connectivity without manually resending unless the experiment specifically calls for it.

Purpose: understand what can and cannot be reconciled after ambiguous disconnects.

## For every capture, record

- capture ID;
- protocol snapshot revision;
- local start/end time and timezone;
- browser/version if readily available;
- exact human actions;
- exact controlled prompt/file body where safe;
- conversation URL/identifier after replacing private identifiers if necessary;
- relevant frontend asset hashes if collected;
- whether the result completed, failed, or became ambiguous;
- anything clicked after a failure.

## Export strategy

HAR with response content is useful for initial manual work. It may not preserve every streaming detail perfectly, so later recorder work should use Chromium DevTools Protocol instrumentation to record request, response, SSE/fetch streaming, WebSocket activity where applicable, and loaded asset identities directly.

## Sanitization gate

Before evidence enters Git:

1. scan headers and bodies for credentials;
2. remove/rewrite session-bearing values;
3. remove unrelated private conversations/account data;
4. retain deterministic test payloads where safe;
5. document every sanitization class;
6. inspect the final diff manually.

A capture that cannot be safely sanitized should remain referenced by hash/metadata only rather than committed.

## Comparison discipline

When a later deployment breaks Chatarium, rerun the smallest canonical capture that exercises the failure. Compare against the last known-good observation before changing adapter code. Preserve both revisions.
