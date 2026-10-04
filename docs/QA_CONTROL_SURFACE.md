# QA control-surface map

This document answers one question before Chatarium asks Codex to perform live QA:

> Can Codex actually control every moving part needed for this test without asking the principal to do anything except submit the Codex prompt?

The answer must be based on a proven control channel, not on the fact that a window is visible on the same workspace.

## Principal contract

The principal's routine QA actions are exactly **zero**.

The principal may:

- submit the Codex goal prompt;
- read the final result.

The principal does **not**:

- run Git commands;
- build;
- launch Chatarium;
- reload extensions;
- select profiles;
- click browser pages;
- open DevTools;
- watch terminal output;
- copy logs;
- take screenshots;
- retry a flow;
- inspect the GUI;
- decide whether a run "looks right";
- commit or push.

A true account-holder identity gate such as MFA/CAPTCHA is a blocker, not QA. Codex must report it explicitly rather than turning it into a testing checklist.

## Important correction

Putting Codex and QA Edge on Hyprland workspace 6 does **not** prove that Codex can control Edge.

A workspace is only window organization.

Likewise, saying "Playwright can control Edge" is insufficient unless one of these control channels is actually configured and demonstrated against the QA browser:

1. Playwright MCP browser-extension mode;
2. DevTools/CDP against a QA-only Edge instance;
3. WebDriver against a QA-only Edge instance;
4. desktop/native automation capable of operating the QA Edge window.

Until one of those is proven, Edge control is **UNPROVEN**.

The 2026-10-04 screenshot shows an authenticated QA Edge profile with Chatarium Edge Bridge 0.4.7 loaded. It does **not** show or prove a Playwright MCP connection. Therefore the browser is provisioned for QA, but autonomous browser control is not yet proven.

## Current control matrix

| Surface | Needed for current history-discovery QA? | Codex control mechanism | Current status | Rule |
| --- | --- | --- | --- | --- |
| Git pull/status/diff/commit/push | yes | shell | PROVEN CAPABILITY | Codex owns it |
| Cargo/pnpm/build/tests | yes | shell | PROVEN CAPABILITY | Codex owns it |
| Start/kill/restart Chatarium process | yes | shell | PROVEN CAPABILITY | Codex owns it |
| Capture Chatarium stdout/stderr | yes | shell redirection / process harness | PROVEN CAPABILITY | Codex owns it |
| Inspect journal/cache files | yes | shell | PROVEN CAPABILITY | Codex owns it |
| Exercise history discovery logic | yes | localhost bridge / dedicated CLI QA harness | IMPLEMENTABLE WITHOUT GUI | Prefer this over egui clicking |
| Native egui clicking | no for current history bug | test RPC/CLI first; native automation only if unavoidable | NOT REQUIRED | Do not make principal click |
| Existing QA Edge normal web tabs | yes | Playwright MCP extension or CDP | UNPROVEN IN CURRENT SETUP | Must bootstrap and prove |
| QA Edge authenticated session | yes | already present in QA profile | PROVISIONED, NOT YET AGENT-CONTROLLED | Preserve it |
| Chatarium Edge Bridge current source files | yes | Git + filesystem | PROVEN CAPABILITY | Codex owns it |
| Reload/update unpacked Chatarium extension in existing QA profile | yes for first autonomous run | browser-chrome/desktop control, or replace with an agent-launchable QA browser architecture | UNPROVEN | This is a real bootstrap gap |
| `edge://extensions` browser chrome | only if using the existing unpacked-extension workflow | privileged browser UI / desktop automation | UNPROVEN | Do not assume normal page automation can operate it |
| Browser DOM/accessibility/console/screenshots | useful | Playwright MCP once attached | UNPROVEN UNTIL MCP ATTACH WORKS | Prove once |
| Browser network/CDP evidence | useful | CDP/DevTools or Chatarium extension instrumentation | PARTIALLY AVAILABLE | Prefer machine artifacts |
| Hyprland workspace switching/window discovery | bootstrap only | `hyprctl` shell IPC | LIKELY, MUST VERIFY LOCALLY | Workspace 6 is not browser control |
| Pointer/keyboard automation of arbitrary Wayland apps | bootstrap/fallback only | compositor/input automation | UNPROVEN | Only needed if API/CLI control cannot replace it |
| Personal Edge windows/profile | never | none | OUT OF BOUNDS | Do not touch |

## Current history-discovery QA should not depend on egui

For the current Chatarium failure, there is no reason to require Codex to click the desktop app.

The thing being tested is:

```text
authenticated Edge session
    ↕
Chatarium Edge Bridge
    ↕ localhost command protocol
history discovery / exact mirror logic
    ↕
durable cache/journal
```

That can be exercised by a machine-controlled executable.

The preferred next implementation is a dedicated QA entry point such as:

