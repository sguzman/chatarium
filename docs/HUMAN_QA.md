# Human QA protocol

Chatarium assumes human testing should be explicit, reproducible, and low-risk. A request such as "try it" is not sufficient.

Whenever human QA is required, the handoff must state all of the following before the operator acts.

## Required handoff fields

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

State exactly what ChatGPT needs back: console text, screenshot, exported JSON, HAR, filename, SHA-256, error message, or a simple yes/no observation.

Do not request credentials, cookies, authorization headers, reusable session tokens, or unrelated private conversation content.

### 8. Do not touch

Call out anything that could contaminate evidence or destroy useful state: do not clear site storage, do not manually redact the raw HAR before offline ingestion, do not retry an ambiguous send, do not edit a generated archive, etc.

### 9. Cleanup

If the test creates disposable files, conversations, environment variables, or installs, say what can be removed and what should be retained.

## Safety rule for ambiguous remote actions

If ChatGPT reports a timeout after a send, do **not** blindly resend merely to make a test pass. Preserve the local evidence and record the outcome as unknown until reconciliation can establish what the remote service actually did.

## Private evidence rule

Protocol experiments should use synthetic/disposable conversations whenever possible. Browser flight-recorder exports can contain real conversation text and are private artifacts. Raw HARs can contain credentials and are never committed to the public repository.
