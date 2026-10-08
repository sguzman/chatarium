# Local tool integration

Status: **ACTIVE SUBSTRATE**. One side-effect-free builtin `hello` smoke adapter
is enabled. General MCP, arbitrary local process, filesystem, and network tool
execution remain disabled.

This is Chatarium's local tool-call control plane, not a second invisible
function-execution pipeline. The authoritative state is the append-only journal.
Tools must not acquire extra authority merely because a model generated an
invocation-shaped string.

## Current durable boundaries

The current Rust and desktop implementation has these separate layers:

1. **Provider identity.** `ToolProviderId` and validated
   `ToolProviderName` register a provider. A separately recorded
   `ToolProviderEndpointBound` correlates it to one `RouteEndpointId`.
2. **Immutable call intent.** `ToolCallId`, source `SessionId`,
   `ToolProviderId`, validated operation name, and exact argument text are
   recorded in `ToolCallRecorded`. Recording is not execution.
3. **Route/permission correlation.** A dedicated `RouteClass::ToolCall`
   proposal must use `RoutePolicy::RequireApproval`; the immutable call is
   correlated through `ToolCallRouteBound`. The route must map the source
   session endpoint to the registered provider endpoint. User Allow/Deny
   decisions are durable and cannot be inferred from call text.
4. **One-shot route dispatch authority.** The existing typed
   `RouteGate`/`DispatchPermit` is the only permissible dispatch boundary.
   Tool calls default to inert. The **only** currently enabled execution path
   is a side-effect-free builtin `hello` proof requiring exact provider name,
   operation, JSON argument validation, explicit Allow, and a separate user
   click to dispatch. No other call/provider is executable.
5. **Terminal adapter-result audit.** `ToolCallOutcomeObserved` is an
   independent exact-text result/error fact. Replay requires the call to be
   correlated to the exact `ToolCall` route and requires that route to have
   already consumed a **user-authorized one-shot dispatch permit**. Results
   are correlated to the immutable call, provider, source session, route
   binding, dispatch event, and observation event. Each call/route has at most
   one terminal outcome.
6. **Separate context-admission decision.** A terminal tool outcome is
   context-excluded by default. A locally owning conversation may explicitly
   Admit or Exclude the exact `ToolCallId` outcome through a reversible durable
   `ToolResultContextDecisionRecorded` event. This grants only inference
   evidence visibility; it never grants tool execution authority.

A tool call with no observed terminal result stays unresolved. A durable route
dispatch does not imply actual execution or success, and a generic
`RouteResultObserved` is not automatically interpreted as an application/tool
result.

The tool outcome replay rejects duplicate outcomes, an outcome before
dispatch, mismatched call/route correlation, missing explicit approval,
oversized results, and blank error bodies. A checked pre-append API validates
the entire prospective result before mutating the authoritative journal.
Archive integrity validates all tool/provider/call/outcome journals.

The new tool outcome audit is also the storage contract for future adapters.
The builtin hello adapter produces a real deterministic result after its
one-shot dispatch; all other providers remain inert. No tool result is
silently entered into ordinary transcript or shared memory. Tool context enters
Context Composer only after a separate durable user Admit decision.

## Recovered legacy tool-envelope compatibility

