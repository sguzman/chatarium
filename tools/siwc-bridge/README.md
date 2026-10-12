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
