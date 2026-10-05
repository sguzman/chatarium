# Codex-owned QA workstation

This document defines the operating contract for Chatarium development after the 2026-10-03/04 browser-integration failure cycle.

The principal is **not** the project's manual QA runner. Routine engineering must not depend on the principal repeatedly pulling, launching, clicking, watching logs, copying output, taking screenshots, reloading extensions, or retrying browser flows.

## Canonical browser decision

As of 2026-10-04, Chatarium browser QA uses **Playwright-managed bundled Chromium**.

This supersedes the earlier plan to automate a UI-created Microsoft Edge QA profile.

The old Edge approach was rejected because a second Edge profile under the same Edge user-data root did not provide the hard isolation that had been assumed. During a Playwright extension-mode control proof, the principal's personal Edge window displayed the browser debugging indicator even though the Playwright Extension was installed only in the QA profile. That was enough to reject profile-only isolation.

The canonical boundary is now:

```text
personal Microsoft Edge
  ~/.config/microsoft-edge/
  NO Playwright extension
  NO MCP attachment
  NO CDP QA endpoint
  NO Chatarium automation
  ↓ completely out of scope

Chatarium QA browser
  Playwright-managed bundled Chromium
  ~/.local/share/chatarium-qa-browser/
  Chatarium MV3 bridge loaded from the repository
  persistent ChatGPT QA authentication
  Codex/Playwright control
```

Do not reintroduce personal Edge into the QA path for convenience.

## Browser ownership

Playwright owns the QA browser binary/runtime dependency. The user does not need an Arch `chromium` package merely to satisfy Chatarium QA.

Playwright's Chromium installation/cache is tooling state. The durable browser profile is separately fixed at:

```text
~/.local/share/chatarium-qa-browser/
```

That profile is private local state. It may contain the QA ChatGPT login, cookies, local storage, IndexedDB, extension state, and other browser data. Never commit it, copy it into the repository, or seed it from the principal's personal Edge profile.

A persistent browser profile may only be opened by one Chromium process at a time. QA tooling and the manual launcher must detect/reuse or cleanly report that lock rather than creating ambiguous duplicate instances.

## Manual launcher

Codex must provide and maintain a durable launcher:

```text
~/.local/bin/chatarium-qa-browser
```

The operator-facing command is therefore simply:

```text
chatarium-qa-browser
```

It opens the same headed Playwright-managed Chromium profile used by automated QA.

The launcher exists for unavoidable human identity actions such as initial ChatGPT login, MFA, CAPTCHA, or account consent. It is **not** a manual QA surface.

The launcher must:

- resolve the correct Playwright-managed Chromium executable instead of hard-coding a versioned cache path;
- use `~/.local/share/chatarium-qa-browser/`;
- load the current Chatarium MV3 bridge from the repository when browser flags are part of the chosen harness;
- avoid the principal's Edge/Chrome profiles entirely;
- fail clearly or reuse the existing QA process if the profile is already locked.

## Playwright control mode

Do **not** use Playwright browser-extension mode for the canonical Chatarium QA browser.

The canonical mode is a Playwright-launched persistent Chromium context with a dedicated user-data directory. Playwright MCP supports persistent state and a custom `--user-data-dir`; extension-specific options such as `--extension`, `--profile-dir-name`, and `PLAYWRIGHT_MCP_EXTENSION_TOKEN` belong only to the retired attach-to-existing-browser experiment.

The operational MCP configuration should use Playwright's Chromium browser plus the dedicated QA user-data directory. If extension launch arguments require an advanced Playwright config file or a dedicated launcher/harness, Codex owns that configuration.

The MV3 bridge itself is Chromium-compatible even though the repository directory/component name remains `edge-bridge`.

For extension testing, use Playwright's bundled Chromium because branded Google Chrome and Microsoft Edge no longer reliably support the command-line side-loading flags required for unpacked-extension automation.

## Extension lifecycle

The Chatarium MV3 bridge must be loaded from the repository automatically when QA Chromium starts.

The target source is:

```text
browser/edge-bridge/
```

The historical component name `Edge Bridge` is retained for source/history continuity; it does **not** mean Edge is the QA browser.

The automated browser harness should launch persistent Chromium with the repository extension directory using the supported Chromium extension-loading arguments. This makes source update + browser restart the deterministic extension reload path.

Do not create a permanent dependency on:

- `edge://extensions`;
- manual Load unpacked;
- manual Reload;
- copying extension files into a browser profile;
- installing the Playwright Extension into a personal browser.

Codex verifies the running bridge version/service worker itself.

## Authentication

Authentication belongs to the dedicated QA Chromium profile.

The principal may perform the initial interactive ChatGPT login in `chatarium-qa-browser` and may satisfy later MFA/CAPTCHA/account-consent challenges when genuinely required.

