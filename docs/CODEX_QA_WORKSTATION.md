# Codex QA workstation

Chatarium's Codex browser QA target is Playwright-managed bundled Chromium. It is intentionally separate from the principal's Microsoft Edge profile and from the retired Playwright extension-mode setup.

## Canonical locations

- Persistent QA profile: `~/.local/share/chatarium-qa-browser/`
- Human/Codex launcher: `~/.local/bin/chatarium-qa-browser`
- Browser tooling: `tools/qa-browser/`
- Chromium extension source: `browser/edge-bridge/`
- MCP JSON config: `tools/qa-browser/mcp-config.json`

The QA profile is created empty by Playwright. It must never be seeded from Edge, Chrome, or another browser profile, and it must never be committed.

## Setup and validation

From the repository root:

```sh
pnpm --dir tools/qa-browser install
pnpm --dir tools/qa-browser exec playwright install chromium
pnpm --dir tools/qa-browser run validate
```

`validate.mjs` launches a headed persistent context, reports Playwright's resolved executable and browser version, opens a disposable `example.com` page, observes the extension MV3 service worker, reads its manifest version/ID, and closes the context. It does not inspect or launch Microsoft Edge.

The launcher opens the same headed persistent context:

```sh
chatarium-qa-browser
```

It uses an OS lock to reject a competing launcher for the same profile and prints a diagnostic when the Playwright browser or extension path is unavailable.

## MCP configuration

The active Codex MCP entry must use:

```text
@playwright/mcp
--config=/home/sguzman/Code/Source/Rust/chatarium/tools/qa-browser/mcp-config.json
```

The JSON selects `browserName: "chromium"`, the persistent QA profile, headed mode, and the current repository extension. It must not contain `--browser=msedge`, `--extension`, `--profile-dir-name`, or `PLAYWRIGHT_MCP_EXTENSION_TOKEN`.

The executable path is deliberately not hard-coded: the installed Playwright package resolves its own managed Chromium revision. If this repository moves, regenerate the absolute extension paths in the local MCP JSON and launcher setup rather than pointing at a browser profile from another product.

## Safety boundary

This setup never reads `~/.config/microsoft-edge/`, attaches to Edge, or uses CDP against Edge. Login/MFA/account consent may occur in the dedicated Chromium profile only. The launcher is not a substitute for automated regression validation.
