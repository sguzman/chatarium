# Protocol recorder CLI

`chatarium-recorder` is the offline ingestion boundary between browser captures and Chatarium's public protocol corpus.

It is deliberately **offline-first**: give it a HAR you exported yourself and it produces sanitized, fingerprinted evidence plus structural derived data. It does not log into ChatGPT, obtain credentials, or bypass browser/session controls.

## Commands

```text
chatarium-recorder sanitize-har <input.har> <output.har>
chatarium-recorder inventory-har <input.har> <output.json>
chatarium-recorder snapshot-har <input.har> <snapshot-dir> <capture-id>
chatarium-recorder inspect-har <input.har>
chatarium-recorder fingerprint <file>
```

### `sanitize-har`

Parses HAR JSON and replaces common credential-bearing values with `<redacted>`, including authorization/cookie headers, token-like query parameters, cookie values, common CSRF/session/device identifiers, URL credentials, and sensitive keys found inside JSON request/response bodies.

The output is pretty-printed JSON and its SHA-256 fingerprint is printed to stdout.

### `inventory-har`

Sanitizes the input in memory and writes a value-free structural request inventory. Each HAR entry records:

- index, method, host, normalized path, and response status;
- request/response MIME types and whether a request body exists;
- browser resource type when present in the HAR;
- query-parameter names without values;
- request- and response-header names without values.

Obvious UUIDs and long numeric/high-entropy identifier-like URL path segments are normalized to `<id>` in the derived inventory. This makes protocol diffs less noisy and reduces propagation of instance-specific identifiers. The sanitized HAR remains the stronger source evidence; the inventory is a derived comparison surface.

Example:

```powershell
cargo run -p chatarium-recorder -- inventory-har `
  .\captures\C03-send-text.raw.har `
  .\captures\C03-send-text.requests.json
```

### `snapshot-har`

Creates evidence in the same shape used by `protocol/SNAPSHOT_FORMAT.md`:

```text
<snapshot-dir>/
├── evidence/
│   └── <capture-id>.har.json
└── derived/
    ├── <capture-id>.meta.json
    └── <capture-id>.requests.json
```

The request inventory is derived from the sanitized HAR, not directly from raw input. The metadata records both fingerprints, entry count, sanitized size, recorder version, capture timestamp, and the fact that raw evidence is retained outside Git. It intentionally does **not** copy the raw HAR filename or raw byte size into the public snapshot metadata.

Example:

```powershell
cargo run -p chatarium-recorder -- snapshot-har `
  .\captures\C03-send-text.raw.har `
  .\protocol\snapshots\2026-09-17.001 `
  C03-send-text
```

### `inspect-har`

Sanitizes the HAR in memory and prints the same normalized structural endpoint view used by the request inventory: method, status, host/path, and MIME type. Query values are not printed and obvious instance-ID path segments appear as `<id>`.

This is useful for quickly identifying which requests belong to a controlled experiment before deeper documentation.

## Sanitization is not a publication oracle

The sanitizer is defense-in-depth. It cannot prove that arbitrary conversation bodies are non-sensitive, because protocol payloads legitimately contain user-authored and assistant-authored text. The canonical workflow is therefore:

1. run experiments using controlled synthetic text;
2. export HAR with response content;
3. run `snapshot-har`;
4. inspect the sanitized capture manually;
5. inspect the derived request inventory for unexpected identifiers or structure;
6. only then commit the snapshot to `protocol/snapshots/...`.

Never use a personal conversation as a public fixture just because automated redaction passed.