Do not migrate authentication by copying cookies, browser databases, or profile directories from Edge.

Once authenticated, the persistent QA profile is reused across Codex sessions. Authentication failure is reported as an identity boundary, not converted into repeated manual QA.

Example blocker:

```text
AUTH/CONSENT BLOCKED
Needed: sign in to ChatGPT in chatarium-qa-browser
Testing completed by Codex: no operator QA pending
Resume point: <exact machine state>
```

## Ownership contract

For a normal Chatarium engineering goal, Codex owns the whole loop:

```text
git sync
  ↓
inspect/reproduce
  ↓
edit
  ↓
build
  ↓
ensure Playwright Chromium dependency is present
  ↓
launch/reuse dedicated QA Chromium
  ↓
load/verify current Chatarium MV3 bridge
  ↓
launch/restart Chatarium or QA harness
  ↓
exercise browser flow
  ↓
collect logs / DOM / console / screenshots / traces
  ↓
diagnose
  ↓
edit again
  ↓
repeat until acceptance criteria pass
  ↓
run repository gate
  ↓
git status/diff review
  ↓
commit
  ↓
push
  ↓
report exact head + evidence
```

The principal performs no intermediate regression steps.

## Shell / terminal

Codex uses its ordinary local shell for Git, Cargo/pnpm, browser dependency bootstrap, browser launch/cleanup, Chatarium launch/restart, log capture, regression suites, artifact generation, and temporary test state.

Do not ask the principal to run a command that Codex can run itself.

## Native Chatarium GUI

Playwright cannot operate egui. That does **not** restore a human-QA requirement.

Order of preference for native application control:

1. CLI/test harness;
2. typed localhost/debug/test RPC;
3. deterministic test-only command or fixture;
4. accessibility/native automation;
5. compositor-level automation as a last automated fallback.

If a native action is repeatedly needed for regression testing, add a programmatic test surface rather than making the principal click it.

## Evidence rules

A Codex QA run should collect its own machine evidence, including whichever of these are relevant:

- exact Git head;
- Playwright/Chromium version;
- QA user-data directory;
- extension version/service-worker identity;
- browser console errors;
- browser screenshot;
- DOM/accessibility snapshot;
- CDP/network summary;
- terminal logs;
- generated test artifacts;
- local cache/journal proof;
- repository-gate exit status.

Private browser/session material is never committed.

## Git completion contract

Codex owns Git hygiene for the goal: sync safely, inspect status/diff, run the required gate, exclude private QA state, commit directly to the requested branch, push, and verify the remote head.

A task is not complete merely because code was edited locally.

## No-operator regression rule

Routine handoffs such as "pull this commit", "reload the extension", "run this command", "click this conversation", "copy the logs", "take a screenshot", "open DevTools", or "try again" are prohibited.

If Codex cannot perform one, the next engineering task is to remove that limitation or report a precise machine/environment blocker.

## Completed control-plane proof and next acceptance target

The Chromium control plane was proven end to end on 2026-10-04: Playwright-managed Chromium
155.0.8059.12 used `~/.local/share/chatarium-qa-browser/`, loaded the repository bridge at
version 0.4.10, routed class `chatarium-qa` automatically to workspace 6, supported DOM,
accessibility, console, screenshot, and navigation control, and never targeted personal Edge.

The live history-discovery acceptance milestone completed on 2026-10-05:

1. the dedicated profile was authenticated through the genuine identity/consent gate;
2. the production Rust account-bridge/history-discovery path ran without egui clicking;
3. bounded first-party ChatGPT traffic produced 85 observed conversation summaries;
4. account context and debugger/Network/reload boundaries were proven;
5. empty-cache fresh-tab recovery observed 3 candidate surfaces and completed cleanup;
6. the safe 85-item observed catalog persisted through the durable cache boundary;
7. a later sparse pass retained the catalog and suppressed recovery;
8. personal Edge remained outside the automation boundary and no synthetic request was used.

The next browser-integration milestone is exact single-conversation mirroring through the
existing production bridge path.

## Completion report

A browser-facing Codex final report should include:

```text
HEAD: <sha>
PUSHED: yes/no
QA BROWSER: Playwright Chromium
QA PROFILE: ~/.local/share/chatarium-qa-browser/
BRIDGE VERSION: <version>
LIVE ACCEPTANCE: pass/fail/blocker
AUTOMATED EVIDENCE: <short summary>
PERSONAL EDGE TOUCHED: no/yes
REPOSITORY GATE: pass/fail
CHANGES: <short summary>
REMAINING BLOCKER: <none or exact machine/identity boundary>
```

No human QA checklist is included unless the principal explicitly asks for one.
