# Local mirror read contract

Chatarium's remote history catalog and durable mirror snapshots are a local read surface.
The desktop catalog combines the last observed catalog identity with replayed queue state and
local snapshot bindings. Catalog rows use explicit structural states:

- `REMOTE · NOT MIRRORED`
- `MIRRORED LOCALLY`
- `MIRRORED LOCALLY · PARTIAL`
- `TRANSIENT FAILURE`
- `RATE LIMITED`
- `STRUCTURAL FAILURE`

Selecting a locally mirrored row loads the validated snapshot from the local journal, replays the
snapshot audit, and projects the visible user/assistant transcript. This path does not probe
authentication, start Chromium, invoke the browser bridge, or issue remote HTTP. A partial mirror
remains readable but displays `MIRRORED LOCALLY · PARTIAL` and explains that older or structurally
unavailable content is not present in the local projection.

Selecting an unmirrored row only selects its cached catalog identity. Remote capture is an explicit
separate action (`Mirror from ChatGPT`) and is never triggered by local selection. The observed
catalog count is not an account-completeness claim.

The machine-controlled local QA paths are:

```text
chatarium-qa local-mirror-status
chatarium-qa local-transcript --catalog-index N
```

Their output is structural only: no titles, message text, or raw response bodies are emitted.
Both paths use the durable journal and cached catalog only.

The desktop is the production owner of remote scheduling. It exposes explicit `START MIRRORING`,
`PAUSE`, and `RESUME` controls and runs one capture at a time. Queue lifecycle events and snapshot
promotion are serialized through the existing persistence worker. A 429 or authentication loss
pauses the controller without destroying local browsing; transient and structural outcomes are
durably recorded and are not retried in an immediate loop.

Authentication remains fail-closed. A visible ChatGPT account shell is not sufficient authority for
mirroring. The production probe uses the first-party `/backend-api/me` oracle through the
authenticated page context: a normal `200` is authenticated, `401` or ordinary `403` is not
authenticated, and a `403` carrying a server challenge marker is classified as unknown/server
challenge rather than logout. The production worker stops and preserves the queue for either
unknown or unauthenticated state.

The production mirror controller implementation and its local tests are landed, but live
acceptance is still pending. The pending external condition is a Cloudflare challenge on
first-party backend requests; a challenge-marked 403 is not treated as logout. Local reading and
replay remain usable while remote capture is blocked, and future live acceptance may resume after
the challenge clears. The observed catalog and test fixtures do not establish account-wide
conversation completeness.

The structural controller inspection path is:

```text
chatarium-qa production-controller-status
```

It reconstructs the queue without browser or authentication work and reports the production
controller contract, concurrency, and structural queue counts.
