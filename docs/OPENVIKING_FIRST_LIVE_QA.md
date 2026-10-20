# Chatarium: first live OpenViking acceptance

This is the single remaining **host-specific acceptance check**, not another
implementation phase. GitHub CI exercises the deterministic checkpoint lifecycle
with synthetic backends; it cannot reach an OpenViking server running on another
computer.

## One-command launch (repository root)

```sh
node tools/run-openviking-qa.mjs
```

The launcher checks the fixed `127.0.0.1:1933/mcp` OpenViking endpoint by
listing MCP tools **without writing or reading any personal memory**, then
opens the ordinary local-first Chatarium desktop through its existing Cargo
launcher. If OpenViking is offline or the tool schema differs, the GUI still
opens and reports that discovery is unavailable. It does not ask for account
credentials or print raw MCP responses. Node.js 22+, Cargo/Rust and the
repository checkout are required for this source-build QA path. If a valid
Chatarium executable is already running, close it before using this launcher.

## First practical test

1. Confirm the launcher prints **OpenViking MCP discovery: PASS**. This proves
   only read-only discovery, not safe writing or complete memory integration.
   If it says UNAVAILABLE, investigate the existing local service separately;
   Chatarium itself can still be used offline.
2. Open or create an ordinary **local** Chatarium conversation and use a model
   that can actually complete responses. Have a substantive short exchange
   about a harmless test project with a specific persistent decision and next
   action. No special "save memory" instruction is required. An assistant
   completion must contain at least 160 non-whitespace characters before
   automatic checkpoint scheduling applies.
3. Let the conversation become idle. Background summarization requires active
   inference, may wait up to its 90-second scheduling interval and may return
   `NO_MEMORY` for routine messages. If a durable summary is generated,
   the host performs a synthetic exclusive-create safety check before sending
   exactly one production checkpoint. **This diagnostic creates a disposable
   synthetic QA object on the existing service.** It never overwrites an
   existing personal checkpoint.
4. Look for **OpenViking memory checkpoint saved and verified by read-back**
   in Chatarium. Also inspect the matching checkpoint in the existing
   OpenViking Studio, if available. A failed preflight leaves the item Ready;
   an attempted but unverified create is Uncertain and must **not** be sent
   again manually.
5. After a confirmed checkpoint, restart Chatarium and continue the same local
   conversation with a question about the prior decision. Check whether the
   context composer admits a verified OpenViking checkpoint. Cross-conversation
   project sharing is opt-in and is **not** needed for this first acceptance.

**Pass** requires the automatic normal-turn checkpoint, exact server read-back
confirmation, and subsequent verified recall. A tool-discovery PASS alone is
not sufficient. A missing model, failed sign-in or unavailable local service
is an environment blocker, not evidence that a write succeeded.

During this test, avoid uploading or publicly posting private local journals,
checkpoint summaries, API keys, raw browser profiles, or full logs. A concise
status message or redacted screenshot is sufficient for defect triage.

The existing focused lifecycle specification is
[`OPENVIKING_MEMORY_LIFECYCLE.md`](OPENVIKING_MEMORY_LIFECYCLE.md).
