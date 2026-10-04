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

Post-restart proof still required:

1. Inspect the fresh active MCP configuration and confirm it launches bundled Chromium with the dedicated profile.
2. Use Playwright MCP to enumerate only the dedicated QA browser tabs.
3. Open and close one disposable QA page and verify DOM, accessibility, console, and screenshot operations.
4. Confirm no personal Edge profile or window was touched.

The prior Edge extension-mode MCP session must not be treated as proof for this new architecture.
