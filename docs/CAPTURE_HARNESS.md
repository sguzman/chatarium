# Capture harness

Chatarium's protocol-observation loop must not depend on a human manually operating DevTools, exporting HAR files, creating temporary directories, finding downloads, or running a chain of Cargo commands.

The capture harness is the Windows-first automation boundary that turns a controlled ChatGPT web experiment into one portable sanitized artifact plus private local evidence.

## Design goal

After one-time setup, the normal operator interaction should be approximately:

```text
chatarium-capture run C03-send-text
```

Chatarium owns everything else:

1. choose an isolated capture directory;
2. launch a dedicated Microsoft Edge profile;
3. enable a localhost-only Chrome DevTools Protocol endpoint;
4. attach to the intended `chatgpt.com` page target;
5. record the experiment incrementally;
6. drive the synthetic interaction when the experiment defines one;
7. preserve ambiguous failures instead of guessing;
8. sanitize and derive structural artifacts;
9. validate the portable bundle;
10. print the one output path the operator may need to return.

Human QA exists only for state or perception that cannot reasonably be automated.

## Why CDP

Microsoft Edge exposes the Chromium DevTools Protocol to custom tooling. A client can discover page targets through the local DevTools HTTP endpoints and attach to a target's DevTools WebSocket. The protocol exposes browser/network/runtime events that are sufficient for a first-party-page observation harness.

The Windows `smoke-edge` command uses Chromium's browser-wide DevTools pipe in ASCIIZ mode. It sends read-only CDP commands from the dedicated Chatarium profile and does not depend on a TCP listener or `DevToolsActivePort`; pipe startup failures are reported without falling back to TCP. The localhost TCP transport remains available to other capture paths and diagnostics. This transport choice does not enable the deferred `init` or `run` flows or define a portable capture-bundle format.

References:

- https://learn.microsoft.com/en-us/microsoft-edge/devtools/protocol/
- https://developer.chrome.com/blog/remote-debugging-port

Modern Chromium intentionally constrains remote debugging of the default browser data directory. Chatarium treats that as a desirable boundary, not something to work around: the harness owns a dedicated browser profile and never depends on the user's everyday browser profile.

## Persistent profile

Default location on Windows:

```text
%LOCALAPPDATA%\Chatarium\capture-browser\edge-profile\
```

This is a Chatarium-owned browser profile, not a copy of the user's normal Edge profile.

The profile may contain the user's ChatGPT login after the user authenticates once during `chatarium-capture init`. It is therefore private local state and must never be archived into the protocol corpus or portable capture bundle.

The harness must refuse to use known/default Edge user-data paths. A caller cannot override this protection with a simple `--force` flag.

## Commands

### `chatarium-capture doctor`

Read-only diagnostics.

It reports:

- whether a supported Edge executable can be found;
- Edge version;
- Chatarium capture profile path;
- whether that path is obviously distinct from default Edge profile locations;
- whether a stale harness lock exists;
- whether previous unfinished capture runs exist;
- whether the local artifact directories are writable;
- harness version.

`doctor` must not launch Edge, log in, modify the profile, delete files, or contact ChatGPT.

### `chatarium-capture init`

Operator-confirmed bootstrap for the dedicated persistent Edge profile.

Behavior:

1. create the dedicated capture profile directory if absent;
2. launch Edge with that persistent profile at `https://chatgpt.com/` using the Windows anonymous-pipe DevTools transport;
3. let the operator sign in through the normal ChatGPT UI if needed;
4. wait for one terminal Enter confirmation;
5. verify that a page target is on the exact `https://chatgpt.com` host;
6. persist bootstrap metadata in a private diagnostic run and close the harness-owned browser cleanly.

The dedicated profile remains on disk for later runs. Authentication is not programmatically verified. This command must not import credentials from another profile, extract credentials or cookies, inspect browser storage, automate login, or bypass any authentication flow. Closing stdin before Enter is an explicit operator abort. A failed final target check preserves the profile for another manual attempt.

### `chatarium-capture run C00-idle-load`

Canonical idle-load experiment.

Behavior:

1. create a private run directory and append-only capture journal;
2. launch the dedicated Edge profile;
3. attach to the `chatgpt.com` page target;
4. enable required CDP domains;
5. navigate/load the standard ChatGPT surface;
6. record until the experiment's settle rule is met;
7. finalize private evidence;
8. generate the portable sanitized bundle;
9. validate hashes/manifest;
10. close harness-owned Edge.

No ChatGPT conversation mutation is performed.

### `chatarium-capture run C03-send-text`