```text
cargo run -p chatarium-desktop --bin chatarium-qa -- history-discovery
```

or an equivalent test-only command/RPC.

Its job is to:

1. start the localhost browser bridge;
2. wait for the browser extension;
3. probe authentication;
4. run frozen primary discovery;
5. run fresh-tab recovery when policy requires it;
6. print a typed JSON result;
7. write a sanitized diagnostic bundle;
8. exit success/failure.

This lets Codex test the actual browser integration from the shell without touching egui at all.

The desktop GUI can consume the same library/state machine. The QA executable is not a second implementation of discovery.

## The browser bootstrap gap

There is one important unresolved moving part right now:

> How does Codex obtain reliable control of the already-authenticated QA Edge profile and reload/update the unpacked Chatarium extension without the principal touching Edge?

This must be solved before claiming zero-touch live QA.

### Path A — Playwright MCP extension on the existing QA profile

Upstream Playwright supports connecting to existing Edge tabs through its browser extension and can target a specific browser profile.

Advantages:

- keeps the already-authenticated QA session;
- does not require a separate compositor;
- normal web-page interaction becomes directly agent-controlled;
- personal profile can remain without the Playwright extension.

Bootstrap problem:

- the Playwright extension itself must be present and connected in the QA profile;
- the current screenshot does not prove that it is installed;
- browser-internal pages such as extension management must not be assumed controllable by ordinary page automation.

Codex must either automate this bootstrap through a native/browser control channel or report that this path is blocked. The principal is not the fallback installer.

### Path B — QA-only Edge process controlled by CDP/WebDriver

Microsoft Edge supports a distinct user-data directory and DevTools/WebDriver control.

Advantages:

- complete process ownership by Codex;
- no risk of attaching to the principal's personal Edge process;
- launch/kill/restart is shell-controlled;
- remote debugging is deterministic;
- browser MCP can attach directly.

Bootstrap problem:

- the existing authenticated QA state currently lives in the UI-created profile under the normal Edge user-data root;
- moving/copying that state into a dedicated QA-only user-data directory must be proven safe and complete;
- do not copy live browser databases casually or assume cookies alone are sufficient.

This path is stronger isolation, but authentication-state migration is real engineering work.

### Path C — desktop automation on workspace 6

If A and B cannot be bootstrapped without touching privileged browser UI, Codex may automate the current workspace directly.

Potential building blocks include:

- `hyprctl` for workspace/window discovery and focus;
- screenshot tooling for visual state;
- Wayland/XWayland keyboard/pointer automation;
- accessibility tooling where available.

This is the place where compositor/desktop automation becomes relevant.

A **second** compositor is not automatically required. The existing Hyprland session may be sufficient if its automation surfaces can reliably target only workspace 6.

But this must be demonstrated. It was wrong to claim categorically that no compositor/desktop-control work would be needed before proving one of the API-level browser paths.

## Extension reload problem

The current QA browser shows Chatarium Edge Bridge 0.4.7 while the repository has moved beyond it.

That means the first fully autonomous run must solve extension update/reload.

After the bootstrap is solved, Chatarium should make future reloads agent-owned as well. Options to investigate include:

- an explicit QA/dev command in the extension that calls `chrome.runtime.reload()` after source files are updated;
- an agent-owned browser process whose lifecycle reloads the tested extension deterministically;
- a browser automation path that can reload the extension without operator action.

Do not keep a permanent dependency on the principal opening `edge://extensions` and clicking Reload.

## Acceptance proof for "Codex controls QA Edge"

Do not mark browser control proven until one Codex run, with no principal interaction after the prompt, can produce machine evidence for all of the following:

1. identify the QA Edge profile/session;
2. prove it did not attach to the personal profile;
3. read a normal page from the authenticated QA session;
4. navigate a QA tab;
5. take a screenshot;
6. read browser console state;
7. verify Chatarium Edge Bridge version;
8. cause the tested extension/runtime to reload or otherwise launch the exact current build;
9. launch the Chatarium QA executable;
10. complete a localhost extension roundtrip;
11. collect terminal + browser evidence;
12. cleanly stop its own test processes.

Only then is "Codex can control Edge" a proven project capability.

## Goal for the next Codex prompt

The next Codex goal is **not** "test Chatarium 0.4.10."

It is:

> Establish and prove the zero-touch QA control plane. Inventory the actual local QA Edge profile and available Hyprland/browser-control tooling; choose the least invasive control path that keeps the personal Edge profile out of scope; build a headless/CLI history-discovery QA entry point so egui is not part of the test; bootstrap autonomous control of the QA Edge profile or a QA-only equivalent; prove extension version/reload control; then run the history-discovery acceptance test. Codex owns all Git/build/run/browser/evidence/commit/push steps. The principal performs no QA actions.

If the control plane itself cannot be established, stop with the exact technical blocker and the evidence gathered. Do not return a manual workaround.
