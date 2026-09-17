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
