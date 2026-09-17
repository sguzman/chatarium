# Snapshot format

Each protocol observation lives under `protocol/snapshots/<revision>/`.

Minimum files:

```text
<revision>/
├── manifest.toml
├── observations.md
├── sanitization.md
└── evidence/
```

Recommended additions:

```text
├── actions.md
├── frontend-assets.txt
├── requests/
├── responses/
├── events/
└── derived/
```

For HAR-backed captures produced by `chatarium-recorder snapshot-har`, the mechanically generated portion currently looks like:

```text
<revision>/
├── evidence/
│   └── <capture-id>.har.json
└── derived/
    ├── <capture-id>.meta.json
    └── <capture-id>.requests.json
```

`evidence/*.har.json` is the sanitized source evidence. `derived/*.requests.json` is a value-free structural comparison surface generated from that sanitized evidence. Derived output must never silently replace the evidence it came from.

## `manifest.toml`

Example:

```toml
[snapshot]
id = "2026-09-17.001"
observed_at = "2026-09-17T11:00:00-06:00"
observer = "manual-devtools"
status = "sanitized"

[client]
product = "chatgpt-web"
browser = "Microsoft Edge"
platform = "Windows 11"
page_url = "https://chatgpt.com/"

[frontend]
html_sha256 = "<optional>"
asset_manifest_sha256 = "<optional>"

[capture]
format = "har"
raw_retained_outside_git = true
sanitized_fixture = "evidence/send-text.har.json"
request_inventory = "derived/send-text.requests.json"

[scope]
actions = ["new-chat", "send-text", "stream-complete"]

[compatibility]
previous = "2026-09-17.000"
```

Unknown values should be omitted or marked unknown rather than fabricated.

## `actions.md`

Record the human-visible sequence precisely enough to reproduce the experiment:

```text
1. Open a new ChatGPT conversation.
2. Wait until the composer is idle.
3. Enter exactly: respond with exactly TEST123
4. Submit once.
5. Do not interact until output completes.
```

Include timestamps when they help correlate requests.

## `observations.md`

Separate facts from interpretations.

Suggested headings:

- Observed requests
- Observed response transport
- Observed event order
- Stable-looking fields
- Ephemeral-looking fields
- Error/recovery behavior
- Open questions
- Hypotheses requiring another experiment

Use language such as `observed`, `appears`, and `unknown` intentionally.

## `sanitization.md`

Record what classes of data were removed and how. Example:

```text
- Cookie headers removed entirely.
- Authorization-like values replaced with <REDACTED_AUTH>.
- Account identifiers replaced with stable local placeholders.
- Controlled prompt body retained because it contains no private content.
- Signed upload URLs removed.
```

The point is to preserve structural usefulness without creating a credential archive.

## Derived request inventories

`derived/<capture-id>.requests.json` is generated from the sanitized HAR and intentionally omits request/header/query values. It records structural fields such as method, host, normalized path, status, MIME types, header names, query names, resource type, and whether a request body exists.

Obvious UUIDs and long identifier-like URL path segments are normalized to `<id>` in this derived layer. That normalization is an interpretation for comparison purposes; the corresponding sanitized HAR remains the evidence for the exact observed path.

A diff in derived inventory is a signal to inspect the underlying evidence, not proof by itself that endpoint semantics changed.

## Evidence files

Prefer original wire representation after sanitization. Derived normalized JSON is useful but should not silently replace the source representation because ordering, framing, duplicate headers, or streaming delimiters may matter later.

## Snapshot amendments

A snapshot can receive a metadata-only amendment if needed, but substantive new evidence should normally become a new revision. If a security/privacy redaction modifies an old snapshot, document the redaction in both the snapshot and `protocol/CHANGELOG.md`.