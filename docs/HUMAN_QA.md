# Human QA protocol

Chatarium assumes human testing should be explicit, reproducible, low-risk, and **rare**. A request such as "try it" is not sufficient, but neither is turning the operator into a manual CI runner.

## Automation-first rule

Before asking for human QA, automate everything that does not intrinsically require a person:

- checkout/update/build steps;
- syntax/format/check/test validation;
- fixture generation;
- log collection;
- file hashing;
- import/export validation;
- diffing and structural inspection;
- cleanup where it is safe to automate.

Human QA exists for observations Chatarium cannot yet obtain itself, such as interacting with the current ChatGPT UI, confirming a visual state, or producing a browser/network artifact that requires an authenticated human session.

**Do not push mechanical setup or diagnostic labor onto the operator merely because it is convenient for development.**

## Human burden budget

A normal QA handoff should aim for:

- one focused interaction sequence;
- no more than a few human actions;
- at most one artifact upload or screenshot unless multiple artifacts are genuinely necessary;
- no copying long console transcripts when the program can write a diagnostic bundle instead;
- no manual calculation, hashing, file comparison, or interpretation that code can perform;
- no installation during QA unless the test specifically concerns installation.

If a proposed test exceeds that budget, first build a harness or diagnostic command that collapses the work.

## Required handoff fields

Whenever human QA is still required, the handoff must state the following before the operator acts.

### 1. Goal

What question this test answers. Keep one QA run focused on one failure surface whenever practical.

### 2. Install / run

Exactly what software, script, binary, browser extension, or repository checkout is required. State whether anything new must be installed.

### 3. Surface being touched

Name the exact browser page, local file, directory, application, DevTools panel, or Chatarium component the operator will interact with.

### 4. Preconditions

State required starting conditions, such as:

- disposable vs private ChatGPT conversation;
- browser recorder version expected;
- whether Chatarium Desktop must be closed;
- whether DevTools Preserve log must be enabled;
- whether the network should remain normal or be intentionally interrupted.

### 5. Exact actions

Number every human action. Include literal test text and filenames where relevant. Do not depend on the operator guessing which button/menu/field is intended.

### 6. Expected result

Say what success looks like before the test begins. If multiple outcomes are informative, describe each and what it means.

### 7. Evidence to return

State exactly what ChatGPT needs back. Prefer one generated diagnostic bundle over many manually copied fragments. Request console text, screenshots, exported JSON, HARs, filenames, hashes, error messages, or yes/no observations only when the tooling cannot capture them itself.

Do not request credentials, cookies, authorization headers, reusable session tokens, or unrelated private conversation content.

### 8. Do not touch

Call out anything that could contaminate evidence or destroy useful state: do not clear site storage, do not manually redact the raw HAR before offline ingestion, do not retry an ambiguous send, do not edit a generated archive, etc.

### 9. Cleanup

If the test creates disposable files, conversations, environment variables, or installs, say what can be removed and what should be retained. Prefer a cleanup command/tool over a list of manual deletion steps.

## Safety rule for data destinations

Tools that can write durable Chatarium state must not silently fall back to the user's default data directory when a test or import destination was intended. Diagnostic/import tools should require an explicit destination when ambiguity could contaminate normal state.

## Safety rule for ambiguous remote actions

If ChatGPT reports a timeout after a send, do **not** blindly resend merely to make a test pass. Preserve the local evidence and record the outcome as unknown until reconciliation can establish what the remote service actually did.

## Private evidence rule

Protocol experiments should use synthetic/disposable conversations whenever possible. Browser flight-recorder exports can contain real conversation text and are private artifacts. Raw HARs can contain credentials and are never committed to the public repository.
