# Sanitization record — 2026-10-01.001

The raw Flight Recorder export is private evidence and is **not committed**.

Raw-source provenance:

```text
sha256: d204b5c6e26536cef112b1dce9b551613129f17b74f1b4e2235c2491b9081fc6
bytes: 990898
```

The selected v0.7.1 run is bounded by its explicit Arm and Disarm events. Only the single C02 conversation-fetch response in that run is represented in the public fixture.

The sanitization is value-minimizing and deterministic:

- concrete identifiers become deterministic `<id:N>` placeholders while repeated references remain equal;
- numeric values become `<number>`;
- booleans become `<bool>`;
- URLs become `<url>`;
- arbitrary/private strings become typed placeholders such as `<string>` or `<redacted-text>`;
- nulls and object/array structure are preserved;
- query values are not captured;
- request headers, cookies, authorization material, request bodies, and browser storage are not committed;
- the controlled conversation title, user text, assistant text, model identifiers, timestamps, and concrete remote identifiers are not committed.

The derived inventory records structural keys, container counts, and type-level shape without copying response scalar values.

No raw response body or reusable authentication material is committed.
