# Chatarium Sign in with ChatGPT bridge

This local bridge connects the Rust desktop client to OpenAI's official
**Sign in with ChatGPT DevKit**.

The DevKit itself is pinned as the git submodule at
`vendor/openai-sign-in-with-chatgpt-devkit`. Chatarium does not copy or
relicense the DevKit source. The pinned upstream code remains subject to its own
license.

The bridge is deliberately credential-blind at its Rust boundary. It exposes
newline-delimited JSON commands for session state, sign-in, profile selection,
model discovery, disconnection, and streamed Responses API inference. Access,
refresh, and ID tokens never appear in bridge stdout.

## Bootstrap

One-time development bootstrap:

```sh
node tools/siwc-bridge/bootstrap.mjs
```

The bootstrap initializes the pinned submodule when necessary and builds only
the DevKit's `@siwc/local` package.

## Linux credential protection

The current Linux alpha encrypts the DevKit credential state with AES-256-GCM.
The encryption key is stored in the desktop Secret Service through
`secret-tool`; it is not stored in Chatarium's journal or bridge files. If the
Secret Service is unavailable, credential persistence fails closed.

This is intentionally separate from Chatarium's append-only conversation
journal.


## Route capability probes

The pinned DevKit intentionally exposes a text-only `streamResponse` surface even
though the Sign in with ChatGPT route documents additional Responses
capabilities. Chatarium keeps that product boundary intact and uses a
developer-only probe command to test the route without exporting OAuth tokens or
forking the credential-owning DevKit.

The desktop exposes the full probe matrix under **Diagnostics → SIWC CAPABILITY
PROBES**. It uses the currently selected account-visible model and writes the
sanitized result matrix to `siwc-capability-probes.json` beside Chatarium's
local journal. No OAuth credential or bearer token is written to that report.

The standalone developer CLI remains available for focused debugging:

```sh
node tools/siwc-bridge/capability-probe.mjs --model <slug>
node tools/siwc-bridge/capability-probe.mjs --model <slug> --probe reasoning
node tools/siwc-bridge/capability-probe.mjs --model <slug> --json
```

Both paths require an already connected ChatGPT profile with plan-usage sharing
enabled and send real, intentionally tiny Responses requests. Results are
therefore account/model specific and consume normal plan usage.

Security boundary:

- the access token stays inside the upstream DevKit;
- the bridge temporarily patches only the outgoing Responses JSON body for one
  request;
- only `input`, `reasoning`, `text`, and `tools` are patchable;
- `model`, `store: false`, and `stream: true` remain controlled by the
  normal DevKit request;
- the original global fetch is restored immediately after the probe;
- normal inference and probes share a single-flight Responses lane: concurrent
  stream/probe requests receive `response_busy` or `probe_busy` instead of
  risking cross-turn request-patch interception;
- every Responses operation requires a bounded request ID for attribution and
  cancellation; the lane is released after success, error, or cancellation,
  with offline regressions in `response-lane.test.mjs`.

The product UI exposes only the fixed probe suite. It does not expose arbitrary
`probe_response` request patching. The raw command remains an internal bridge
mechanism used to turn the capability ledger's remaining unknowns into finite,
reproducible experiments.


## Bounded read-only network recovery

The active `models` command is explicitly read-only and now uses
`retryReadOnly` from `retry-policy.mjs`. It retries only recognized transient
read failures: HTTP 408/429/500/502/503/504, or a narrow set of transport
error codes. A returned 400/401/403, malformed request, or cancellation is
terminal even if an upstream error happens to claim `retryable`.

The default ceiling is **three total attempts**, with exponential delay
starting at 300 ms, bounded jitter, a 3-second maximum individual delay,
and a 6-second aggregate delay budget. A `Retry-After` header takes
precedence; if it exceeds the allowed delay, Chatarium does not retry early.
Retry notices are credential-free `read_retry_scheduled` bridge events carrying
request ID, operation, failure category, status, attempt, next attempt, and
scheduled delay. They are diagnostic events, not proof of server receipt.

For more granular control, the bridge accepts an optional
`CHATARIUM_READ_RETRY_POLICY_JSON` environment value, validated before
issuing a model-discovery read. For example:

