# Postmortems

> Historical browser names and workflows inside incident reports describe what happened at the time. Current browser QA policy is defined by `docs/CODEX_QA_WORKSTATION.md` and uses Playwright-managed bundled Chromium; personal Microsoft Edge is outside the automation boundary.

Postmortems are permanent engineering inputs, not blame-free summaries that disappear after an incident. Each postmortem must record:

- what the product target was;
- what evidence existed at each decision point;
- where implementation or claims exceeded that evidence;
- operator burden created by the failure;
- what remains reusable;
- concrete rules/tests/gates added to prevent recurrence;
- unresolved questions that must remain labeled unresolved.

## Incidents

- [2026-10-03 — ChatGPT history interoperability and Tampermonkey bridge](2026-10-03-chatgpt-history-bridge.md) — incorrect HAR dismissal, repeated false-positive bridge milestones, inadequate observability, request-context omission, excessive human QA, and retirement of Tampermonkey from the critical account-history path.
  - [Claim/evidence ledger](2026-10-03-chatgpt-history-bridge-ledger.md) — timestamped QA sequence, exact integrated heads, claim-vs-proof matrix, operator burden ledger, unresolved facts, and hard stop condition.
- [2026-10-05 — Unsynchronized SIWC capability-probe handoff](2026-10-05-siwc-capability-probe-handoff.md) — remote/local checkout state was conflated, producing a guaranteed missing-file failure; diagnosis then drifted toward auth before recognizing execution had never reached authentication. Corrected by in-app probing, durable sanitized evidence, and hard operator-handoff rules.
- [2026-10-05 — Accidental whole-file truncation during SIWC diagnostics edit](2026-10-05-main-rs-truncation.md) — a miscalled stateful replacement helper reduced `main.rs` to 46 bytes; repaired from a known-good parent and followed by mandatory pre/post-commit file-integrity guards.