Canonical deterministic text-turn experiment.

Default exact synthetic request text:

```text
respond with exactly CHATARIUM_PROTOCOL_TEST_001
```

Expected deterministic assistant marker:

```text
CHATARIUM_PROTOCOL_TEST_001
```

The exact request and expected marker belong to the experiment definition and must be written into the capture manifest. They are not silently rewritten.

Behavior after page readiness:

1. locate/focus the official ChatGPT composer through the live page;
2. insert the exact synthetic request using CDP input/runtime primitives;
3. trigger the normal official-page send interaction;
4. record positive evidence of the rendered/remote user turn if observed;
5. record assistant output/network evidence incrementally;
6. stop when the expected marker is positively observed and the page/network settle rule succeeds, or when the experiment timeout is reached;
7. if transport/page state becomes ambiguous, record ambiguity rather than retrying the turn automatically;
8. finalize and package the run.

The harness must never retry an ambiguous send merely to make a capture pass.

## CDP transport architecture

The browser-control layer must be behind a mockable interface so CI does not require a live browser or account.

Suggested boundary:

```rust
trait BrowserTransport {
    fn browser_version(&self) -> Result<BrowserVersion>;
    fn list_targets(&self) -> Result<Vec<TargetInfo>>;
    fn attach(&mut self, target: TargetId) -> Result<Box<dyn PageSession>>;
}

trait PageSession {
    fn command(&mut self, method: &str, params: Value) -> Result<Value>;
    fn next_event(&mut self, deadline: Instant) -> Result<Option<CdpEvent>>;
}
```

The exact Rust shape may differ, but browser/process management, CDP framing, experiment logic, capture persistence, and sanitization must remain separable.

## Debug endpoint

The harness chooses an available local port and binds/launches debugging for localhost use only. The port is an implementation detail, not part of the persistent configuration contract.

The run manifest records the CDP protocol version but not reusable browser credentials.

A stale debugging process/lock must fail visibly rather than causing the harness to attach to an arbitrary existing browser.

## Capture journal

A capture is durable while it is happening. The harness may not buffer the whole experiment in memory and write it only at the end.

Private run directory:

```text
%LOCALAPPDATA%\Chatarium\captures\private\<run-id>\
├── run.json
├── events.jsonl
├── bodies\
├── frontend\
└── finalize.json
```

`events.jsonl` is append-oriented and flushed regularly. Each record has a monotonically increasing local sequence number.

A crash or lost network connection must leave enough information to inspect what had already happened.

## Events to observe

v0.1 should preserve at least:

- browser/process start and stop;
- target discovery/attachment;
- navigation and frame changes relevant to the main ChatGPT page;
- `Network.requestWillBeSent` metadata;
- `Network.responseReceived` metadata;
- request post data when CDP exposes it;
- response body after completion when CDP exposes it;
- loading success/failure;
- WebSocket creation/close and sent/received frames;
- relevant runtime/page exceptions;
- experiment actions such as composer located, text inserted, send triggered, marker observed;
- explicit timeout/interruption/ambiguity state;
- frontend JavaScript asset URLs and SHA-256 hashes where bodies are available.

The harness should preserve original CDP method names in the private event record rather than translating every remote observation into a guessed semantic name.

## Body storage

Large bodies are content-addressed rather than repeated inline:

```text
bodies\sha256-<digest>.bin
```

Event records reference the hash, MIME/type information, encoding, and byte count.

Size limits must be explicit. When a body is unavailable or intentionally omitted, record that fact rather than pretending it was empty.

## Private evidence vs portable bundle

Two products exist for every capture.

### Private evidence

Private evidence is the strongest local forensic source. It may contain controlled conversation bodies and browser/network details unsuitable for a public repository.

It stays outside Git.

Private evidence must still avoid intentionally collecting reusable credential values when they are unnecessary. In particular, cookie stores and password databases are never copied.

### Portable bundle

The bundle is the thing a user can hand back to ChatGPT for protocol analysis.

Default output:

```text
%LOCALAPPDATA%\Chatarium\captures\portable\<run-id>.chatarium-capture.zip
```

The portable bundle contains only sanitized/derived artifacts and a manifest.

Suggested layout:

```text
manifest.json
capture/
  events.sanitized.jsonl
  requests.json
  responses.json
  websocket.jsonl
frontend/
  assets.json
protocol/
  inventory.json
  observations.json
logs/
  harness.log
```

Exact layout may evolve, but the manifest is authoritative.

## Sanitization boundary

Sanitization rules belong in one reusable implementation shared with `tools/recorder`.

The portable bundle must not contain reusable values for:

