# Protocol Observatory

`protocol/` records the ChatGPT web client's behavior as observed at particular times. It is an empirical corpus, not a declaration of a supported public API.

## Evidence hierarchy

From strongest to weakest:

1. sanitized request/response/event capture tied to an explicit action;
2. frontend asset/code evidence tied to the same deployment;
3. repeated observation across independent sessions;
4. derived schema or implementation behavior;
5. hypothesis.

Documentation must distinguish observation from inference. Unknowns remain unknown.

## Layout

- `snapshots/<revision>/` — immutable timestamped observation sets;
- `flows/` — cross-snapshot descriptions of user action -> observed behavior;
- `endpoints/` — endpoint semantics accumulated across revisions;
- `schemas/` — derived structural schemas;
- `fixtures/` — sanitized examples suitable for tests;
- `CHANGELOG.md` — human-readable protocol revision history.

A snapshot should contain a manifest, action notes, sanitization record, frontend asset identity, and the smallest useful set of sanitized evidence.

## Revision IDs

Use `YYYY-MM-DD.NNN`, for example `2026-09-17.001`. The date is the local observation date; the suffix distinguishes independent observation sets.

A revision ID does **not** claim that OpenAI deployed exactly once at that instant. It identifies our evidence set.

## Immutability

Published snapshots should be treated as immutable historical evidence. If a secret or private payload is discovered later, redact it immediately and record that the snapshot was redacted. Do not silently rewrite substantive observations to match later knowledge.

## Secrets and private content

Never commit reusable authentication or private conversation material merely because it appeared in a HAR.

At minimum sanitize:

- cookies;
- `Authorization` and equivalent credentials;
- anti-CSRF/session tokens;
- signed URLs where possession grants access;
- account identifiers not necessary to understand shape;
- private conversation bodies unrelated to the controlled test;
- uploaded private files.

Prefer deterministic placeholders such as `<REDACTED_SESSION_TOKEN>` so structural comparisons remain useful.

## Controlled observation

One capture should answer one question whenever practical. A giant browsing session full of unrelated actions makes causal attribution much harder.

The baseline experiment set is defined in [`CAPTURE_PLAYBOOK.md`](CAPTURE_PLAYBOOK.md).

## Implementation relationship

`crates/protocol` may encode interpretations derived from this corpus. It must identify the newest observation revision against which it was validated. The Rust implementation is never retroactive evidence that the remote service behaved a certain way.

## C02 semantic parsing

Snapshot 2026-10-01.001 is the validated baseline for the ConversationFetch flow. The crates/protocol::conversation_fetch module now parses the observed successful envelope only when the caller supplies that exact observation revision.

The parser establishes a minimal semantic envelope:

- exact opaque remote conversation identity;
- observed title and numeric create/update timestamps;
- ordered message records;
- author role/name/metadata;
- the two observed content shapes, content_type + parts and content_type + content;
- message status/end-turn/weight/metadata/recipient/channel;
- current-node identity;
- page-info cursors and pagination flags.

Unmodeled top-level fields are intentionally ignored. Message content and metadata remain structural JSON at this stage; the parser does not infer transcript semantics, convert timestamps, or write remote content into the durable journal.

The parser also supports exact identity correlation: a caller may provide the remote conversation identity it requested or has durably bound, and the response is rejected if the returned conversation_id differs.

The committed sanitized C02 fixture is used as the parser regression source. Tests materialize its typed placeholders into deterministic non-private values at test time, so the repository exercises the observed structure without committing real conversation content.

The successful 2026-10-01.001 fixture was captured with Flight Recorder v0.7.1 and therefore establishes only the query-key names `include_has_versions` and `num_turns`; their values remain unknown. Flight Recorder v0.7.2 introduces a narrowly approved query-evidence channel for a future C02 run. Only those two keys on an exact conversation-resource GET/HEAD may retain occurrence-ordered literals, and only empty/lowercase-boolean/bounded-decimal forms are publishable. Unsupported shapes become explicit redacted markers. The corpus validator, fixture importer, typed read model, and durable V3 audit all enforce the same boundary so no later implementation can retroactively invent values for the v0.7.1 observation.


## Corpus validation

The committed corpus is validated in CI with:

```text
cargo run -p chatarium-recorder -- validate-corpus protocol
```

The validator checks snapshot identity and required notes, fixture-to-snapshot provenance, obvious credential-bearing object values, and executable canonical C03 SSE semantics. The C03 fixture is replayed through the same typed parser used by `crates/protocol`, keeping evidence and interpretation coupled by tests without treating the implementation as evidence.
