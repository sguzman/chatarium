# Sanitization record — 2026-10-03.001

The raw Edge HAR is private evidence and is **not committed**.

Raw-source provenance:

```text
sha256: 2fef410f7b38b1b818eb3a978d0d3c1a189cdafa0b6fd39cef4bf0c05df2cbc0
bytes: 23519530
```

This snapshot was manually reduced instead of publishing a mechanically "sanitized" full HAR. The raw capture contains unrelated private conversation text, account/settings payloads, concrete remote identifiers, request headers, and ephemeral anti-abuse/security material.

Committed evidence retains only publication-safe structural facts:

- concrete conversation/message/request/account identifiers are removed or replaced with typed placeholders;
- all conversation title/user/assistant/tool/reasoning text is removed;
- cookies, authentication/session material, device/account identifiers, and request-header values are not copied;
- Sentinel/challenge/proof/turnstile/conduit values are not copied;
- account and settings response bodies are not copied;
- the only literal query values retained are the two pre-approved C02 values allowed by the repository's safe grammar: `num_turns=10` and `include_has_versions=true`;
- response counts, key sets, content-shape variants, HTTP statuses, MIME types, and boolean pagination flags are retained as structural evidence.

The small regression fixture is a structural reduction, not a full transcript copy. It preserves representative parser-relevant message shapes using placeholders only.
