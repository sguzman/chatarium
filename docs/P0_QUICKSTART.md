# P0 quickstart — make the current site survivable

This is the shortest path from an unreliable `chatgpt.com` tab to having a local recovery record **today**.

The browser flight recorder is temporary infrastructure while Chatarium's native client and direct protocol adapter are being built. It does not make ChatGPT's network reliable. It makes a large class of frontend/network failures less destructive by preserving local evidence before the page can lose it.

## 1. Install the flight recorder

Raw userscript:

`https://raw.githubusercontent.com/sguzman/chatarium/main/browser/flight-recorder.user.js`

With Tampermonkey installed in Edge/Chrome:

1. Open Tampermonkey and create a new userscript.
2. Replace the generated template with the complete contents of the raw userscript above.
3. Save it.
4. Reload `https://chatgpt.com/`.
5. Confirm a small **Chatarium** panel appears in the lower-right corner and reports `v0.3.0` or newer.

If the panel does not appear, do not assume protection is active. Check Tampermonkey first.

## 2. Verify draft durability without risking real work

In a disposable/new ChatGPT conversation:

1. Type exactly `CHATARIUM_DRAFT_TEST_001` into the composer.
2. Do **not** send it.
3. Wait a fraction of a second.
4. The Chatarium panel should report `draft saved`.
5. Click **Copy draft** and paste into a harmless local editor. It should reproduce the exact marker.

You can also inspect state from DevTools Console:

```js
await ChatariumFlightRecorder.status()
```

The current conversation's `draftWal.text` should contain the marker.

After verifying it, clear the test text normally.

## 3. Verify send-intent durability

Still in a disposable conversation, send exactly:

```text
respond with exactly CHATARIUM_SEND_TEST_001
```

The recorder synchronously journals the outgoing text before the site's normal send handling gets a chance to clear the composer.

While the turn is active, the panel may temporarily report an unresolved send. Once the user's message is observed in the rendered conversation, that send intent should become confirmed.

Inspect if desired:

```js
ChatariumFlightRecorder.readSendIntents()
```

The latest record retains the exact outgoing text. **Copy last send** is the emergency recovery path even if ChatGPT's surface becomes confused afterward.

An unresolved send is not automatically a failed send. It means Chatarium has local evidence that you attempted the turn but does not yet have enough evidence to classify the remote outcome.

## 4. Verify assistant recovery

Let the deterministic response render. The panel should report that an assistant snapshot has been saved.

Click **Copy assistant** and paste into a local editor. It should contain the rendered assistant text. You can inspect the synchronous emergency slot directly:

```js
ChatariumFlightRecorder.readAssistantWal()
```

The assistant WAL is separate from both the draft WAL and send-intent journal. If the page later clears/re-renders the composer or displays a transient timeout, already-rendered assistant text remains recoverable from its own slot.

The emergency assistant slot keeps up to the newest 500,000 characters. The IndexedDB transcript archive remains the longer-term observed-message store.

## 5. Observe site failures instead of losing them

If ChatGPT displays a visible `role="alert"` or toast error, the recorder preserves the newest observed error separately:

```js
ChatariumFlightRecorder.readErrorWal()
```

The panel also shows the latest observed site error. This is evidence about what the frontend displayed, not a claim about whether the remote mutation succeeded or failed.

## 6. Export before doing anything destructive

At any time press:

**Ctrl+Shift+Alt+E**

or click **Export** in the Chatarium panel.

This downloads a local JSON record containing the current draft WAL, bounded send-intent journal, latest assistant snapshot, latest visible-error record, event history, archived draft records, and observed transcript messages.

The export contains conversation text. Treat it as private data.

## What P0 protects right now

- exact composer text after real input events;
- per-conversation draft write-ahead records;
- outgoing send intent in a separate synchronous journal that an empty post-send composer cannot erase;
- new-chat sends across the `/` → `/c/<id>` route transition;
- latest rendered assistant text in a separate synchronous emergency slot;
- longer-lived rendered user/assistant transcript snapshots in IndexedDB;
- visible alert/toast text such as delivery-timeout errors;
- browser online/offline and navigation events;
- explicit unresolved-send state instead of pretending an ambiguous timeout is success or failure.

It deliberately does **not** auto-inject recovered text into ChatGPT's React-controlled composer yet. Copying recovery text is safe; mutating the site's editor automatically requires a tested site adapter.

---

# First protocol evidence

Once P0 is active, the next bottleneck is no longer scaffolding: Chatarium needs controlled observations of the real consumer web protocol.

Do these in disposable conversations only. Do not capture a private conversation for protocol documentation.

## C00 — idle page load

1. Open Edge DevTools → **Network**.
2. Enable **Preserve log**.
3. Clear the network log.
4. Reload `https://chatgpt.com/`.
5. Do nothing until startup traffic settles.
6. Export/save the network log as a HAR **with response content**.
7. Name it something recognizable such as `C00-idle-page-load.raw.har`.

This separates startup/session/config traffic from actual chat actions.

## C03 — one complete deterministic turn

Use a separate clean capture:

1. Open a disposable new chat.
2. Open DevTools → Network and enable **Preserve log**.
3. Clear the network log.
4. Send exactly:

   ```text
   respond with exactly TEST123
   ```

5. Do not click anything else until the response completes.
6. Export/save the network log as a HAR **with response content**.
7. Name it `C03-send-text.raw.har`.

Do not manually redact the HAR before giving it to Chatarium tooling; keep the raw file private and let the offline sanitizer produce a derived copy. Raw evidence should never be committed to this public repository.

## Local ingestion

After cloning the repo locally, the intended ingestion path is:

```powershell
cargo run -p chatarium-recorder -- inspect-har .\captures\C03-send-text.raw.har

cargo run -p chatarium-recorder -- snapshot-har `
  .\captures\C03-send-text.raw.har `
  .\protocol\snapshots\2026-09-17.001 `
  C03-send-text
```

`inspect-har` prints a normalized value-free request view. `snapshot-har` creates sanitized evidence, structural request inventory, and derived metadata. The sanitized result still requires human inspection before it is committed; sanitization is defense-in-depth, not proof that arbitrary conversation content is publishable.

For the full experiment matrix, see [`../protocol/CAPTURE_PLAYBOOK.md`](../protocol/CAPTURE_PLAYBOOK.md).
