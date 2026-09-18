# Chatarium capture harness

`chatarium-capture` is the Windows-first automation boundary for controlled observations of the official ChatGPT web client.

Current status: **read-only capture foundation**. The experiment model, dedicated-profile safety rules, Edge discovery, read-only `doctor`, smoke diagnostics, reusable transport, and manual profile bootstrap exist. Canonical experiment execution remains unavailable.

## Commands

```text
chatarium-capture doctor
chatarium-capture smoke-edge
chatarium-capture init
chatarium-capture run C00-idle-load
chatarium-capture run C03-send-text
```

`init` first launches ordinary Edge with the dedicated persistent profile and no CDP or remote-debugging switches. The operator signs in normally and closes that visible window. Only after the harness-owned process tree exits and the profile lock is released does `init` reopen the same profile using the Windows anonymous pipe for read-only final-target verification. The command never claims authentication was verified. `run` remains unavailable and performs no browser launch or remote mutation.

## Edge/CDP transport foundation

The library exposes the mockable `BrowserTransport` and `PageSession` boundaries in
`chatarium_capture::transport`. `DevToolsBrowserTransport` queries only the local
`/json/version` and `/json/list` resources, validates all returned WebSocket URLs
against `127.0.0.1` and the selected ephemeral port, preserves raw CDP event method
names, correlates responses by command ID, and queues unsolicited events. Page
attachment is limited to `about:blank` and `https://chatgpt.com` targets.

`LaunchedEdge::launch` discovers Edge in standard Windows locations, requires the
exact `%LOCALAPPDATA%\Chatarium\capture-browser\edge-profile` path, and refuses
known/default Edge profile trees and path traversal. It requests
`--remote-debugging-address=127.0.0.1` and `--remote-debugging-port=0`. Existing
harness locks or a stale `DevToolsActivePort` cause a visible error; they are never
silently removed before launch. Call `LaunchedEdge::shutdown` to terminate/wait for
the owned Edge process tree, remove its lock/active-port file, and append the durable cleanup
event. On Windows it invokes `SystemRoot\System32\taskkill.exe` with `/PID`, `/T`, and `/F`,
scoped to the launched PID; it does not search by image name. Drop also makes best-effort
process cleanup if explicit shutdown is missed.

The transport issues no ChatGPT backend requests. `init` uses only browser readiness
and target-discovery commands during its verification phase; it does not attach to a
page or mutate page state. `run` does not invoke CDP. Once a command is sent, timeout, disconnect, or I/O failure does not prove
that the browser did not execute it; callers must preserve that uncertainty and must
not automatically retry a mutating command. Tests inject process, HTTP, and WebSocket implementations, so they require
neither an installed Edge browser nor a ChatGPT account.

## `smoke-edge`

This explicit operator command verifies the concrete local Edge/CDP transport before
ChatGPT login, navigation, or experiments are enabled. It launches the standard
installed Microsoft Edge executable with exactly `about:blank`, using only the
Chatarium-owned `%LOCALAPPDATA%\Chatarium\capture-browser\edge-profile` profile in
incognito mode and an ephemeral `127.0.0.1` DevTools port. It attaches only when
DevTools reports exactly one `about:blank` page target and executes the read-only
`Page.getFrameTree` command.
It never navigates to ChatGPT or contacts `chatgpt.com`, does not require login, does
not request or copy cookies, and does not use the user's normal Edge profile.

The dedicated `edge-profile` remains on disk after the smoke command; the diagnostic
uses an incognito window so it does not restore that profile's previous browser
session. The private diagnostic run is stored under
`%LOCALAPPDATA%\Chatarium\captures\diagnostics\<run-id>` with `run.json` and
append-only `events.jsonl`; the smoke does not collect page bodies or cookie data.
The temporary harness lock and `DevToolsActivePort` file are removed and their
absence is checked before a `PASS` result is printed. The launched Edge process tree
is closed on both success and failure. A failure prints its primary error, cleanup
status, and run path when the run directory was created.

When a DevTools port is discovered, Windows diagnostics take one TCP-table snapshot
before readiness and a second immediately before cleanup if startup fails. Only TCP
records with the selected port are journaled. The summary includes address family,
state, owner PID, and whether the owner is the launched Edge process, a descendant, a
different process, or unknown. It also reads the HKLM and HKCU
`RemoteDebuggingAllowed` values without changing registry state; an explicit disabled
value stops readiness attempts. Connection failures are reported separately from
listener observations, and no firewall cause is inferred from a failed connection.
If Windows cannot expose a snapshot, the diagnostic records the inspection error and
leaves the result unknown.
The policy values are checked as soon as the owned Edge process starts, before waiting
for `DevToolsActivePort`; an explicit disabled value stops startup promptly. In that
preflight case no listener snapshot is possible because no port has been selected.

After parsing the validated loopback port from `DevToolsActivePort`, startup readiness
retries only transient local connection/readiness errors while Edge remains alive.
Each HTTP attempt is bounded by the remaining time in the single 15-second startup
deadline. Invalid endpoint metadata, malformed protocol data, and other permanent
validation failures stop immediately. The journal records the readiness start and one
terminal result with attempt count and the last transient error, when present.
Readiness probes only `127.0.0.1` and `::1` at that selected port. The first family
that returns valid DevTools version metadata is retained for subsequent HTTP and
WebSocket socket connections. Browser-provided WebSocket URLs may identify
`localhost`, IPv4 loopback, or IPv6 loopback; their path/handshake metadata is preserved,
but Chatarium connects to the selected concrete loopback socket without resolving
browser-provided hostnames.

Run it manually on Windows with:

```powershell
cargo run -p chatarium-capture -- smoke-edge
```

This smoke command does not enable `run`.

## `doctor`

`doctor` is read-only. It reports:

- harness version;
- derived Chatarium-owned Edge profile path;
- whether that path is distinct from known default Edge profile trees;
- a discovered Edge executable, if present in standard Windows locations;
- whether the dedicated profile already exists;
- embedded canonical experiment definitions.

Run through Cargo during development:

```powershell
cargo run -p chatarium-capture -- doctor
```

## Invariants

- Never use the user's default Edge profile.
- Never copy/import cookies or credentials from another browser profile.
- Exact synthetic experiment text is versioned under `protocol/experiments/` and embedded in the harness build.
- Unknown experiment IDs fail; the program does not improvise a new experiment.
- A mutating experiment must not automatically retry after an ambiguous remote outcome.
- Live capture must journal incrementally before the project treats it as usable evidence.
- Portable artifacts and private local evidence are different products.

See `docs/CAPTURE_HARNESS.md` for the authoritative v0.1 contract and GitHub issue #5 for implementation acceptance criteria.
