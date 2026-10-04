# Codex-owned QA workstation

This document defines the operating contract for Chatarium development after the 2026-10-03/04 browser-integration failure cycle.

The principal is **not** the project's manual QA runner. Routine engineering must not depend on the principal repeatedly pulling, launching, clicking, watching logs, copying output, taking screenshots, reloading extensions, or retrying browser flows.

## Control-plane proof requirement

See [QA control-surface map](QA_CONTROL_SURFACE.md).

Do not equate "window is open on workspace 6" with "Codex can control it." Browser, extension-management, native-GUI, and compositor surfaces require an explicit, demonstrated control channel. Until that proof exists, mark the surface UNPROVEN and make establishing the control channel part of the engineering goal.

For the current history-discovery failure, prefer a shell-driven QA executable/local bridge harness over egui clicking. Native desktop control is not required merely because the production app has a GUI.

## Current provisioned environment

The principal has already provisioned a dedicated Microsoft Edge **QA profile**:

- authenticated to the accounts needed for Chatarium browser testing;
- Chatarium Edge Bridge installed as an unpacked extension;
- visually grouped with Codex on Hyprland workspace **6**;
- separate in intent from the principal's normal browsing profile.

Workspace 6 is a useful visual quarantine, not a security boundary. Automation must still positively identify the QA browser/profile before acting.

The principal's normal Edge profile and normal browsing windows are outside the Chatarium automation boundary. Do not attach broad browser automation to the principal's whole Edge user-data root merely because it is convenient.

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
launch/restart Chatarium
  ↓
attach to QA Edge
  ↓
reload/verify test extension when needed
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

The principal does not perform intermediate QA steps for Codex.

Codex should use direct commits to the requested branch. Do not create a PR unless explicitly requested.

## Tool split

### Shell / terminal

Codex uses its ordinary local shell for:

- `git pull --ff-only`, status, diff, commit, and push;
- Cargo/pnpm/tooling commands;
- building the Edge extension and desktop app;
- starting/stopping/restarting Chatarium processes;
- capturing stdout/stderr to files;
- inspecting process state;
- running local regression suites;
- generating and comparing artifacts;
- managing temporary test state.

The shell is the default way to operate native project machinery. Do not ask the principal to run a command that Codex can run itself.

### QA Edge

The preferred generic browser-control path is **Playwright MCP browser-extension mode** attached only to the dedicated QA Edge profile.

Upstream Playwright documents extension mode specifically for connecting to existing Edge/Chrome tabs while reusing the profile's authenticated session and installed extensions. It also supports pinning a particular browser profile with `--profile-dir-name`.

A Codex stdio MCP configuration can use the user's preferred package manager:

```toml
[mcp_servers.playwright]
command = "pnpm"
args = ["dlx", "@playwright/mcp@latest", "--browser=msedge", "--extension"]
```

Once the exact QA Edge profile directory name is known, pin it:

```toml
[mcp_servers.playwright]
command = "pnpm"
args = [
  "dlx",
  "@playwright/mcp@latest",
  "--browser=msedge",
  "--extension",
  "--profile-dir-name=Profile N",
]
```

Do not guess `Profile N`. Codex must discover and verify the QA profile identity before pinning it.

Browser MCP is responsible for:

- navigation;
- clicking/typing;
- DOM/accessibility inspection;
- console inspection;
- screenshots;
- browser-side assertions;
- existing-tab and authenticated-session work;
- exercising the unpacked Chatarium extension.

### Browser-control bootstrap

If the Playwright browser extension is not yet installed/connected in the QA profile, **that is an automation bootstrap task, not a request for manual QA**.

Codex must first try to establish the browser-control channel itself. Workspace-6 desktop automation may be used temporarily for bootstrap if necessary. Once browser MCP is working, browser QA should move to Playwright/DevTools rather than screen-coordinate automation.

If browser security requires an explicit human identity/consent action that cannot be automated (for example a CAPTCHA, MFA challenge, or browser permission confirmation that automation is not allowed to accept), Codex stops with a precise blocker such as:

