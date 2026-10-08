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
