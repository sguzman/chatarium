# Sanitization record — 2026-09-29.001

The source HAR must remain private.

Although it was exported through browser DevTools, inspection found values that are not suitable for a public protocol repository.

## Classes found in the raw HAR and omitted from Git

- `x-conduit-token` values, including JWT-shaped values;
- Sentinel prepare/finalize tokens;
- proof-of-work and turnstile material;
- signed websocket `verify` query values;
- account identifiers;
- device identifiers;
- conversation and message UUIDs;
- request/turn tracing identifiers;
- conversation batch payloads containing hidden/system/context material and unrelated private context.

The raw HAR also contains enough application state that treating it as a harmless sanitized artifact would be incorrect.

## What is retained

Committed evidence retains only:

- endpoint paths and HTTP methods;
- selected response statuses and MIME types;
- field/header **names** useful for structural analysis;
- deterministic controlled test text;
- deterministic assistant marker text;
- placeholder identifiers;
- timestamps needed to establish ordering;
- raw HAR SHA-256 and size for provenance.

## Raw-source provenance

```text
sha256: bda0a642ca948788c0e083fbbe567ff9853effd4e00189f4732f5b10b32da166
bytes: 35223925
```

The raw source is not a fixture and must not be added to Git.
