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
