# Protocol recorder CLI

`chatarium-recorder` is the offline ingestion boundary between browser captures and Chatarium's public protocol corpus.

It is deliberately **offline-first**: give it a HAR or Chatarium Flight Recorder export you produced locally and it creates sanitized, fingerprinted evidence plus structural derived data. It does not log into ChatGPT, obtain credentials, or bypass browser/session controls.

The package also exposes a reusable Rust library (`chatarium_recorder`) for schema-
agnostic JSON value and HAR sanitization, structural inventory generation, SHA-256
fingerprints, and snapshot writing. The CLI calls these same operations so its
behavior is covered alongside the library API.

```rust
let sanitized = chatarium_recorder::sanitize_har_bytes(har_bytes)?;
let digest = chatarium_recorder::sha256_hex(&sanitized);
```

The capture-harness specification is in [`docs/CAPTURE_HARNESS.md`](../../docs/CAPTURE_HARNESS.md).
Portable bundle schema-specific validation is intentionally deferred until the
harness emitter schema exists. The repository does not yet have a portable bundle
producer, a versioned emitted manifest schema, or a canonical sanitized bundle
fixture, so the recorder does not guess a format. A future harness can call the
schema-agnostic `sanitize_value` primitive to apply the shared redaction rules to
`serde_json::Value` data.

## Commands

```text
chatarium-recorder sanitize-har <input.har> <output.har>
chatarium-recorder inventory-har <input.har> <output.json>
chatarium-recorder snapshot-har <input.har> <snapshot-dir> <capture-id>
chatarium-recorder snapshot-flight <input.json> <experiment.toml> <snapshot-dir> <capture-id>
chatarium-recorder inspect-har <input.har>
chatarium-recorder fingerprint <file>

chatarium-inventory-diff <before.requests.json> <after.requests.json> <output.diff.json>
```

The package keeps `chatarium-recorder` as its default Cargo run target, so existing `cargo run -p chatarium-recorder -- ...` commands remain unambiguous even though the companion diff binary also exists.

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

### `snapshot-flight`

Ingests a private Chatarium Flight Recorder export and a versioned experiment definition.

Unlike the schema-agnostic JSON sanitizer, this command understands that Flight Recorder exports can contain sensitive material **inside SSE text blobs**. It therefore never copies raw `network-stream-chunk.payload.text` into the public artifact.

The command:

- selects the latest run beginning at the latest `recorder-started` event, so cumulative browser storage does not mix old QA runs into the new snapshot;
- reconstructs SSE frames incrementally across browser chunk boundaries using `crates/protocol`;
- preserves only the experiment's exact canonical request/expected text as literal message content;
- replaces concrete conversation/message/send identities with stable per-artifact placeholders;
- redacts signed token values, unknown scalar metadata, arbitrary message bodies, generated titles, and private hidden context;
- fails closed for an SSE frame it cannot parse instead of copying the raw frame text;
- writes a structural inventory with event-kind counts, SSE event/control types, delta operations/paths, marker counts, encodings, and completion signals;
- records the raw source SHA-256 and byte count for provenance without copying the raw file or source path into the snapshot.

Output:

```text
<snapshot-dir>/
├── evidence/
│   └── <capture-id>.flight.json
└── derived/
    ├── <capture-id>.flight.inventory.json
    └── <capture-id>.flight.meta.json
```

Example:

```text
cargo run -p chatarium-recorder -- snapshot-flight \
  ~/Downloads/chatarium-flight-recorder-....json \
  protocol/experiments/C03-send-text.toml \
  protocol/snapshots/2026-09-29.003 \
  C03-send-text
```

The raw recorder export remains private. The derived output is designed to be dramatically safer and less noisy, but it is still evidence that should be reviewed before publication; the tool does not claim to be a universal privacy oracle.

### `inspect-har`

Sanitizes the HAR in memory and prints the same normalized structural endpoint view used by the request inventory: method, status, host/path, and MIME type. Query values are not printed and obvious instance-ID path segments appear as `<id>`.

This is useful for quickly identifying which requests belong to a controlled experiment before deeper documentation.

### `chatarium-inventory-diff`

Compares two generated request inventories structurally. Endpoint identity is the tuple `(method, host, normalized path)`. For each endpoint, the diff compares the multiset of observed structural variants, including status, MIME types, body presence, resource type, query-key names, and request/response-header names.

That means all of these become explicit maintenance signals:

- a new or removed endpoint;
- the same endpoint returning a new status/MIME shape;
- request/header/query shape changes;
- repeated occurrence-count changes for an otherwise identical shape.

The report deliberately contains no request/header/query values because it consumes the already value-free inventory layer.

Run it through Cargo as:

```powershell
cargo run -p chatarium-recorder --bin chatarium-inventory-diff -- `
  .\protocol\snapshots\2026-09-17.001\derived\C03-send-text.requests.json `
  .\protocol\snapshots\2026-09-24.001\derived\C03-send-text.requests.json `
  .\protocol\diffs\2026-09-17.001--2026-09-24.001.C03.json
```

The command prints a compact summary (`added`, `removed`, `changed`, `unchanged`) and writes the full machine-readable report. A structural diff is a signal to inspect the corresponding sanitized HAR evidence; it is not by itself proof of a semantic protocol change.

## Sanitization is not a publication oracle

The sanitizer is defense-in-depth. It cannot prove that arbitrary conversation bodies are non-sensitive, because protocol payloads legitimately contain user-authored and assistant-authored text. The canonical workflow is therefore:

1. run experiments using controlled synthetic text;
2. export HAR with response content;
3. run `snapshot-har`;
4. inspect the sanitized capture manually;
5. inspect the derived request inventory for unexpected identifiers or structure;
6. diff against the previous equivalent capture when one exists;
7. only then commit the snapshot to `protocol/snapshots/...`.

Never use a personal conversation as a public fixture just because automated redaction passed.
