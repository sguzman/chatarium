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

The local archive search is rebuilt from the durable observed catalog and the visible transcript
projection of durable mirrors. `ALL LOCAL DATA` searches cached titles for every observed catalog
entry and visible text for locally mirrored entries; `TITLES` searches titles only; and
`MIRRORED TRANSCRIPT TEXT` searches only projected user/assistant transcript text from complete or
partial local mirrors. Hidden, internal, tool, and reasoning content is never indexed. An
unmirrored conversation can match by title but cannot match by transcript text.

Search results identify `TITLE` versus `LOCAL TRANSCRIPT`, and transcript results show a compact
local snippet only in the desktop UI. Mirror-state filters compose with search across `ALL`,
`MIRRORED`, `MIRRORED · PARTIAL`, `NOT MIRRORED`, `TRANSIENT FAILURE`, `RATE LIMITED`, and
`STRUCTURAL FAILURE`. `Ctrl+K`, Up/Down, Enter, and Escape provide keyboard navigation. Selecting
a local result opens the existing offline read path; selecting an unmirrored title only selects
the catalog row and never fetches it.

The search index is a rebuildable projection, not a new source of truth. Search performs no
authentication probe, browser startup, remote HTTP, or queue mutation, and it does not imply
account-wide completeness.

Locally mirrored transcripts open in an offline reader sourced only from the existing visible
user/assistant projector. Messages are clearly grouped by role with durable sequence/provenance
metadata, readable spacing, preserved paragraph line breaks, and optional durable timestamps.
The reader recognizes headings, paragraphs, unordered and ordered lists, block quotes, inline
code, and fenced code blocks. Code is monospaced, whitespace-preserving, horizontally scrollable,
and has a local copy action; copying any message or code block uses only the desktop clipboard.

When a transcript result opens a mirror, matching visible text is highlighted and Previous/Next
controls plus `Ctrl+P`/`Ctrl+N` navigate the hits. `PageUp`, `PageDown`, `Home`, and `End` provide
local reading navigation. Per-conversation scroll offsets are stored in disposable
`local-reader-state.json` UI metadata, separate from the authoritative journal, and restored on
reopen when available. Opening, scrolling, searching, highlighting, copying, and restoring a
reader position never probes auth, starts Chromium, invokes the bridge, issues HTTP, or mutates the
remote queue.

Partial mirrors retain a persistent `MIRRORED LOCALLY · PARTIAL` banner and explain that unavailable
earlier or structurally omitted content is not present locally. Hidden, internal, tool, and
reasoning content remains excluded before rendering and indexing.

The machine-controlled local QA paths are:

```text
chatarium-qa local-mirror-status
chatarium-qa local-transcript --catalog-index N
chatarium-qa local-search-status
chatarium-qa local-search --query synthetic-safe-query
chatarium-qa local-reader-status --catalog-index N --query synthetic-safe-query
```

Their output is structural only: no titles, message text, snippets, remote identities, or raw
response bodies are emitted. The reader status path reports only selected index, projected message
count, Markdown/code block counts, search-hit count, partial state, scroll restoration, and
local-read-only transport flags. All paths use the durable journal and cached catalog only.

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
