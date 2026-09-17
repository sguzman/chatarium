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

## What version 0.3 protects

- **Per-conversation draft WAL.** Composer text is synchronously copied into `localStorage` and then archived into IndexedDB. Navigating to another chat does not intentionally overwrite another conversation's synchronous draft record.
- **Separate send-intent WAL.** A send attempt is synchronously journaled *before* the site's normal bubbling send handler runs. Later draft mutations cannot erase this record.
- **Send confirmation.** When the corresponding user message appears in the rendered transcript, the recorder marks the matching send intent confirmed. If that confirmation never arrives, the send remains explicitly unresolved rather than being guessed away.
- **Latest-assistant WAL.** The newest rendered assistant text is copied into a separate synchronous emergency record while the streamed transcript is also archived into IndexedDB. A later composer clear or route transition cannot overwrite this slot.
- **Visible-error WAL.** Visible alert/toast text is preserved separately and journaled as an event. A message such as `Message delivery timed out` therefore survives after the toast disappears.
- **Incremental transcript snapshots.** Rendered user and assistant message text is archived into IndexedDB. A streaming assistant message updates one stable observed record when a message ID or stable transcript position is available.
- **Connectivity and navigation events.** Browser online/offline transitions and route changes are journaled.
- **Recovery controls.** The status panel can copy the latest saved draft, copy the most recent send intent, copy the latest assistant snapshot, or export the complete local recorder state.

The separate safety records are intentional. An empty post-send composer must not destroy the attempted user message, and a later page mutation must not destroy the assistant text that already reached the machine.

## Status panel

The lower-right panel reports whether the browser is online, whether the current conversation has a non-empty saved draft, how many send intents remain unresolved, whether an assistant snapshot exists, and the most recently observed visible site error.

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
- observed transcript messages.

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

This remains a browser flight recorder, not yet the direct network-protocol recorder. DOM selectors can change when ChatGPT changes. Assistant text is observational and may miss content that never reached/rendered in the page. A DOM transcript is not treated as canonical remote state.

Version 0.3 intentionally **does not auto-inject recovered text into the composer**. Copying recovered text is safe; mutating a React-controlled editor without a verified adapter can create a second class of data-loss bugs. Automatic restore belongs behind a tested site adapter.

Likewise, "confirmed" currently means *observed in the rendered user transcript*. It does not yet mean a protocol acknowledgement was captured. The protocol observatory will refine this distinction.

Visible-error capture is intentionally conservative: it observes `role="alert"` and known toast containers rather than scraping arbitrary red-looking text from the page.

## Privacy

The local archive contains conversation text. It remains in browser storage until the browser profile/site data is cleared. Exports contain that text too. Do not commit personal exports to this public repository.

Protocol fixtures should use controlled non-sensitive test conversations and follow `protocol/CAPTURE_PLAYBOOK.md` before anything is committed.