# Sanitization record — 2026-09-30.001

The raw Flight Recorder export is private evidence and is **not committed**.

Raw-source provenance:

```text
sha256: ecfb1f1228321e8319de4aeab819e4b0f2bb1422377a96af56143ad2aaf42900
bytes: 786960
```

The selected legacy v0.7.0 run is bounded from Arm sequence 67 through Disarm sequence 80. A response that completed afterward at sequence 81 is excluded.

The committed C01 fixture is intentionally minimal. It preserves only the observed read method, normalized endpoint path, query parameter names, HTTP status/content type, and enough placeholder-only JSON structure to demonstrate top-level pagination plus nested conversation-summary pagination.

The public evidence removes or generalizes:

- conversation titles and message/content text;
- account, organization, user, conversation, message, gizmo, and other concrete identifiers;
- cursor values;
- timestamps and numeric values;
- account/settings payload values;
- pin payload values not required for the C01 semantic claim;
- all unrelated server-controlled scalar strings.

Flight Recorder read mode did not inspect or persist cookies, authorization values, request headers, request bodies, browser storage, or query parameter values.

The snapshot's `evidence/C01.read-observation.json` preserves all 12 selected response metadata records plus value-free structural measurements for the four sidebar responses. `protocol/fixtures/2026-09-30.001/c01-sidebar-read.json` is a smaller representative regression fixture derived from that evidence.

The raw export must remain outside Git.
