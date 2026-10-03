# Capture playbook

This playbook creates protocol evidence that can later be compared across ChatGPT frontend revisions.

## Preparation

Use a controlled, non-sensitive test conversation. Open browser developer tools before the experiment and enable network log preservation. Capture one behavior at a time where practical.

Do not intentionally expose reusable credentials for the sake of documentation. Raw captures may contain them incidentally; keep raw evidence outside Git and sanitize before committing.

## Capture-before-code rule

For undocumented consumer-web integration, current network evidence is mandatory before reproducing the request in runtime code.

If the existing corpus does not establish the complete request, collect a raw HAR, CDP trace, or equivalent first-party network artifact from the exact target flow. Do not infer missing values from endpoint names, old captures, UI behavior, or successful authentication.

The 2026-10-03 history-bridge incident is the canonical example: path/query evidence was insufficient because the first-party conversation-list request also carried account-selection context in a request header.

A manual HAR is acceptable and should be requested immediately when it is the fastest safe way to obtain missing evidence. Do not delay or discourage capture merely because an automated harness is preferred.

## Request-context checklist

For every target request, record or classify:

- method;
- full path;
- query keys and literal values needed for parity;
- duplicate/order semantics where observed;
- body/content type;
- relevant origin/referrer/credential behavior;
- request headers;
- account/workspace/project selectors;
- challenge/Sentinel dependencies;
- request initiator and ordering prerequisites;
- response status/content type;
- response schema;
- identity/pagination metadata.

Committed evidence may redact private values, but it must preserve enough typed presence/classification information to show that a context-bearing header existed.

Do not commit reusable credentials or raw account identifiers.

## Baseline captures

### C00 — idle page load

Action: load `https://chatgpt.com/` and do nothing until network activity settles.

Purpose: distinguish boot/session/configuration traffic from conversation actions.

### C01 — conversation list

Action: expose/load the conversation list without opening a new thread if possible.

Purpose: identify list/pagination traffic.

### C02 — open existing conversation

Action: open a controlled existing thread.

The controlled thread does not need to be a particular historical C02 tab. If a previous test thread is inaccessible, create a fresh ordinary non-sensitive test conversation first, let it finish, navigate away, and use the later reopen as C02. The setup mutation must happen before read capture is armed; the capture itself should contain only the reopen action.

Purpose: identify conversation retrieval and any lazy secondary requests. For the query-value follow-up, use Flight Recorder v0.7.2 or newer, arm reads immediately before opening the controlled thread, perform only that open action, then disarm. The recorder may retain literals only for the approved `include_has_versions` and `num_turns` keys on the exact conversation-resource request; do not inspect or transcribe query values manually. Older captures remain values-unknown.

### C03 — new text turn, complete

Use the exact canonical prompt:

```text
respond with exactly CHATARIUM_PROTOCOL_TEST_001
```

Expected assistant marker:

```text
CHATARIUM_PROTOCOL_TEST_001
```

Do not add Markdown backticks or otherwise rewrite this text in a canonical C03 run.

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

HAR is acceptable bootstrap evidence for initial protocol archaeology and can be collected manually when doing so is faster than finishing the automated harness first.

A raw HAR must still be treated as private evidence. Browser "sanitized" export is not sufficient proof that the file contains no reusable tokens, account/device identifiers, signed websocket parameters, or private conversation/context bodies.

HAR may also omit streaming response bodies. Snapshot `2026-09-29.001` observed a `text/event-stream` response whose SSE body was absent from the export.

Therefore HAR is useful for endpoint/request/order discovery, while later recorder work should preserve SSE/fetch streaming, WebSocket activity, and loaded asset identities directly.

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