```text
AUTH/CONSENT BLOCKED
Needed: reauthenticate QA Edge
Testing completed by Codex: none pending from operator
Resume point: <exact command/state>
```

That is not a QA handoff. The principal may satisfy the identity gate once, after which Codex resumes the same automated loop.

### DevTools/CDP alternative

Chrome DevTools MCP / direct CDP is an acceptable alternative when it is scoped to a QA-only Edge instance or QA-only user-data directory.

Do **not** auto-connect DevTools MCP to the principal's shared `~/.config/microsoft-edge` root if that can expose normal browsing profiles/tabs. Microsoft explicitly documents that an auto-connected agent inherits the active browser session.

If a separate QA-only Edge user-data root is later created, DevTools MCP becomes a good second browser driver.

## Native Chatarium GUI

Playwright cannot operate egui.

That does **not** restore a human-QA requirement.

Order of preference for native application control:

1. CLI/test harness;
2. typed localhost/debug/test RPC;
3. deterministic test-only command or fixture;
4. accessibility/native automation;
5. compositor-level automation as a last automated fallback.

If a native action is repeatedly needed for regression testing, add a programmatic test surface rather than making the principal click it forever.

A native UI observation that genuinely cannot be automated should be treated as missing test infrastructure. The engineering task is to add that infrastructure.

## Evidence rules

A Codex QA run should produce its own evidence. Depending on the failure surface, this can include:

- terminal log bundle;
- exact Git head;
- extension version;
- browser console errors;
- browser screenshot;
- DOM/accessibility snapshot;
- CDP/network summary;
- generated test artifact;
- local cache/journal proof;
- exit status from the repository gate.

Do not ask the principal to transcribe evidence that exists on the QA machine.

Private browser/session material is never committed. Keep reusable credentials, cookies, bearer tokens, private conversation bodies, and raw sensitive captures outside Git.

## Git completion contract

Codex owns Git hygiene for the goal.

Before pushing:

1. sync safely with `git pull --ff-only` when appropriate;
2. inspect `git status` and the actual diff;
3. run the required test/QA gate;
4. do not commit generated private QA state;
5. make a descriptive direct commit;
6. push;
7. verify the remote head.

A task is not complete merely because code was edited locally.

## No-operator regression rule

The following are prohibited routine handoffs:

- "pull this commit";
- "reload the extension";
- "run this command";
- "click this conversation";
- "wait and tell me what happens";
- "copy the terminal lines";
- "take a screenshot";
- "try it again";
- "open DevTools and inspect...";
- "verify the version for me".

Codex performs those steps on the QA workstation.

If Codex cannot perform one, the next engineering task is to remove that limitation or produce a precise machine/environment blocker. It must not silently convert missing automation into labor for the principal.

## Immediate Chatarium acceptance target

At the time this policy was introduced, the repository head under test was Edge Bridge 0.4.10 with the fresh-tab history bootstrap path.

The QA Edge screenshot still showed an older Chatarium Edge Bridge version. The next autonomous Codex run therefore owns all of the following:

1. sync the repository;
2. make the QA Edge profile load/reload the current unpacked extension;
3. verify the browser reports the expected extension version;
4. launch Chatarium Desktop and capture its terminal log;
5. let startup history acquisition run;
6. inspect whether frozen 0.3 discovery succeeds directly or the isolated fresh-tab fallback recovers it;
7. if it fails, use the automatically collected browser/terminal evidence to debug and iterate;
8. rerun until the acceptance boundary is proven or a concrete non-QA blocker is reached;
9. run the repository gate;
10. commit and push any required fix.

The principal should receive the result, not a test script to execute.

## Completion report

A useful Codex final report for a browser-facing goal contains:

```text
HEAD: <sha>
PUSHED: yes/no
QA EDGE: <profile identity / extension version>
LIVE ACCEPTANCE: pass/fail/blocker
AUTOMATED EVIDENCE: <short summary>
REPOSITORY GATE: pass/fail
CHANGES: <short summary>
REMAINING BLOCKER: <none or exact machine/identity boundary>
```

No human QA checklist is included unless the principal explicitly asks for one.
