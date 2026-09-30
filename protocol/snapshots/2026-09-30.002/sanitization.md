# Sanitization record — 2026-09-30.002

The raw Flight Recorder export is private evidence and is not committed.

Raw-source provenance:

sha256: f8f2a1de659dc1a5c7aca636ee1d4a0c66cdb4351fffb530a857fbd63507a0eb
bytes: 864006

The selected v0.7.1 Arm/Disarm run is represented by its run identity and bounded counters. The raw export contains unrelated account, project, settings, and conversation material and remains outside Git.

The committed C02 evidence preserves only:

- HTTP method;
- normalized backend path;
- query-key names;
- status;
- content type;
- captured-byte count;
- truncation/body-presence flags;
- safe top-level JSON type;
- causal selection metadata;
- raw-source SHA-256 and byte count.

The public evidence removes or generalizes:

- concrete conversation identifiers;
- response body text;
- account, organization, user, project, message, and other concrete identifiers;
- query values;
- headers, cookies, authorization material, and request bodies;
- unrelated private response payload structure;
- the concrete server error string.

The smaller regression fixture contains only the three C02 conversation-fetch responses and replaces the private JSON error body with {"detail":"<string>"}.

No credentials or reusable authentication material are committed.