```json
{
  "maxAttempts": 3,
  "initialDelayMs": 300,
  "maxDelayMs": 3000,
  "maxTotalDelayMs": 6000,
  "jitterRatio": 0.2,
  "categories": {
    "rate_limit": { "enabled": true, "maxAttempts": 3 },
    "http_transient": { "enabled": true, "maxAttempts": 2 },
    "network_transient": { "enabled": false }
  }
}
```

Only these explicit categories are recognized. Unknown keys, invalid types,
oversized JSON, and unsafe limits fail closed; the maximum configurable
attempt count is five and the total delay budget cannot exceed 90 seconds.
A disabled category is never retried, and both the total and category caps
apply. This is an advanced operator setting, **not yet an in-app editor**,
and it controls only the read-only `models` operation. Retry notices identify
`stage: model_discovery`; the Rust side logs a sanitized classification
without treating it as transcript content.

**This helper is never applied to ordinary `stream_response` inference POSTs
or to MCP/tool execution.** A network reset without a response after a POST
may leave remote delivery unknowable; it must not cause a blind resend.
The existing native 429/recovery mechanism is separate and is not replaced
by this read-only model discovery policy. The current increment does not
claim to be the full granular native retry configuration UI.

The offline bridge test injects one model-discovery 429 and asserts a single
scheduled retry followed by success. It separately simulates an inference
network drop after attempted dispatch and asserts exactly one attempted
request and no read-only retry; unit tests cover failure classification,
cooldowns, and attempt limits. Both test suites are in Windows and Linux CI.

## Live inference activity evidence (native journal and reader)

Ordinary `stream_response` turns now emit separate
`agent_activity` NDJSON observations with a bounded request identity,
source label, per-turn sequence, source UTC timestamp, and elapsed monotonic
milliseconds. Events cover request start, attempted HTTP delivery, observed
HTTP acceptance, first and subsequent received text chunks, completion,
and failure. Terminal events distinguish `not_dispatched`,
`wire_unconfirmed`, `http_rejected` (explicit HTTP 4xx/5xx),
`http_acknowledged` and `assistant_delta_seen`; cancellation is separately
flagged. The activity stream records the numeric status for explicit HTTP
rejections rather than reporting a known HTTP 429 as an ambiguous lost reply.

These events contain no prompt bodies, delta text, credentials, fabricated
reasoning transcripts, or invented tool actions. The original `delta`,
`transport_progress`, and `result` frames remain compatible. The native
side validates and appends activity under the exact locally owned
conversation/turn identity, replays it from the durable journal, and shows a
searchable amber activity card in the chat reader. `CHATARIUM_LOG=debug`
also exposes sanitized diagnostic metadata. Cards are never model-authored
messages or automatically admitted inference context. Genuine upstream
reasoning-summary/action streams, when available, and richer per-event
filter/controls remain future work.

The offline bridge integration tests assert that an actual synthetic streamed
turn emits ordered activity events, and that a disconnected inference POST is
marked `wire_unconfirmed` without retrying. Separate deterministic tests
verify stage order, monotonic timing, cancellation, provenance, and privacy.

## Offline alpha transport contract

Run `node --test tools/siwc-bridge/test/alpha-protocol.test.mjs` to
exercise the **real Node bridge process** through its stdin/stdout protocol.
The test-only `--import` preload substitutes a deterministic fake
DevKit and replaces network `fetch` with an offline stub. It verifies
session/model discovery, credential-free Linux Secret Service sign-in
preflight (including fail-closed behavior), patched and unpatched streamed turns, delta ordering,
overlap rejection, cancellation, recovered next-turn behavior, disconnect,
and process shutdown. No credential provider, browser, API key, or account is
accessed. These passing tests **do not establish** a successful live ChatGPT
OAuth/Responses session; that remains the alpha gate in issue #89.


On Linux, a missing Secret Service key is not silently replaced if the
DevKit's encrypted chatgpt-auth.json already exists. The original file is
left untouched so keyring restoration remains possible. Early failure of
the secret-tool helper also cannot crash the sidecar through a broken stdin
pipe. Both conditions are covered by isolated, credential-free preflight tests.