The original wire contract has now been recovered from
[sguzman/chatgpt-tool-shim](https://github.com/sguzman/chatgpt-tool-shim)
at commit `8682ea08f86734afa028805638fb51d1500ad3b4`, particularly
`src/protocol/parse_tool_calls.ts`, `format_tool_result.ts`,
`types.ts`, and `test/protocol.test.ts`.

The existing extension uses a complete XML-like element with attributes and a
JSON body:

```xml
<tool_call name="clock.now">
{}
</tool_call>
```

An optional `id` attribute may supply correlation identity. Otherwise the
shim generates `call_<hex>` using 32-bit FNV-1a over the JavaScript UTF-16
units of `name:trimmed_json_body`.

Terminal output uses a separate result element with explicit correlation and
JSON outcome content:

```xml
<tool_result name="clock.now" id="call_example">
{"ok":true,"now":"2026-05-08T00:00:00.000Z"}
</tool_result>
```

The existing shim defines eight tool names including `hello`, `clock`,
`clock.now`, browser tab read/list operations, and `local.mcp.call`.
Those were extension-specific capabilities, **not** implicit permissions for
Chatarium's Linux desktop. Tool names or XML text alone create no execution
authority.

`crates/protocol/src/tool_envelope.rs` now contains a pure Rust compatibility
parser/formatter for this legacy envelope, including the original hash
derivation and an intentionally stricter full-message/attribute validation.
It rejects fenced examples, surrounding prose, malformed JSON, duplicate or
unknown XML attributes, and oversized envelopes. The result reader/formatter
preserves the legacy `<tool_result>` structure without treating it as a
native MCP JSON-RPC transport.

This is **protocol compatibility**, not a new adapter or automatic model tool
execution. Chatarium-internal provider/call/route identities remain separate
from the optional legacy wire `id`, and a future adapter must correlate them
explicitly rather than conflating the two identity domains.

The model-facing request shape, if any, must separately respect the ready
Local Inference Contract for the active SIWC profile/model. Native capability
availability is not permission authority to run a tool.

## Native MCP 2026-07-28 wire boundary

`crates/protocol/src/mcp_wire.rs` is now a second, distinct pure protocol
surface. It targets the published **2026-07-28** MCP revision, not the older
stateful 2025 handshake. It does not implement an MCP transport or execute a
call.

The 2026 revision has no `initialize`/`initialized` handshake or
protocol-level session. Requests such as `server/discover`, `tools/list`, and
`tools/call` carry `io.modelcontextprotocol/protocolVersion`,
`io.modelcontextprotocol/clientCapabilities`, and client identity in their
own `params._meta`. These fields are protocol metadata, not Chatarium route
permission.

The pure codec provides:

- deterministic, bounded JSON-RPC request building and one-frame stdio
  serialization with escaped embedded newlines;
- exact numeric request-ID correlation for responses and structured errors;
- strict rejection of unsolicited/batched/multi-frame/oversized responses;
- explicit refusal of `inputRequired` multi-round-trip requests: no model
  or server output can silently cause follow-up execution;
- argument and tool-name validation before any later execution adapter could
  consume a route permit.

**Two separate representations remain intentional:** legacy Tool Shim
`<tool_call>`/`<tool_result>` XML-like envelopes are compatibility artifacts;
native MCP 2026 messages are JSON-RPC. An adapter must explicitly correlate the
legacy envelope identity with Chatarium's durable `ToolCallId` and the MCP
request ID; these identities must not be guessed to be equivalent.

Official protocol: [MCP 2026-07-28 specification](https://modelcontextprotocol.io/specification/2026-07-28).
The current revision changes transport assumptions materially; do not invent a
2025-style protocol session for new adapters.

### Inert stdio provider configuration

The first transport *configuration* substrate is landed in
`crates/core/src/tool.rs` and
`crates/store/src/tool_transport_config_audit.rs`.
It adds a durable `ToolProviderTransportConfigured` event, but deliberately
**does not spawn or activate a server**.

A configuration holds one absolute Linux executable path, a bounded argv
vector (not a shell command line), and an exact operation allowlist.
Validation rejects relative/parent-traversal executable paths, control
characters, excessive arguments, duplicate/empty operations, and oversized
individual values. The selected program's existence and trust are **not**
asserted by storing the path. The transport implementation must separately
verify and enforce those properties before any future launch.

The provider must be registered and have a durable routing endpoint first.
Configuration is one-shot immutable in this first audit version, and it
cannot be applied retroactively to a provider with already-recorded calls.
The special `chatarium.builtin` identity cannot acquire an external stdio
executable. Archive integrity replays this separate audit fail-closed.

Transport configuration does **not** grant invocation, process, file,
network, or inference context permissions. In particular, a configured
operation allowlist is necessary but never sufficient to consume a
`DispatchPermit`.

### Non-executing stdio call preflight

`crates/store/src/tool_stdio_preflight.rs` now provides a read-only,
fail-closed `preview_stdio_tool_invocation` projection. It joins the current
authoritative journal across the immutable tool call, `ToolCall` route, current
local-conversation session leaf, provider endpoint binding, and immutable stdio
configuration.

A preview requires the exact route to remain undispatched and explicitly
**Allowed by the user** under `RequireApproval`. It rejects stale/retired
source sessions, endpoint mismatches, unconfigured providers, operations outside
the configured allowlist, invalid JSON-object arguments, and mismatched legacy
XML-like envelopes. It produces exactly one bounded MCP 2026
`tools/call` JSON-RPC frame correlated to `ToolCallId` as a numeric request
ID; optional legacy envelope IDs are not mistaken for local identities.

The preview does **not** record an event, consume a permit, activate an
executable, launch a process, or interpret a result. A preview may become stale
immediately; it must never be treated as a cached authorization token. A future
runner must rerun these checks at execution time and separately enforce
explicit provider activation and the one-shot dispatch boundary.

### Linux executable metadata inspection

`crates/store/src/tool_stdio_executable_inspection.rs` provides a second
non-executing check for an external Linux stdio executable. It rejects missing
paths, symlink components, non-directory ancestors, group/world-writable
ancestor directories, non-regular targets, files lacking executable mode,
and executable files writable by group or world. Non-Linux builds explicitly
report unsupported platform.

This is conservative metadata inspection, **not an authenticity guarantee**.
A filesystem path can change after inspection (TOCTOU); a positive result must
not be cached as a launch permit. It makes no assertion that the binary is
benign, does not restrict what an eventual subprocess can access, and never
reads process credentials or spawns an executable. External execution still
requires an independent explicit activation and a race-aware launch design.

### Durable one-shot dispatch reservation (no external launch)

`crates/store/src/tool_stdio_dispatch.rs` introduces a move-only
`ReservedStdioToolDispatch`. Its checked reservation API replays the
activated preview from the **current authoritative journal**, rechecks the
conservative Linux executable path inspection and the exact user-approved
`RequireApproval` route, and durably appends a single `RouteDispatched`
event before returning any reservation. Duplicate reservations, unapproved
calls, and absent executables fail without consuming authority.

The recovery projection `replay_unresolved_external_tool_dispatches`
reconstructs dispatched-but-unresolved external calls from historical journal
prefixes and cross-checks their activation and user approval **as of dispatch**,
not against the current session. The desktop Tool call audit now displays
**UNRESOLVED EXTERNAL DISPATCHES**, including exact call, route, provider and
dispatch sequence; there is intentionally no Retry control.

A reserved call has **not necessarily started**. If Chatarium crashes after
the durable dispatch but before a subprocess is launched, or while a
subprocess may have been running, recovery must present an **unresolved**
call. It may not automatically dispatch that same call again or synthesize
an error/result. Only an adapter's correlated terminal observation can
resolve a dispatched call. This is deliberate at-most-once attempt semantics,
not a guarantee of exactly-once side effects.

Archive integrity checking also replays unresolved external dispatches and
their historical permission evidence. A malformed history cannot silently
become a purportedly recoverable tool call.

This milestone is the journal-to-runner handoff; no desktop execution action
has been enabled by it. Before external tool execution is exposed, the Linux
runner must separately enforce race-aware executable identity, process
confinement, bounded runtime and I/O, and a child-outcome audit, rather than
treating reservation or metadata inspection as a process-security proof.

### Explicit provider activation audit (no execution)

`crates/store/src/tool_provider_activation_audit.rs` adds a durable,
checked Activate/Deactivate state machine bound to the *exact immutable*
external stdio configuration sequence. An activation event records an
explicit user-authority decision, not executable trust or permission for any
particular call. Duplicate transitions, activation of an unconfigured
provider, wrong configuration identity, and malformed replay are rejected
before append. Archive integrity replays this audit.

`preview_activated_stdio_tool_invocation` combines the existing exact-call,
route, endpoint, and session preflight with the activation state. It requires
a currently active provider and activation **before the call was recorded**.
Disabling a provider blocks pending calls. Re-enabling it does not revive
old pending calls recorded before the newest activation.

The desktop now has explicit human configuration and activation controls.
In **Context & inference controls → Tools / MCP → External MCP stdio**:
register a uniquely named provider and bind its endpoint, select the provider,
enter an absolute Linux executable path, exact argv (one item per line), and
exact allowed operation names (one per line), then separately save its
**immutable** transport configuration. After inspecting the displayed path,
argv, and allowlist, explicitly choose **Activate**. The background persistence
worker performs conservative Linux executable metadata inspection before
appending the user activation decision. Activation is refused for nonexistent,
symlinked, writable, or otherwise unqualified executables; Windows does not
activate Linux providers. **Deactivate** is an independent durable revocation,
and does not require an executable to remain present.

An approved external ToolCall route exposes an **Activation-aware MCP preview**
showing its exact bounded request or the reason it is ineligible. Previews are
side-effect-free and computed only when expanded in the UI. No external adapter
is permitted to consume a DispatchPermit yet. Linux metadata inspection is
not an authenticity guarantee; a future runner must recheck executable
identity and safety against TOCTOU races immediately before one-shot dispatch.

The configuration/activation UI is intentionally not an arbitrary terminal,
a shell command editor, or an automatic model tool loop. No shell, process,
network, or implicit model call becomes executable through these controls.

### Next transport implementation

The first real transport should remain Linux-native and cost-free. Two
standard options exist:

- **stdio**, which requires an explicitly user-selected, already installed
  local server executable and an audited subprocess-launch policy; no shell
  string interpolation, inherited secret dumping, or implicit provider
  execution;
- **Streamable HTTP**, which requires user-configured endpoint and auth
  material, correct 2026 `Mcp-*` headers and SSE handling. Do not infer
  that a URL or provider name is trustworthy.

Neither transport is currently enabled. Before any network/process action,
persist a user-reviewed provider configuration with an explicit operation
allowlist, apply a bounded timeout and output cap, and require the existing
one-shot explicitly approved `ToolCall` route for each invocation. Interrupted
requests remain ambiguous and must not be blindly retried.

## Builtin hello smoke workflow (Linux-native, no new dependencies)

The small adapter in `apps/desktop/src/local_tool_adapter.rs` reuses the
original shim's exact `hello` semantics: successful output contains
`{"message":"hello"}`. It performs **no filesystem, network, process,
browser, or external MCP actions**.

In the desktop tool area:

1. Manually register a provider named exactly `chatarium.builtin` and bind its
   routing endpoint. The source conversation also needs a current endpoint.
2. Select that provider, enter the `hello` operation, and supply `{}` as
   arguments. Alternatively paste the original `<tool_call name="hello">`
   envelope and click **Read legacy envelope**; this validates/fills the
   operation but preserves the exact pasted text without recording a call.
3. Click **Record call + propose approval route**, inspect the immutable call,
   and explicitly **Allow** the pending ToolCall route.
4. Separately click **Run local hello**. The persistence worker revalidates
   current source/provider addressability, exact provider/operation, arguments,
   route identity, and explicit user permission before it consumes the typed
   one-shot dispatch authority.
5. Inspect the exact terminal `<tool_result>` in **Tool call audit**, with
   route, dispatch sequence, and outcome sequence.
6. To use the result as later model evidence, explicitly click **Admit result**
   on a terminal outcome belonging to the current conversation. The default is
   excluded. **Exclude result** reverses admission without removing the
   original result or its execution audit. Open **Exact next-request context**
   to inspect the user-role provenance envelope before sending.

Any failed pre-dispatch validation leaves the journal unchanged. After durable
dispatch, a crash before outcome is **unresolved**, not automatically retried.
Repeated clicks cannot dispatch the same route twice.

This is a deliberate local smoke test of the control plane, not a claim that
the system hosts arbitrary MCP providers or offers an automatic agent tool
loop. A separate adapter and execution policy is still required for every
future nontrivial tool.

## Tool-result context safety

`crates/store/src/tool_result_context_audit.rs` independently replays
completion, route/provider/session provenance, and local-conversation ownership
at the original outcome event. A later conversation binding cannot retroactively
claim another session's tool result.

Exact tool result text is **not copied into admission events**. Reversible
Admit/Exclude decisions reference the immutable call, route, outcome sequence,
and owning conversation. Results larger than 64 KiB remain inspectable but
cannot enter inference context; this boundary rejects rather than truncates.

The active admitted set is frozen when the user clicks Send. Context Composer
serializes it at user trust level as untrusted evidence with the exact
adapter-output text inside a Chatarium provenance envelope. It never becomes
authored transcript history, instructions, or a privileged tool role. The
feature does not automatically propose or execute any tool.

## Next implementation boundary

A real adapter must establish the following in order:

- an explicit configured provider transport with no implicit executable or
  filesystem authority;
- an adapter binding the recovered, bounded legacy envelope to durable
  provider/call/route identities and correlated responses;
- a persistence-worker dispatch path that revalidates current call/provider
  addressability and the **explicit user-approved RouteGate** immediately
  before consuming exactly one permit;
- a bounded, asynchronous adapter invocation that records either an exact
  result or an exact error, leaving interrupted/ambiguous outcomes unresolved
  rather than retrying state-changing calls blindly;
- preservation of the existing explicitly controlled, provenance-bearing
  tool-result context-admission policy for every additional adapter.

Neither route approval nor call recording is permission to execute before a
trusted adapter is installed and explicitly activated. No arbitrary shell
command execution, automatic model-proposed tool calls, or background tool
execution should be introduced as a shortcut.
