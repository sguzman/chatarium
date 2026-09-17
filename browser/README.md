# Browser flight recorder

This directory contains the P0 emergency durability layer for the official ChatGPT web client.

`flight-recorder.user.js` is a Tampermonkey-compatible userscript. It does not replace the ChatGPT network stack. Its purpose is to make the existing site less capable of destroying already-authored or already-observed text while the native client and direct protocol adapter are being built.

## What version 0.1 records

- current composer text, with a synchronous `localStorage` write-ahead record;
- send intent before common submit paths;
- conversation/route identity and URL;
- observed user/assistant transcript text from `data-message-author-role` nodes;
- browser online/offline transitions;
- navigation and recorder-start events;
- an IndexedDB archive of drafts, messages, and events.

The synchronous write-ahead record is intentionally tiny. IndexedDB holds the larger archive.

## Export

Press **Ctrl+Shift+Alt+E** while ChatGPT is open. The script downloads a JSON export containing the current write-ahead record plus archived events, drafts, and observed messages.

For diagnostics from DevTools console:

```js
await ChatariumFlightRecorder.status()
await ChatariumFlightRecorder.exportAll()
```

## Important limitations

This is a flight recorder, not yet a network-protocol recorder. DOM selectors can change when ChatGPT changes. Assistant text is observational and may miss content that never reached/rendered in the page. The script does not claim that a DOM transcript is the canonical remote conversation.

Version 0.1 also does not automatically restore old drafts into the composer; preserving evidence comes before mutating the official UI. Recovery UI will be added deliberately.

## Privacy

The local archive can contain conversation text. It remains in browser storage until exported or cleared. Do not commit personal exports to this public repository. Protocol fixtures should use controlled non-sensitive test conversations and the sanitization process in `protocol/CAPTURE_PLAYBOOK.md`.