- `Authorization`;
- `Cookie` / `Set-Cookie`;
- CSRF/session/access/refresh tokens;
- device/session secrets;
- URL credentials;
- known token-like query parameters;
- browser profile paths when they reveal irrelevant local identity;
- arbitrary credential-bearing fields discovered by the sanitizer.

Header and query *names* may be preserved when useful for structural documentation.

Synthetic request/assistant bodies for canonical experiments may remain because their exact content is part of the experiment specification.

Sanitization is defense-in-depth, not an oracle. Portable-bundle validation should reject obvious credential patterns and report uncertainty instead of automatically publishing anything.

## Experiment definitions

Experiments are data, not hard-coded scattered branches.

Suggested shape:

```toml
id = "C03-send-text"
description = "Create one deterministic synthetic text turn"
start_url = "https://chatgpt.com/"
timeout_seconds = 90

[action]
type = "send_text"
text = "respond with exactly CHATARIUM_PROTOCOL_TEST_001"

[success]
type = "assistant_text_contains"
text = "CHATARIUM_PROTOCOL_TEST_001"

[settle]
quiet_network_ms = 1500
quiet_dom_ms = 750
```

The final schema may differ, but the exact action must be manifest-visible and versioned.

## Identity and temporary ChatGPT conversation IDs

The first real flight-recorder QA demonstrated that ChatGPT can expose a temporary `WEB:` conversation identity before navigation settles on a canonical UUID. Capture tooling must preserve both observations and their temporal relation.

The harness itself should not guess that one identifier is canonical merely from syntax. Higher-level import/analysis may correlate identities when positive evidence exists, such as stable message IDs plus an observed navigation transition.

## Streaming/status placeholders

The page can temporarily render request placeholders such as `Thinking`. The harness records them as page observations with their DOM/CDP identity. It must not classify a transient placeholder as completed assistant content merely because it has `data-message-author-role="assistant"`.

## Failure semantics

A capture has an explicit final state:

```text
completed
completed_with_warnings
outcome_ambiguous
failed_before_mutation
failed_after_mutation
aborted_by_operator
```

These states describe the experiment/capture, not a moralized success/failure of ChatGPT.

For C03, `failed_after_mutation` or `outcome_ambiguous` must never trigger an automatic resend.

## Restart/finalization

`run.json` is created before browser mutation.

A finalizer can be rerun against an unfinished run directory. It must:

- validate journal sequence;
- content-address any remaining body files;
- sanitize deterministically;
- avoid duplicating finalized records;
- produce the same portable logical evidence for the same private journal;
- record warnings for unavailable late response bodies;
- never resume/replay a remote send merely because packaging was interrupted.

## One-time human workload

The only expected setup burden is the dedicated-profile login during `chatarium-capture init` if ChatGPT requires authentication.

The harness must explain exactly what surface is open and what is being persisted. It must not ask the user to copy cookies, tokens, profile databases, DevTools output, or browser-storage values.

## Recurring human workload

Target:

```text
chatarium-capture run C03-send-text
```

If a human observation is still necessary, the command itself should state one concise action, for example:

```text
Chatarium needs one observation:
Did the dedicated browser show a CAPTCHA? [y/N]
```

Do not emit a multi-step QA checklist when the program can determine the answer itself.

## Relationship to the protocol corpus

The capture harness creates evidence. It does not declare protocol truth.

```text
live official web client
        ↓
CDP capture harness
        ↓
private evidence
        ↓
sanitized portable bundle
        ↓
recorder/inventory/diff
        ↓
protocol snapshot + observations
        ↓
protocol adapter implementation
```

`protocol/` remains the empirical record. `crates/protocol` remains our implementation of selected observations.

## v0.1 non-goals

- bypassing ChatGPT authentication or service protections;
- copying a default browser profile;
- direct calls to undocumented ChatGPT endpoints;
- general-purpose browser automation;
- CAPTCHA solving;
- native desktop integration;
- automatic public publication of captures;
- interpreting every page/network field semantically.

## Codex implementation boundary

This specification is intentionally detailed enough for a coding agent to implement without redefining architecture.

Codex may choose internal Rust types and well-maintained dependencies, but it must not change these invariants without an explicit architecture revision:

1. dedicated browser profile only;
2. one-time normal login, no credential extraction;
3. incremental private capture journal;
4. no blind retry after ambiguous remote mutation;
5. shared sanitization implementation;
6. one portable output artifact;
7. experiment text/actions are exact and auditable;
8. CI can test logic without a live account;
9. human workload is minimized by automation rather than documentation.
