# Browser flight recorder

This directory contains Chatarium's **P0 emergency durability layer** for the official ChatGPT web client.

`flight-recorder.user.js` is a Tampermonkey-compatible userscript. It does not replace ChatGPT's network stack. Its job is narrower and urgent: make the current site much less capable of destroying text that has already existed on your machine while the native client and direct protocol adapter are being built.

## Install

1. Install Tampermonkey in Edge/Chrome.
2. Create a new userscript.
3. Replace its contents with `flight-recorder.user.js` from this directory and save.
4. Reload `https://chatgpt.com/`.
5. Confirm a small **Chatarium** status panel appears in the lower-right corner.

The userscript runs only on `chatgpt.com`.

## What version 0.7.2 protects

- **Per-conversation draft WAL.** Composer text is synchronously copied into `localStorage` and then archived into IndexedDB. Navigating to another chat does not intentionally overwrite another conversation's synchronous draft record.
- **Separate send-intent WAL.** A send attempt is synchronously journaled *before* the site's normal bubbling send handler runs. Later draft mutations cannot erase this record.
- **Send confirmation.** When the corresponding user message appears in the rendered transcript, the recorder marks the matching send intent confirmed. If that confirmation never arrives, the send remains explicitly unresolved rather than being guessed away.
- **Latest-assistant WAL.** The newest rendered assistant text is copied into a separate synchronous emergency record while the streamed transcript is also archived into IndexedDB. A later composer clear or route transition cannot overwrite this slot.
- **Visible-error WAL.** Visible alert/toast text is preserved separately and journaled as an event. A message such as `Message delivery timed out` therefore survives after the toast disappears.
- **Incremental transcript snapshots.** Rendered user and assistant message text is archived into IndexedDB. A streaming assistant message updates one stable observed record when a message ID or stable transcript position is available.
- **Connectivity and navigation events.** Browser online/offline transitions and route changes are journaled.
- **Recovery controls.** The status panel can copy the latest saved draft, copy the most recent send intent, copy the latest assistant snapshot, or export the complete local recorder state.
- **Private text-turn stream capture.** At document start the recorder wraps the page's existing `fetch` function and observes only `POST /backend-api/f/conversation`. It clones the returned response and incrementally journals decoded response-stream chunks into IndexedDB without reading request headers, cookies, request bodies, Sentinel values, conduit tokens, or browser credential stores. The site's original response remains the branch consumed by ChatGPT.
- **Protocol-backed reconciliation.** For the v1 stream shape validated by snapshot `2026-09-29.002`, the recorder parses completed SSE frames in parallel with the raw chunk journal. A positively observed user `input_message` can confirm the matching pending send intent using the stream's canonical conversation ID. A positively observed `channel="final"` assistant text message is reconstructed incrementally into the existing assistant WAL and message archive. Derived events preserve the evidence source instead of hiding that the conclusion came from protocol rather than DOM observation.
- **Explicit protocol-read experiments.** Read capture is OFF on every page load. While armed, same-origin `GET`/`HEAD` `/backend-api/` requests are observed without reading request headers, cookies, auth material, request bodies, or browser storage. Each Arm interval is a distinct run with its own ID and request/capture/skip/error/byte counters. Requests are bound to the run active when they begin, so a late response from an earlier run cannot contaminate a later one.

The separate safety records are intentional. An empty post-send composer must not destroy the attempted user message, and a later page mutation must not destroy the assistant text that already reached the machine.

## Status panel

The lower-right panel reports whether the browser is online, whether the current conversation has a non-empty saved draft, how many send intents remain unresolved, whether an assistant snapshot exists, and the most recently observed visible site error. When protocol reads are armed it also reports eligible request count, captured response count, skipped response count, errors, and bytes. This distinguishes "the site made no eligible request" from "the recorder saw the request but failed/skipped the response."

An unresolved send is **not automatically an error**. It means Chatarium observed local send intent but has not yet observed enough evidence to classify the remote outcome. That distinction is central to the project.

The assistant WAL is deliberately bounded to the newest 500,000 characters. Typical responses fit entirely. If an exceptionally large rendered response exceeds that bound, the emergency slot keeps the newest tail and records that the prefix was truncated; IndexedDB transcript snapshots remain the longer-term archive.

## Export and recovery

Press **Ctrl+Shift+Alt+E** while ChatGPT is open to download a JSON export containing:

- current per-conversation draft WAL;
- bounded send-intent journal;
- latest assistant emergency snapshot;
- latest visible-error record;
- archived events;
- per-conversation draft records;
- observed transcript messages;
- private `network-stream-*` events for captured text-turn response streams, including ordered decoded chunks, status/content type, byte counts, truncation/error observations, and one local stream identifier;
- explicit protocol-read run summaries and `protocol-read-*` events. Each read event carries its run identity, so back-to-back experiments remain separable even when an earlier response completes late.

The status panel provides **Copy draft**, **Copy last send**, **Copy assistant**, and **Export** actions.

