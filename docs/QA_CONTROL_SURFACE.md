# QA control surface

The browser control surface for Chatarium QA is a dedicated Playwright-managed Chromium persistent context.

```text
Codex / launcher
    |
    v
Playwright bundled Chromium
    |  user-data-dir: ~/.local/share/chatarium-qa-browser/
    |  --disable-extensions-except=browser/edge-bridge
    |  --load-extension=browser/edge-bridge
    v
Chatarium MV3 bridge service worker
```

The historical directory name `browser/edge-bridge` is retained for compatibility. It is a Chromium-compatible MV3 extension source directory, not a request to use Microsoft Edge.

## Observable proof

The shell-level validator must establish all of the following without browser MCP tools:

1. `chromium.executablePath()` resolves to an existing Playwright-managed executable.
2. The persistent context user-data directory is exactly `~/.local/share/chatarium-qa-browser/`.
3. The context is headed.
4. `browser/edge-bridge` is loaded.
5. An MV3 extension service worker is observable.
6. The extension ID and manifest version are read from that worker.
7. A disposable normal page can be opened.
8. No Microsoft Edge process or profile is accessed.

The validator closes its context in a `finally` block. A failure is diagnostic and must not be reinterpreted as successful extension loading.

## Historical evidence

Earlier QA used a Playwright extension connection to a dedicated Edge profile. That configuration is retired, not erased from historical reports. Current setup and future QA use bundled Chromium so the browser executable, profile ownership, extension loading, and lifecycle are controlled by Playwright.
