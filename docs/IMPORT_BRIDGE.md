# Browser flight-recorder import bridge

The import bridge moves evidence from the temporary browser-side survival layer into Chatarium's native durable store without pretending the browser recorder observed more than it actually did.

## Command

```powershell
cargo run -p chatarium-importer -- flight-recorder <export.json> [data-dir]
```

If `data-dir` is omitted, the importer uses the same location policy as Chatarium Desktop:

1. `CHATARIUM_DATA_DIR` when set;
2. `%LOCALAPPDATA%\Chatarium` on Windows;
3. `./.chatarium` as a final fallback.

Do not run the importer against a journal that another Chatarium process is actively mutating during early development. Close Chatarium Desktop first.

## Source preservation

Before semantic import begins, the exact export bytes are copied into:

```text
<data-dir>/imports/flight-recorder/<sha256>.json
```

The archive filename is content-addressed. Existing archives are rehashed before reuse. The source archive is private local data and may contain conversation text.

## Journal compatibility

Journal writer format v2 adds an optional `scope` field so imported evidence from multiple browser conversations does not collapse into one synthetic conversation. Journal v1 records remain readable and are treated as unscoped.

## Semantic mapping

A flight-recorder v3 export is translated conservatively:

- saved draft -> `draft_changed` in the observed conversation scope;
- send intent -> `user_message_committed` + `dispatch_attempted`;
- confirmed send intent -> additionally `remote_acceptance_observed`;
- unresolved/pending send intent -> **no invented acceptance or failure**;
- rendered user transcript message -> `transcript_user_message_observed`;
- rendered assistant text -> `assistant_snapshot_observed`;
- assistant WAL text not already represented in captured messages -> `assistant_snapshot_observed`;
- visible site/client error -> `client_error_observed`;
- import boundaries -> `import_started` and `import_completed`.

A rendered assistant snapshot does not prove that generation completed, so the importer does not emit `assistant_completion_observed` merely because text was present in the DOM.

## Provenance payload

Imported semantic events carry a JSON payload containing:

- source export SHA-256;
- stable import event key;
- source format/version;
- source timestamp when available;
- source conversation and URL when available;
- exact text when applicable;
- observation-specific details such as message ID, send state, or content hash.

The journal event's own timestamp remains the local import/append time. Source observation time is preserved inside the provenance payload instead of being rewritten as if the native client observed it live.

## Idempotency and crash recovery

Each derived event has a stable key within the source export fingerprint. Before appending, the importer scans already-durable journal payloads for that `(source sha256, event key)` pair.

Therefore rerunning the same export:

- does not duplicate already imported semantic events;
- can continue after a crash midway through import;
- keeps `import_completed` last in the deterministic plan.

Every journal append still crosses the normal Chatarium durability boundary (`write` -> `flush` -> `sync_data`) before the importer advances.

## Privacy

Flight-recorder exports are **not** protocol fixtures and should not be committed to the public repository. They can include private drafts, messages, assistant text, URLs, and site errors. The import bridge is a local recovery path, not a sanitizer.
