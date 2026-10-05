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
