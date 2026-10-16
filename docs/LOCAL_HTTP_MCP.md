# Two fixed local Streamable HTTP MCP servers

Chatarium supports these **user-run**, explicitly trusted loopback services. It
does **not** start additional daemon copies or search the LAN for servers.

| Provider for model MCP requests | Exact endpoint | Health | Server project |
| --- | --- | --- | --- |
| `chatarium.local.mathematica` | `http://127.0.0.1:8765/mcp` | `http://127.0.0.1:8765/healthz` | [sguzman/mathematica-mcp](https://github.com/sguzman/mathematica-mcp) |
| `chatarium.local.manifoldr` | `http://127.0.0.1:8877/mcp` | `http://127.0.0.1:8877/healthz` | [sguzman/manifoldr](https://github.com/sguzman/manifoldr) |

These ports and paths come from the **current server repositories** (Mathematica
v1.0.0 and Manifoldr v0.4.0). A service bound **only to its LAN IP** will not
answer `127.0.0.1`; to use this loopback-only client, ensure that the configured
service also listens on loopback (or binds to `0.0.0.0` on a trusted machine).
The Chatarium client does not change the host service config.

## Transport and authority

This is a separate, explicitly scoped **Streamable HTTP** transport. It does
not configure or weaken the existing **network-isolated stdio** executor.

- A persistent, local Node 22+ JSON-lines process communicates only with the
  two literal `127.0.0.1` endpoint URLs. No arbitrary URLs, redirects,
  credentials, implicit shell commands or model traffic are forwarded.
- Manifoldr uses its documented **2025-11-25** stateless MCP HTTP/JSON mode.
  Mathematica uses **2025-03-26** and retains its MCP transport session across
  calls in the same Chatarium local conversation. Client sessions are isolated
  by local conversation and server.
- Tool invocation is processed by Chatarium's immutable call intent, exact
  named operation allowlist, model tool policy, durable route/Allow decision,
  and **separate one-shot Run** boundary. The route is reserved durably
  before any HTTP call. Unknown network outcomes are not retried.
- By default **unconfigured exact policies are Ask**, *not Automatic*.
  Users must approve the proposed route and click the separate localhost MCP
  Run button. Explicit user-authored standing rules for individual tools may
  be changed in Chatarium's MCP settings; never create permissions by parsing
  model output or treating a successful server health check as authorization.
- Tool outcomes and errors are added to the existing durable tool audit and
  admitted to the model only through Chatarium's tool-result admission process.

## In-conversation approval and execution status

The latest MCP call's **durable state** is shown in the conversation area,
without opening Advanced / Tools / MCP. This status is projected from the
journal, not inferred from the assistant's text or from server reachability.

- **Requested, not executed:** An Ask-policy call presents **Allow this call**
  and **Deny**. No HTTP call has been dispatched.
- **Approved, not executed:** After an explicit Allow is journaled, press
  **Run approved tool once**. This separate Run action reserves the one-shot
  route, then dispatches the tool. Do not confuse the purple request card with
  a completed result.
- **Automatically authorized:** Only exact, user-saved `automatic` policies
  use the existing automatic dispatch path. A model-proposed request cannot
  authorize itself; unconfigured operations remain Ask.
- **Dispatched without result:** The call may be running or its outcome may
  be uncertain. The app never silently retries it. Following a restart it
  does not claim the old model continuation is active.
- **Completed or failed:** A terminal result/error is journaled and displayed
  separately in the purple MCP activity. Chatarium attempts to admit that
  result into the still-active agent continuation.

Tool approval and execution remain guarded by Chatarium's immutable intent,
persisted decision, route gate, at-most-once dispatch and tool-result admission.
A Deny terminates the matching in-memory model continuation. Preflight failures
clear the pending UI flag instead of leaving the interface indefinitely busy.
An interrupted in-memory model continuation is **not** magically restored when
the application reopens, even if its tool request and outcome survive.

**Mathematica warning:** Wolfram Language can execute OS commands as the user
running the Mathematica server. Its HTTP endpoint has no built-in
authentication or TLS. Treat it like a trusted local shell, especially when
considering standing automatic permissions or exposing the daemon to LAN.
No Mathematica operation is pre-authorized in Chatarium.

Manifoldr exposes only the nine documented **public, read-only** tools. Its
server cannot place bets, sell positions or cancel orders through MCP.

## Tools and examples

The `chatarium.builtin::tools.list` directory describes 15 operations:
Manifoldr `popular_markets`, `trending_markets`, `high_volume_markets`,
`search_markets`, `suggested_markets`, `list_categories`,
`get_category`, `get_market`, `get_probabilities`; Mathematica
`mathematica_create_session`, `mathematica_execute_code`,
`mathematica_close_session`, `mathematica_list_sessions`,
`mathematica_get_finance`, `mathematica_time`.

For example, the model may propose:

```json
{"chatarium_mcp_call":{"provider":"chatarium.local.manifoldr","tool":"search_markets","arguments":{"term":"AI","limit":5}}}
```

When asked to evaluate Mathematica expressions, first call
`mathematica_create_session` and retain its returned `session_id`. Later
`mathematica_execute_code` requests carry that Wolfram session ID; they must
run from the **same Chatarium local conversation** because ownership is enforced
by the Mathematica daemon's MCP transport session. Closing a Wolfram session
frees a kernel slot. Restarting either service invalidates the MCP/Wolfram
sessions: never assume a previous ID survives restart.

## Deployment and diagnostics

Expected user units, when installed by the separate projects:

- `systemctl --user status mathematica-mcp.service`
- `systemctl --user status manifoldr-mcp.service`

Read-only local checks:

```sh
curl --max-time 3 http://127.0.0.1:8765/healthz
curl --max-time 3 http://127.0.0.1:8877/healthz
```

Both must report `ok` for HTTP daemon reachability. This **does not** prove
that the licensed Wolfram kernel starts, that Manifold's upstream public API
is reachable, or that any model invocation succeeded.

Chatarium's transport tests are in `tools/local-http-mcp/client.test.mjs` and
exercise mocked initialization, independent conversation sessions, SSE,
localhost URL validation and tool invocation without touching your running
services. CI runs these tests on Windows and Linux.

The tool transport is implemented by `tools/local-http-mcp/client.mjs`,
`tools/local-http-mcp/worker.mjs` and
`apps/desktop/src/local_http_mcp.rs`. When running directly from a source
checkout, the Node worker script is found relative to Chatarium's source
directory. An independently packaged binary needs an explicit installation
layout for that worker; installed-binary portability is not yet certified.
