# QA browser migration resume state

State written for the post-restart continuation of the bundled-Chromium migration.

Completed in the pre-restart Codex process:

- inspected `AGENTS.md`, existing browser bridge documentation, Node/pnpm tooling, and the active MCP configuration;
- added `tools/qa-browser/` with a pinned Playwright dependency, persistent-context config, headed launcher, and shell validator;
- added `tools/qa-browser/mcp-config.json` for Chromium plus the current `browser/edge-bridge` extension;
- added `docs/CODEX_QA_WORKSTATION.md` and `docs/QA_CONTROL_SURFACE.md`;
- installed/validated Playwright-managed Chromium and confirmed the extension service worker, ID, and version with the shell validator;
- rewrote the active Codex Playwright MCP entry to use the repository-owned Chromium config;
- committed the repository changes in the migration commit; pushing that commit directly to `main` is the final pre-restart repository operation.

Validation notes:

- `pnpm --dir tools/qa-browser run validate` passed with headed Chromium 153.0.8010.12, Playwright revision 1243, persistent profile, and bridge service worker version 0.4.7;
- the launcher smoke test passed and its controlled QA Chromium process was shut down;
- bridge package checks, JavaScript syntax checks, MCP JSON parsing, and targeted `cargo check -p chatarium-core -p chatarium-protocol -p chatarium-store` passed;
- full `cargo check` remains blocked by pre-existing `tools/capture` errors involving `EDGE_PIPE_NAME` and `EdgeBrowserTransport::Pipe`, unrelated to this migration.

Post-restart control-plane proof completed on 2026-10-04:

1. Playwright MCP launched bundled Chromium 155.0.8059.12 with the dedicated profile.
2. The QA window was `chatarium-qa` and Hyprland routed it automatically to workspace 6.
3. Playwright verified navigation, DOM, accessibility, console, and screenshot control.
4. The unpacked MV3 bridge loaded from `browser/edge-bridge/` and reported version 0.4.10.
5. Personal Microsoft Edge was not controlled or used as the QA target.

Live history-discovery acceptance completed on 2026-10-05 after the dedicated profile was
authenticated. The production Rust QA entry point observed 85 conversation summaries from
first-party traffic, persisted a structural catalog, and emitted no private bodies or titles.
The empty-cache run also exercised the 0.4.10 fresh-tab recovery path; a reused stale service
worker was cleared by restarting only the Playwright QA Chromium, after which the exact
fresh-tab command completed with debugger-before-navigation, Network, first-party navigation,
observation, detach, and temporary-tab cleanup proof. A second bounded run retained the 85-item
catalog and correctly suppressed fresh-tab recovery.

## Exact single-conversation mirror acceptance (2026-10-05)

One bounded production capture for the deterministic catalog item succeeded through the
first-party navigation path: the exact response was observed with HTTP 200, valid JSON,
validated remote identity, and no rate limiting. The response was promoted once into the
append-only journal at snapshot sequence 92; the temporary tab and debugger were cleaned up.
No raw response body, title, message text, or reusable account material was added to Git.

The persisted C02 graph contained six messages. The active `current_node` was a visible
assistant text node. Its parent chain passed through a `reasoning_recap` node whose parent was
absent even though both pagination flags were false. The internal node was encoded as a
`content` object rather than the `Thoughts` variant. The projector now treats the observed
`reasoning_recap` and `model_editable_context` content types as non-visible internal scaffolding:
it projects the visible suffix and marks the result partial, while still rejecting a missing
parent on a visible node, cycles, identity mismatches, and unsupported completeness claims.

Journal reopen/replay was then repeated without remote HTTP and reproduced one projected
visible message with `truncated_before=true`, accurately recording a partial mirror rather than
claiming a complete transcript. The regression test uses synthetic IDs and redacted structure
only.

## Resumable catalog mirroring acceptance (2026-10-05)

The mirror scheduler now derives its state from append-only queue lifecycle events plus the
existing durable snapshot audit. It distinguishes discovered, queued, capturing, fully mirrored,
partially mirrored, rate-limited, transient-failure, and structural-failure states. Successful
full and partial snapshots are excluded from later work selection; a transient failure remains
eligible for a later explicit bounded run; a rate-limited item is not selected automatically.
The queue reports catalog indexes and structural counts only.

Local queue tests cover empty/all-pending derivation, successful full/partial state, restart
replay, idempotent derivation, transient failure resumption, rate-limit exclusion, and
structural-failure isolation. `mirror-status` performs an offline journal/catalog reconstruction
and does not start browser automation.

One serial live acceptance run was capped at three items. It selected catalog indexes 0, 1, and
2; index 0 was promoted and replayed as a partial mirror, while indexes 1 and 2 recorded
transient authentication failures. No HTTP 429 occurred and no retry was launched. The journal
was reopened before reporting, the existing partial mirror remained partial, and the resulting
state was 85 observed, 2 partial, 83 pending, with no full or rate-limited items. This is an
observed catalog, not an account-wide completeness claim.