For diagnostics from DevTools console:

```js
await ChatariumFlightRecorder.status()
await ChatariumFlightRecorder.exportAll()
await ChatariumFlightRecorder.copySavedDraft()
await ChatariumFlightRecorder.copyLatestSendIntent()
await ChatariumFlightRecorder.copyLatestAssistant()
ChatariumFlightRecorder.readSendIntents()
ChatariumFlightRecorder.readAssistantWal()
ChatariumFlightRecorder.readErrorWal()
```

## Failure semantics

The recorder deliberately distinguishes these states:

```text
authored locally
    ↓
send intent durably recorded
    ↓
site send attempted
    ↓
remote outcome may be unknown
    ↓
rendered user message observed → confirmed

assistant text rendered
    ↓
latest assistant WAL updated synchronously
    ↓
IndexedDB transcript projection updated

visible site failure
    ↓
last-error WAL + append-only event
```

A timeout, disconnect, page crash, or frontend exception between the middle states must not be rewritten as either "definitely failed" or "definitely succeeded." Future protocol-level reconciliation will resolve more of these cases without relying on the DOM.

## Important limitations

This remains a browser flight recorder rather than a complete network-protocol recorder. DOM selectors can change when ChatGPT changes. Assistant text is observational and may miss content that never reached/rendered in the page. A DOM transcript is not treated as canonical remote state.

Version 0.6 added one deliberately narrow protocol observation: response-stream capture for `POST /backend-api/f/conversation`. Version 0.7 adds an operator-armed read-only observation surface for same-origin `GET`/`HEAD` `/backend-api/` traffic, and v0.7.1 gives every armed interval an isolated run identity plus per-run diagnostics. Version 0.7.2 adds one narrower exception to the previous query-values-unknown rule: only on exact same-origin `GET`/`HEAD` `/backend-api/conversations/<id>` reads, only the keys `include_has_versions` and `num_turns` are inspected. Their occurrence order and duplicates are preserved. A value is retained only when it is empty, lowercase `true`/`false`, or an optional-minus decimal integer of at most 10 digits; any other value shape becomes `unsupported: true` without copying the raw value. Other query keys remain names only. Read capture remains OFF after each page load. Neither mode captures request bodies, request headers, authentication material, browser storage, WebSocket frames, or frontend assets. The stream clone is bounded to 8,000,000 captured bytes; read responses are bounded to 1,000,000 bytes each and 4,000,000 bytes per run. Exceeding a limit is recorded explicitly and only Chatarium's observation branch is cancelled.

Version 0.6 intentionally **does not auto-inject recovered text into the composer**. Copying recovered text is safe; mutating a React-controlled editor without a verified adapter can create a second class of data-loss bugs. Automatic restore belongs behind a tested site adapter.

`confirmed` records now retain their confirmation evidence. DOM transcript observation remains one confirmation surface; v0.6 can also confirm from a positively parsed protocol `input_message`. These remain evidence classifications rather than a claim that any one remote signal is universally authoritative.

Visible-error capture is intentionally conservative: it observes `role="alert"` and known toast containers rather than scraping arbitrary red-looking text from the page.

## Privacy

The local archive contains conversation text. Version 0.7.2 exports may also contain raw decoded response-stream content from controlled or personal turns. This is **private evidence**, not a publication-ready sanitized artifact. It remains in browser storage until the browser profile/site data is cleared. Exports contain that material too. Do not commit personal exports to this public repository.

Protocol fixtures should use controlled non-sensitive test conversations and follow `protocol/CAPTURE_PLAYBOOK.md` before anything is committed. A v0.7.2 private export may contain the narrowly approved C02 query literals above; older v0.7.0/v0.7.1 exports contain only query-key names, so their query values remain unknown and must never be backfilled from assumption.

## Edge account-history bridge (current critical-path implementation)

The replacement runtime now lives in [`edge-bridge/`](edge-bridge/). It is a sideloadable Manifest V3 Edge/Chromium extension with narrow `chatgpt.com` + loopback host permissions, a service-worker-owned typed loopback client, passive first-party account-context observation, and explicit MAIN-world execution for the evidence-backed authenticated GETs.

The desktop no longer accepts `page` or `tampermonkey` as successful critical-path history transports. Extension results must carry proof of extension version, correlated desktop roundtrip, exact ChatGPT tab, MAIN-world execution, account-context presence, request-profile identity, and HTTP status. Rust then adds parser/semantic evidence; exact-conversation synchronization is not called complete until the matching live mirror is durably committed.

**Implementation is not live-browser validation.** Do not infer that the extension works in the target Edge environment merely because the files or automated checks pass. Human QA remains blocked behind the instrumentation and CI gate in `docs/HUMAN_QA.md` and issue #102.

## Account bridge (retired critical-path prototype)

