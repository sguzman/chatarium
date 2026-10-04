# QA control-surface map

This document answers one question before Chatarium asks Codex to perform live QA:

> Can Codex actually control every moving part needed for this test without turning the principal into the integration harness?

## Principal contract

The principal's routine QA actions are exactly **zero**.

The principal may submit the Codex goal prompt, read the final result, and satisfy a genuine account-holder identity/consent gate such as initial login, MFA, CAPTCHA, or an account-security confirmation.

The principal does **not** run Git commands, build, launch Chatarium, reload extensions, select browser profiles, inspect DevTools, watch terminal output, copy logs, take screenshots, retry flows, or decide whether a regression "looks right."

## 2026-10-04 Edge isolation correction

The first browser-control setup used a UI-created Microsoft Edge QA profile under the same Edge user-data root as the principal's normal browsing profiles.

That setup did prove a number of useful facts:

- Playwright MCP itself was available;
- after adding `--browser=msedge`, Playwright could control a disposable tab;
- DOM/accessibility, console, screenshot, and navigation operations worked;
- no personal tabs were observed by the control proof.

But the setup did **not** prove a hard isolation boundary. During the Edge-pinned run, the principal's personal Edge window displayed the browser debugging indicator even though the Playwright Extension was installed only in the QA profile.

That invalidated the assumption that "separate Edge profile" meant "separate automation boundary."

The old Edge QA profile and Playwright browser-extension token are therefore **retired from the canonical Chatarium QA architecture**.

## Canonical QA browser

Moving forward, Chatarium QA uses:

```text
Playwright-managed bundled Chromium
+
persistent Chatarium-owned user-data directory
~/.local/share/chatarium-qa-browser/
+
Chatarium MV3 bridge loaded from browser/edge-bridge/
```

The principal's normal Edge root:

```text
~/.config/microsoft-edge/
```

is outside the browser-automation system.

Do not attach Playwright MCP, CDP, WebDriver, or desktop browser automation to personal Edge for ordinary Chatarium QA.

Do not install or depend on the Playwright browser extension in personal Edge.

## Why bundled Chromium

Bundled Chromium is preferred over Microsoft Edge, Google Chrome, or an Arch-installed Chromium package for this QA role because:

- Playwright manages the matching browser dependency;
- the QA binary/runtime becomes ordinary project-tooling state;
- Playwright supports a dedicated persistent profile through `--user-data-dir`;
- unpacked MV3 extension testing can be launched deterministically with Chromium;
- branded Chrome/Edge command-line extension side-loading is restricted;
- the QA browser can be headed for normal interactive login while still being fully agent-controlled;
- personal Edge is removed from the architecture entirely.

The repository component remains named `edge-bridge` for historical/source continuity; it is a Chromium-compatible MV3 extension.

## Current control matrix

| Surface | Needed? | Codex control mechanism | Current policy/status |
| --- | --- | --- | --- |
| Git pull/status/diff/commit/push | yes | shell | Codex owns it |
| Cargo/pnpm/build/tests | yes | shell | Codex owns it |
| Start/kill/restart Chatarium | yes | shell/process harness | Codex owns it |
| Capture Chatarium stdout/stderr | yes | shell redirection/process harness | Codex owns it |
| Inspect journal/cache files | yes | shell | Codex owns it |
| Native egui clicking | usually no | CLI/test RPC first | Do not make principal click |
| QA browser binary | yes | Playwright-managed Chromium | Canonical browser |
| QA browser profile | yes | `~/.local/share/chatarium-qa-browser/` | Dedicated private state |
| QA browser launch | yes | `chatarium-qa-browser` / Playwright | Codex-owned |
| ChatGPT login/MFA | sometimes | headed QA Chromium | Principal only at true identity gate |
| Chatarium MV3 bridge source | yes | Git/filesystem | Codex-owned |
| Bridge load/reload | yes | launch persistent Chromium with current unpacked extension | Codex-owned |
| Browser DOM/accessibility/console/screenshots | yes | Playwright | Must be proven in new Chromium setup |
| Browser network/CDP evidence | useful | Playwright/CDP + bridge instrumentation | Machine-collected |
| Personal Edge windows/profile | never | none | OUT OF BOUNDS |

## Browser bootstrap target

Codex must bootstrap the Chromium QA browser itself.

The canonical durable pieces are:

```text
Playwright browser cache/tooling
    -> managed by Playwright

~/.local/share/chatarium-qa-browser/
    -> persistent QA browser state

~/.local/bin/chatarium-qa-browser
    -> stable human/Codex launcher

browser/edge-bridge/
    -> current unpacked Chatarium MV3 extension
```

The launcher must resolve the Playwright-managed Chromium executable rather than hard-code a versioned cache path.

If the QA profile is already locked by a running browser, automation must reuse it through a supported path or fail with a precise lock/process blocker. Do not spawn a second competing browser against the same profile.

## Authentication boundary

Do not copy personal Edge cookies, databases, profile directories, storage state, or credentials into QA Chromium.

If login is needed:

1. Codex launches `chatarium-qa-browser`;
2. Codex navigates it to the appropriate ChatGPT login surface;
3. the principal performs only the identity/consent action;
4. Codex resumes;
5. the authenticated state persists in the dedicated QA profile.

That is setup/authentication, not regression QA.

## Extension lifecycle

The extension lifecycle must be deterministic and agent-owned.

For Playwright Chromium extension QA, the browser should be launched as a persistent context with the current repository extension loaded. Browser restart against the same QA profile is the normal reload mechanism after source changes.

Do not depend on `edge://extensions`, manual extension reload, or the Playwright browser extension.

The first successful bootstrap must prove:

1. the expected MV3 service worker exists;
2. its extension ID/version correspond to the current build;
3. source changes are reflected after the automated restart/reload path;
4. the localhost Chatarium bridge roundtrip succeeds.

## History-discovery QA should not depend on egui

The browser integration under test is:

```text
authenticated QA Chromium session
    ↕
Chatarium MV3 bridge
    ↕ localhost command protocol
history discovery / exact mirror logic
    ↕
durable cache/journal
```

Prefer a dedicated CLI/test entry point or typed local RPC that exercises the same production logic. The desktop GUI may consume the same state machine, but it should not be required merely to validate browser integration.

## Acceptance proof

Do not mark the new Chromium control plane proven until one Codex run, with no principal interaction after any unavoidable login gate, can produce machine evidence for all of the following:

1. identify the Playwright-managed Chromium binary/version;
2. prove the persistent profile path is `~/.local/share/chatarium-qa-browser/`;
3. prove personal Edge is not the automation target;
4. load the current Chatarium MV3 bridge from the repository;
5. verify bridge version/service worker;
6. navigate a normal QA page;
7. inspect DOM/accessibility state;
8. inspect browser console state;
9. take a screenshot;
10. launch the Chatarium QA executable/process;
11. complete a localhost bridge roundtrip;
12. exercise history discovery/mirroring acceptance;
13. collect terminal + browser evidence;
14. cleanly stop only its own test processes.

## Next Codex goal

The next goal is to establish the Playwright-managed Chromium QA environment, not to perform another Edge-profile test.

Codex owns dependency installation, browser/profile/launcher creation, extension loading, control-plane proof, repository changes, testing, commit, and push.

The only acceptable principal interruption is a true identity/consent gate inside the dedicated QA Chromium browser.
