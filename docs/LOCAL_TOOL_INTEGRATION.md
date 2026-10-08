# Local tool integration

Status: **ACTIVE SUBSTRATE**. No live MCP or local tool execution is enabled.

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
   The new tool UI is intentionally **inert**: it can prepare, inspect, and
   decide a route but cannot yet send work to a tool adapter.
5. **Terminal adapter-result audit.** `ToolCallOutcomeObserved` is an
   independent exact-text result/error fact. Replay requires the call to be
   correlated to the exact `ToolCall` route and requires that route to have
   already consumed a **user-authorized one-shot dispatch permit**. Results
   are correlated to the immutable call, provider, source session, route
   binding, dispatch event, and observation event. Each call/route has at most
   one terminal outcome.

A tool call with no observed terminal result stays unresolved. A durable route
dispatch does not imply actual execution or success, and a generic
`RouteResultObserved` is not automatically interpreted as an application/tool
result.

The tool outcome replay rejects duplicate outcomes, an outcome before
dispatch, mismatched call/route correlation, missing explicit approval,
oversized results, and blank error bodies. A checked pre-append API validates
the entire prospective result before mutating the authoritative journal.
Archive integrity validates all tool/provider/call/outcome journals.

The new tool outcome audit is a *storage contract for future adapters*. No
desktop action currently pretends to manufacture an adapter observation.
No tool result is silently entered into ordinary transcript, shared memory,
or Context Composer.

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
- a separately user-controlled rule for admitting tool results to inference
  context, at non-privileged trust level with source and call provenance.

Neither route approval nor call recording is permission to execute before a
trusted adapter is installed and explicitly activated. No arbitrary shell
command execution, automatic model-proposed tool calls, or background tool
execution should be introduced as a shortcut.