> **Retired for production account-history transport.** `account-bridge.user.js` is retained as protocol/engineering evidence only. Do not continue iterating it as Chatarium's critical runtime bridge. The 2026-10-03 live QA sequence demonstrated unstable environment-dependent transport and insufficient execution/request-context guarantees. See [the complete postmortem](../docs/postmortems/2026-10-03-chatgpt-history-bridge.md).
>
> Tampermonkey remains acceptable for disposable experiments, observation, and the separate emergency flight recorder. The replacement account-history transport must be a purpose-built Edge/Chromium extension with explicit end-to-end diagnostics before human QA.

`account-bridge.user.js` was a deliberately narrow Tampermonkey prototype for P3 live mirroring. It is **not** a generic HTTP proxy and it does not copy reusable ChatGPT credentials into Chatarium.

The bridge splits authority deliberately:

- authenticated ChatGPT requests execute inside the already-signed-in `chatgpt.com` browser context;
- Tampermonkey's privileged loopback transport carries only typed bridge commands/results to `127.0.0.1:43117`;
- cookies, bearer/session tokens, request headers, Sentinel material, browser storage, and account identifiers never cross the bridge;
- exact-conversation reads construct the C02 resource observed in protocol snapshot `2026-10-03.001`: `/backend-api/conversations/<id>?num_turns=10&include_has_versions=true`;
- v0.2 adds one fixed ordinary-history command for the observed first page only: 20 recent, non-archived, non-starred conversations at offset 0; the bridge rejects any different resource profile instead of becoming a generic proxy;
- history discovery and exact-conversation fetches are explicit typed commands; there is no automatic pagination or rate-limit retry;
- non-200 responses return status metadata but not remote body text;
- successful JSON bodies are bounded to 4 MiB before crossing loopback;
- authentication probing reads only the HTTP status of `/backend-api/me` and discards its body.

Bridge v0.3 prefers a direct page-context loopback request to `127.0.0.1:43117`. On modern Edge this uses the browser's Local Network Access permission model; Chatarium's loopback server answers CORS/preflight only for `https://chatgpt.com`, and still requires the typed bridge marker and endpoints. If direct page transport is unavailable, the userscript can fall back to Tampermonkey's privileged `GM_xmlhttpRequest`.

Bridge v0.4 also preserves ChatGPT's active account context without exporting it from the browser. At document-start the userscript passively observes first-party same-origin fetches and retains only the `ChatGPT-Account-ID` header value in page-local memory. History enumeration and exact-conversation reads are refused until that first-party account context has been observed. The raw account ID is never sent over loopback, written to the journal, printed to the console, or surfaced in desktop diagnostics; Chatarium receives only a boolean proof that account context was present.

Desktop history status is intentionally evidentiary rather than cosmetic. A successful list displays safe proof fields such as loopback transport, `account-context=yes`, remote HTTP status, parsed item count, and remote total. A syntactically valid empty result without account-context proof is rejected instead of being shown as “0 recent chats.”


### Chromium / Edge transport notes

Tampermonkey 5.5.0 on Chromium/Edge 153 has an upstream background-networking regression that can stall or abort GM requests. Because the user's current environment matches that exact combination, v0.3 deliberately does not rely on the GM fallback there. The page-context route avoids Tampermonkey's MV3 background lifetime entirely.

Edge 143+ gates public-site access to localhost/local-network endpoints behind Local Network Access permission. The first direct bridge attempt may therefore show an Edge permission prompt for `chatgpt.com` to access the local network/loopback. Allowing that permission lets the page transport reach Chatarium. Standard CORS remains restricted to `https://chatgpt.com`.

On unaffected Tampermonkey/browser versions, `@connect 127.0.0.1` plus appropriate Tampermonkey Site Access remains a fallback transport.

This is still a userscript bridge, not Chrome Native Messaging. A separate native-messaging extension is unnecessary unless both page Local Network Access and Tampermonkey's privileged fallback prove unusable in the target browser.

This userscript is intentionally separate from `flight-recorder.user.js`. The recorder remains `@grant none` and keeps its page-context durability semantics; adding privileged Tampermonkey grants to it would unnecessarily change that execution boundary.

The desktop half of this bridge must bind loopback only, require the bridge marker header, expose typed endpoints only, and never add a generic arbitrary-URL/method forwarding surface.

### Historical result

The prototype established several reusable facts:

- a browser-to-desktop localhost roundtrip was achieved in v0.3;
- authenticated ChatGPT page execution could be reached;
- exact C02 reads and typed result transport were viable;
- a syntactically valid history response could still be semantically wrong when first-party account context was omitted;
- the supplied HAR showed `ChatGPT-Account-ID` is part of the first-party conversation-list context;
- a later v0.4 attempt to observe/preserve that context regressed to no loopback traffic in the target environment.

The exact v0.4 regression cause was not established. That uncertainty is part of the reason the userscript was retired rather than debugged indefinitely.

Do not interpret the presence of this file or its passing syntax tests as evidence that the live account bridge works in the target browser.

