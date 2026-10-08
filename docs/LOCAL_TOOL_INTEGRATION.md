# Local tool integration

Status: **ACTIVE SUBSTRATE**. The side-effect-free builtin `hello` adapter
and deliberately confined, user-approved Linux one-shot MCP stdio execution
are enabled. General host-process, filesystem, network, and automatic
model-driven tool execution remain disabled.

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
   Tool calls default to inert. The two deliberately enabled execution paths
   are the side-effect-free builtin `hello` proof and the confined Linux
   external stdio adapter. Both require explicit user Allow and separate
   one-shot Run/Dispatch; no model output grants execution authority.
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
The builtin hello adapter produces a deterministic result; external stdio
providers can run only inside the constrained Linux one-shot sandbox after
activation, user approval, and a separate Run action. No tool result is
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
- explicit refusal of `input_required` multi-round-trip requests: no model
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

### Confined Linux MCP runner (available with explicit desktop approval)

`apps/desktop/src/local_stdio_runner.rs` contains an opt-in, bounded
one-shot executor accepting only a move-only reserved dispatch, using an
exact single JSON-RPC request frame, and decoding the matching MCP reply. The subprocess
executes on its own worker thread, never on the persistence worker. The
terminal observation is returned as a bounded typed message and appended to
the authoritative journal by the persistence worker, which remains responsive
to other writes while the sandbox runs. A failed durable outcome append leaves
the route unresolved; it is **never retried automatically**.

### Linux host-readiness gate

A configured provider and a valid executable path do **not** prove that the
current host permits the required user, PID, mount, and network namespaces.
The desktop therefore performs an additional, real but **non-provider** sandbox
probe before consuming the ToolCall route permit. On a separate, bounded
process thread, Chatarium launches only the fixed root-owned
`/usr/bin/true` through the *same* `prlimit → bwrap` namespace and mount
policy as an actual request, with a three-second deadline, empty environment,
no stdin, and discarded stdout/stderr. Nothing from the configured provider
is launched. The persistence worker remains free to accept unrelated writes.

In **Context & inference controls → Tools / MCP**, the desktop also offers
**Check Linux sandbox host · no tool call**. This user-triggered diagnostic
runs the same harmless probe without requiring any provider configuration,
approval, or tool-call record. The result is a transient status message, not
permission or a durable tool outcome.

If a missing binary, incompatible namespace policy, AppArmor/LSM rule, or
probe timeout prevents the fixed process from succeeding, the desktop reports
a preflight error **without recording RouteDispatched**. If the probe succeeds,
the journal worker revalidates provider activation, call provenance, current
route Allow, exact configuration and executable metadata **again** before
consuming one permit. Probe success is never stored as an authority token;
later process-launch failures still remain possible and are audited under
at-most-once rules after reservation.

On Arch/EndeavourOS, the host tool packages are `bubblewrap` (bwrap) and
`util-linux` (prlimit). Install missing packages through the distribution's
normal package manager (for example, `sudo pacman -S --needed bubblewrap
util-linux`). Chatarium neither installs packages nor disables host security
policy automatically. A user-installed, writable executable outside the
current root-owned `/usr/bin` allowlist is still refused; the probe does
not broaden tool execution authority.

The first hardened launch policy is intentionally narrow:

- Linux only; no Windows implementation or unsafe host-command fallback.
- The provider executable must be a canonical **root-owned /usr/bin**
  regular executable with conservative metadata checks. User binaries
  elsewhere are unsupported by this launcher for now.
- Requires `/usr/bin/prlimit` and `/usr/bin/bwrap`, both verified as
  root-owned executables. Missing binaries or unavailable kernel user
  namespaces cause an error, never unsandboxed execution. These tools
  are free Linux packages, but they are not presumed installed.
- Ubuntu AppArmor policies may block `bubblewrap` network-namespace
  setup with `loopback: Failed RTM_NEWADDR: Operation not permitted`.
  CI loads Ubuntu's targeted `bwrap-userns-restrict` AppArmor profile
  rather than disabling the host-wide namespace restriction or sharing
  the host network. Other Linux hosts must independently support the
  same isolation policy; lack of support remains a hard launch error.
- `prlimit` enforces 512 MiB address space, 8 CPU seconds,
  an 8 MiB per-file limit, 64 open descriptors, 256 processes (subject
  to Linux real-UID accounting), and no core dumps. These are process
  resource limits, not a whole-system cgroup quota. The runner adds a
  10-second wall-clock timeout, a 1 MiB stdout frame ceiling and
  a 16 KiB stderr ceiling.
- `bubblewrap --unshare-all --unshare-user --disable-userns` creates isolated namespaces
  (including network and PID) and denies further nested user namespaces,
  with read-only `/usr`, synthetic `/dev` and `/proc`, a **32 MiB**
  scratch `/tmp` tmpfs, no home mount, cleared environment, a new
  session, and parent-death cleanup. It never constructs a shell command.

**Important limits:** Filesystem metadata checks are not cryptographic
executable attestation; root compromise and race conditions are beyond
this first policy. The runner cannot make arbitrary native code intrinsically
benign. Namespace support depends on the Linux host. GitHub's
Ubuntu CI now runs an actual isolated stdio MCP request/response fixture
after loading a **targeted** AppArmor bwrap profile; it never relaxes the
host-wide AppArmor restriction or enables host networking. Both the
argument/permission boundaries and the live sandbox path are tested,
and the desktop now exposes a separate **Run sandboxed MCP tool · one
shot** button only after activation and the immutable ToolCall's explicit
user Allow. Before reservation, the worker confirms the strict confinement
policy and required binaries. It then durably records RouteDispatched,
shows the unresolved running call, performs the one-shot isolated execution,
and separately journals an exact terminal response/error. Failure to
record an outcome leaves the call **unresolved**. The user must manually
create a new approved call to try again; Chatarium never auto-retries.
The kernel-specific sandbox prerequisites, lack of cryptographic executable
attestation, and container resource-limit constraints remain documented
security limitations.

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
side-effect-free and computed only when expanded in the UI. The
separately clicked **Run sandboxed MCP tool · one shot** action is the sole
external execution control. It rechecks configuration, activation, route
Allow, and sandbox prerequisites on the persistence worker before durably
consuming a single DispatchPermit. No model-generated or automatic execution
path exists. Metadata inspection is not an authenticity proof and remains
subject to executable replacement races.

Configuration, activation, and permission approval alone launch nothing.
Only the separate explicit one-shot Run action can start a process. It is
confined, not a terminal, shell, arbitrary host-command editor, or automatic
model tool loop. The confined server has no host network or home access.

### Transport scope and future extensions

The initial **Linux stdio** transport is available behind the strict isolated
one-shot Run action. It is limited to root-owned canonical `/usr/bin`
executables and requires `bwrap`, `prlimit`, and working namespaces.
Each call requires immutable configuration, an allowed operation, explicit
activation, route Allow, a separate Run click, bounded I/O, and timeout.
Interrupted calls remain unresolved and are never automatically retried.

User-writable provider binaries, arbitrary host subprocesses, provider
network access, and **Streamable HTTP** are **not** implemented.
Any future HTTP adapter requires separately authorized endpoint credentials,
correct MCP 2026 headers and SSE handling, with no inferred trust from a URL
or provider name. No shell command interpolation or inherited secret dumping
is permitted for any future transport.

## Manually approved provider catalog inspection (MCP 2026)

A configured, activated provider can advertise its actual tools using
`tools/list`, but Chatarium must not mistake that untrusted description for
permission to execute anything. A reserved **Chatarium-local operation**
`chatarium.internal.tools-list` maps to the MCP 2026-07-28 `tools/list` RPC,
**not** to a provider tool named `chatarium.internal.tools-list`.

To permit inspection, explicitly include that exact local operation in the
provider's **immutable operation allowlist before configuration/activation**.
The user selects the provider, clicks **Prepare approved tools/list inspection**
(which only fills an editable `{}` draft), then follows the existing
**Record call → Allow → review wire request → Run sandboxed MCP tool** flow.
No process executes before the separately clicked Run. The root-owned
provider runs with exactly the same Linux bubblewrap/prlimit confinement,
bounded I/O, real namespace preflight, durable one-shot reservation, and
audit as any other approved call. Configured tools that omit the local
operation remain ineligible for catalog inspection.

The response must be a correlated MCP `resultType: complete` envelope
containing a `tools` array with at most 128 entries; each tool must have a
distinct bounded name and JSON-object `inputSchema`. Invalid schemas,
duplicate names, malformed JSON or over-budget responses fail closed and
produce an audited adapter error rather than a purported catalog. Catalog
entries and optional descriptions remain **untrusted provider output**.
Chatarium does not evaluate arbitrary JSON Schema or infer tool safety,
install providers, modify the immutable allowlist, create calls for listed
tools, or admit the results into model context.

The desktop **Tool call audit** can display the advertised names and
descriptions separately from the exact immutable response. If the server
includes `nextCursor`, **Prepare next catalog page** only fills a fresh
draft containing the exact opaque cursor. Pagination never happens
automatically: each page requires its **own new user-approved route and Run**.
The server can mutate its advertised list between requests; this catalog
is an observation at one point in time, not a source of authority.

### Conservative schema-assisted argument drafting

Within a completed `tools/list` audit, each advertised tool exposes a
**Review required-field JSON draft** expander. The pure Rust module
`crates/protocol/src/mcp_schema_draft.rs` inspects only bounded structural
parts of the advertised `inputSchema` and produces **editable JSON text**.
It includes *required* properties only, recursively up to a small depth,
using clearly marked placeholders for basic types (empty string, zero,
false, empty array/object). Optional properties are intentionally omitted.
The generator **never uses provider-supplied default, const, enum, example,
description, or expression content as an argument value**. Unsupported
nested schema references/compositions produce an explicit `null` placeholder
and review note; unsupported root schemas, malformed required structures,
excessive size/depth, and missing property definitions are refused.

The separate **Fill editable tool-call draft · no execution** button is only
enabled when the inspected catalog belongs to the **current conversation**,
the source still accepts calls, the provider is **currently activated**, and
the advertised name is already present in that provider's **immutable
configured allowlist**. The generated template does not validate the server's
full JSON Schema, may contain placeholders that the server would reject, and
is **never silently accepted as complete or safe**. Review/edit it in the
ordinary call-intent form; the user must still explicitly **Record** the
immutable call, **Allow** its route, and separately **Run** its one-shot sandboxed
execution. The provider's catalog cannot change execution permissions, and
schema drafting never launches or contacts a server.

## Standalone Linux host compatibility check (no provider or journal)

Before preparing an external MCP call, or when the desktop's **Check Linux
sandbox host · no tool call** diagnostic fails, run this optional check from a
local Chatarium repository checkout:

```sh
cargo run -p chatarium-desktop --bin chatarium-mcp-host-check
```

For reproducible, noninteractive diagnostics, use the structured form:

```sh
cargo run -p chatarium-desktop --bin chatarium-mcp-host-check -- --json
```

`--json` prints exactly one JSON result object on standard output, with
`schema_version: 1`, a fixed `check` identifier, `status` (`ready` or
`not_ready`), a stable `failure_category`, a diagnostic `detail` (null
on success), and explicit false booleans for tool-route consumption,
provider execution, journal access, and host network access. Failure categories
identify the **check stage** (such as untrusted launcher, failed spawn,
timeout, or sandbox exit), not a proven kernel/LSM root cause. A failure
detail can include sanitized, bounded launcher stderr; treat it as untrusted
diagnostic evidence, never commands or permission advice. Exit statuses are
0 for ready, 1 for not ready, and 2 for malformed CLI arguments. `--help`
and rejected flags exit without running the fixture.

It executes only the fixed root-owned `/usr/bin/true` fixture through the
**same production** prlimit + bubblewrap isolation policy. It does not read
Chatarium's conversation journal, authenticate, configure or invoke an MCP
provider, consume any approval, or use the host network. It exits zero for a
successful readiness observation; otherwise it exits nonzero and prints a
bounded diagnostic. When the launcher itself reports a failure, at most
4 KiB of sanitized stderr is used; raw arbitrary process output is not
forwarded. The desktop's **Tools / MCP** panel now retains the latest manual
host-check result until another host check starts. It displays **READY** or
**NOT READY**, the same stable failure category as the standalone CLI, a
fixed human-readable explanation, and a collapsed bounded detail view.
This panel is a transient UI observation, not a journal event, a cached
host permit, or proof that an unrelated provider is executable. Later
provider calls still repeat all preflight, sandbox and route checks.
The desktop's existing check uses the same probe.

Common blockers include missing/untrusted `/usr/bin/prlimit` or
`/usr/bin/bwrap`, disallowed unprivileged user namespaces, and AppArmor/LSM
restrictions. Inspect the reported host-specific cause; **do not disable
system-wide namespace or LSM security merely to make Chatarium pass**.
A passing check only proves the isolation fixture could run at that moment:
it does not attest a provider executable, grant execution authority, or
guarantee that a later launch will succeed. Actual EndeavourOS/Arch host
validation must use Chatarium-owned automated QA on that host; it cannot be
inferred from GitHub's Ubuntu CI. Routine host-regression work is not a
manual operator task, and the principal's personal browser environment
remains outside this diagnostic.

## MCP tools/call result-shape validation

The strict stdio adapter now structurally validates every correlated,
`resultType: complete` `tools/call` response **before** recording it as a
successful tool result. A result needs a `content` array (including an empty
array if applicable), with at most 128 content blocks. Recognized blocks are
text, image, audio, resource links, and embedded text/blob resources; each
must have the corresponding required fields. Optional `isError` must be a
boolean. Arbitrary JSON values in `structuredContent` are preserved and
must not be mistaken for a substitute for the required `content` array.

This is intentionally **bounded structural screening**, not arbitrary JSON
Schema evaluation or verification of resource URI targets, MIME payloads,
base64 bytes, a provider-advertised `outputSchema`, or whether the underlying
tool action succeeded. An MCP tool's own `isError: true` remains visible in
its exact, audited response; it is not silently recast as a JSON-RPC protocol
error. A malformed completed response produces a durable adapter-error
observation after the already-consumed dispatch, not a retry or a context
admission. Tool-produced text and metadata remain untrusted and
context-excluded unless the user separately admits that exact outcome.

## Read-only structured output inspection (MCP 2026)

`tools/list` now retains an optional provider-advertised `outputSchema` in
the typed catalog snapshot. MCP 2026-07-28 permits object, array, scalar,
null, and boolean JSON Schema roots. A malformed `outputSchema` field that
is neither a schema object nor a boolean schema is rejected with the page;
unknown schema *constraints* are preserved as data, never executed.

`mcp_output_inspection::inspect_structured_tool_output` is a **pure,
non-executing comparison** between one inspected `McpListedTool` and one
already-correlated complete `tools/call` result. It distinguishes:
no advertised schema, no `structuredContent`, tool-reported error,
definite mismatch, inconclusive inspection, and passing the explicitly
limited checks. It checks basic JSON types (including bounded `type` unions), required
object properties, nested `properties`, `additionalProperties`,
homogeneous array `items`, Unicode-code-point `minLength`/`maxLength`,
array `minItems`/`maxItems`, and object `minProperties`/`maxProperties`.
A separate bounded **whole-schema preflight** screens every nested schema,
including constraints beneath absent output properties, before any verdict
can pass the supported subset. Unsupported or malformed constraints yield
`Inconclusive`, even if not reached by this particular output. JSON Schema
features such as `$ref`, `$defs`, composition, `format`, and regex
`pattern` remain **unsupported**, yielding `Inconclusive`.
The supported subset now includes bounded `enum` and `const` JSON-semantic
equality (including integer/float equality), plus numeric `minimum`,
`maximum`, `exclusiveMinimum`, and `exclusiveMaximum`. Exact integer
comparisons use 128-bit arithmetic; numeric comparisons involving floating
representations outside the exactly representable integer range (2^53)
remain `Inconclusive` rather than claiming misleading equality or order.
Unsupported or ambiguous comparisons never grant additional authority.
The bounded subset also checks `multipleOf` through checked base-ten
mantissa arithmetic (up to 18 decimal places, with large integers retained
exactly) and `uniqueItems` through deep JSON-semantic equality. Decimal
multiples such as 4.02 / 0.01 do not use floating-point remainders.
Unrepresentable numeric exponents, possible precision loss, oversized
unique-item arrays (more than 16 entries), or exhausted comparison budgets
produce `Inconclusive`. A malformed or nonpositive `multipleOf` divisor
also produces `Inconclusive`, even when hidden under an absent property.
The `uniqueItems: false` constraint is an inert no-op. The bounded subset
also supports `propertyNames`, which applies a schema to every object key,
including keys already declared in `properties`. It supports draft 2020-12
`prefixItems` tuple positions and applies `items` only to any remaining
array elements; without `prefixItems`, `items` still applies to every
element. A tuple schema need not match the array's length unless separately
constrained by `minItems`/`maxItems`. Malformed or empty `prefixItems`
arrays and unsupported nested constraints remain `Inconclusive` even
when their instance branches are not visited. These additions reuse the
existing bounded schema preflight and node-count budgets. Unsupported dialects
and oversized/deep specimens are inconclusive. Only fixed, bounded
diagnostic strings are emitted; arbitrary provider-supplied fields are
not copied into diagnostics.

The audit now identifies the **first recognized failing constraint** with a
fixed, code-owned diagnostic. Messages distinguish `type`, `required`,
`const`, `enum`, string and collection limits, numeric bounds,
`multipleOf`, `uniqueItems`, and directly forbidding boolean-false schemas
under `properties`, `additionalProperties`, `propertyNames`,
`prefixItems`, or `items`. Nested supported failures keep their own
constraint name. No provider-supplied property name, schema value, result
text, or output path is copied into diagnostic messages. A mismatch reports
one detected failure, not an exhaustive error listing; whole-schema
unsupported or malformed assertions still take precedence and yield
`Inconclusive` rather than a misleading specific mismatch.

**Passing supported checks is not full JSON Schema 2020-12 validation.**
The comparison does not perform network requests, fetch resource links,
load reference schemas, execute tools, authorize calls, or admit a result to
inference context. The caller must correlate the exact recorded call,
provider, conversation, catalog observation, and output itself. Catalogs
can change between observations. The desktop **Tool call audit** now exposes a read-only **Compare with
earlier MCP catalog snapshot** expander. Each verdict names its immutable
catalog-observation sequence and source call. The projection only compares
completed catalog observations from the **same provider and source session**,
owned by the same current local conversation and recorded **before** the
target tool call was recorded. Later catalogs cannot retroactively validate
earlier results. Multiple historical catalogs remain separate observations,
never silently replaced by current provider testimony.

The inspection runs only when expanded, leaves the exact journal unchanged,
and cannot grant tool execution, provider activation, approval, or context
admission. Missing, invalid, out-of-scope, or unsupported schema observations
cannot produce a passing verdict. This is a deliberately limited schema-subset
inspection, not full JSON Schema validation or an automatic admission gate.

### Catalog review and large audit histories

The catalog lists outputSchema availability and offers an expandable,
read-only preview of the exact advertised JSON value (maximum 8 KiB displayed).
Truncation is explicit; the recorded tools/list outcome remains intact and
readable in the immutable journal.

The result comparison area shows the four newest candidate catalog snapshots
first, with up to 60 older candidates in a separate collapsed section.
At most 64 candidate snapshots are displayed per result to keep UI work
bounded; omitted earlier history remains in the journal and is explicitly
disclosed. Outcomes are indexed by call identity rather than searched for
every catalog, and call/route/dispatch chronology is rechecked independently.
A result passing supported checks is labeled **NOT FULL VALIDATION**.
No review or expansion action changes execution, permissions, or context.

## Post-execution result review (desktop)

The **Tools / MCP** panel exposes a collapsed **Recent MCP results · separate
context review** queue above historical call audit. This is separate from the
earlier **Next manual MCP call review** that controls execution. Adapter
results and errors are always **excluded from inference context by default**,
even after a successful explicitly approved tool run.

The queue considers at most the 16 newest terminal observations of the
selected provider, verifying the historically owning local conversation of
each source session at outcome time. It checks every matching durable
context-decision identity, and fails the entire queue closed on replay errors,
duplicate decisions, or mismatched correlation. Foreign conversation results
never enter the queue. Undecided results appear first, then admitted,
explicitly excluded and oversized results. Within each status, the latest
observation appears first. Older evidence stays in the historical audit.

Each row is independently expandable and shows the exact call identifier,
observation sequence, outcome kind, context status and provider/session/route.
The associated immutable call identity and dispatch chronology must agree
before any context-decision control is offered. Inspecting an observation
shows only a bounded, UTF-8-safe 4 KiB prefix, labeled if truncated.
A truncated preview cannot authorize admission: the exact outcome must
instead be reviewed through the historical call audit. Observations larger
than the existing 65,536-byte context limit cannot be admitted at all.

**Admit** is an explicit, per-result decision only within a complete preview.
An already admitted result may be explicitly **Excluded** without viewing
its untrusted body. The UI emits at most one individual decision per frame
and delegates to the existing checked, durable journal append path; it
does not write records directly. There is no bulk approval, automatic
admission, tool launch or retry. The full recorded result is never
rewritten or implicitly granted instruction authority.

## Admitted MCP evidence inventory (desktop)

The conversation's **Context & inference controls → Admitted MCP evidence**
panel now lists *all currently admitted* tool results for that local
conversation, across providers, historical source sessions, and beyond the
16-observation per-provider review window. It derives its evidence from the
same fail-closed `replay_admitted_tool_results` projection that normal-turn
Context Composer consumes. The view joins immutable call metadata, verifies
conversation ownership, admission chronology and exact call/route identity,
and refuses to offer revocation controls if the full inventory is inconsistent.

Entries appear newest-admission-first in a bounded-height, virtualized list.
The count and raw UTF-8 byte total cover the **entire** admitted set, not
just visible rows. Every entry identifies the tool operation, call, provider,
adapter result/error, byte size and latest admission event. Each has a
separate **Revoke** action that records a checked, durable Exclude decision
for exactly one result. No bulk action, process execution, journal erasure,
or implicit permission change is possible. The historical Tools / MCP audit
retains excluded outcomes, and the existing per-result review can admit an
eligible outcome again.

These admissions are **eligible for the next normal conversation request**,
not a guarantee that every specialized controller/worker dispatch carries
the same context. **Exact next-request context** remains the authoritative
preview of the composed request. The inventory is a control surface for
audited evidence selection, not an alternative source of model instructions.

## Per-attempt outgoing MCP context evidence

The next-request eligible inventory is deliberately not conflated with what
a particular dispatch path actually sends. Each newly prepared normal Send,
controller coordination, or worker continuation records a **content-free,
per-turn manifest** inside its existing durable `DispatchAttempted` event.
The manifest is derived from the concrete outgoing `ContextPlan`, not a
different UI preview or a later live admission projection.

Every manifest distinguishes the frozen number of admitted MCP results,
the number included in that path's composed input, and the number omitted.
It includes all-source context item/byte totals and up to 32 newest
per-call identities and dispositions; larger result sets retain exact
counts with explicitly truncated detail. The raw adapter results are not
duplicated. Authored Send uses its Send-click snapshot; specialized
controller/worker requests report eligibility from their start-event
prefixes while keeping unrequested MCP evidence out of their input.

The read-only **Recent outgoing context snapshots** inspector under
**Context & inference controls** shows these recorded attempts per local
conversation and whether later acceptance was observed. The data is
frozen per attempt, unlike the live admitted-evidence inventory. Old
dispatches without this format remain unmanifested; no speculative
retrospective reconstruction is attempted.

### Reverse lookup by MCP result

The **Reverse MCP provenance** inspector beside outgoing-request history
accepts a tool call ID (or a recent ID shortcut) and answers which
historical manifested dispatches actually **included** that exact terminal
result in the composed model input. It separately reports explicitly
admitted-but-omitted requests, requests where a complete manifest shows the
result was not eligible, and **unknown** requests where the identity was
outside the manifest's bounded 32-row detail list. Missing entries in a
truncated manifest are never treated as negative evidence.

A **Where used?** control in the conversation-wide admitted-evidence
inventory and a **Find historical use** control in the focused MCP
result-review queue set the reverse inspector's selected call directly.
The selection is transient, scoped by the historically verified owning
conversation, and purely navigational. It cannot admit data or authorize
tool execution. Open **Recent outgoing context snapshots** to inspect the
selection.

The projection checks immutable call/provider/route/session/outcome identity
across requests and excludes other conversations. A different admission
sequence for a later readmission is permitted. Mismatched identity blocks
the read-only lookup. The expandable matching history is paginated and
can be copied as a versioned JSON evidence report, without raw tool or
message text. This is a reconstruction of **manifested local prepared
requests only**; pre-feature attempts, remote receipt and actual model
attention cannot be established by this lookup.

### Omission reasons from the exact composer

New per-request evidence manifests retain a reason beside each listed
result: `included`, `excluded_by_policy`, `omitted_empty`, or
`not_selected_for_dispatch`. The latter is particularly important for
specialized controller/worker requests that intentionally do not import
ordinary admitted MCP results. These are recorded from the precise
outgoing Context Composer inventory; source identity and decision must
match the frozen eligible set before the dispatch is journaled.

Existing v1 manifests lacking a reason remain readable with an explicit
unrecorded reason. Unknown or contradictory reason codes are rejected.
The forward and reverse audit inspectors and their JSON exports present
only the recorded reason, not an invented explanation from today's
conversation context.

### Transport correlation and complete history

The outgoing-context inspector now uses a single journal pass rather than
re-scanning all events for each displayed attempt. Only later typed
observations with matching scope, local turn ID and request ID can establish
acceptance, completion, failure, partial output, or interruption. An
inconsistent correlation blocks the read-only projection; it is never
interpreted as successful remote acceptance. The manifested origin must
independently match its durable authored commit, coordination start or
continuation start and the recorded conversation. The status display
preserves ambiguity after interrupted transport and flags contradictory
terminal evidence instead of selecting a convenient outcome.

A per-conversation **Newer / Older** pager browses the complete available
manifested history in 12-row pages, rather than silently forgetting older
attempts. This has no effect on admission, transport or existing immutable
journal events.

The viewer additionally caches its verified read-only journal projection
transiently while the append-only high-water and selected conversation are
unchanged. This avoids expensive repeated historical ownership and
coordination replay on every UI frame without changing the durable audit.
Appending a journal event invalidates the cache, as does switching local
conversations. Nothing is persisted in the UI cache.

Each attempt also shows its later transport observations in journal order,
with exact event numbers rather than opaque optional values. An explicit
**Copy this audit as JSON** control exports a versioned evidence report with
structured per-call identity, source-session, route, observation and admission
provenance. Raw messages and tool results are never exported through this
control. The report honestly labels the 32-entry per-call detail cap.

`DispatchAttempted` is not proof of service delivery. Manifests describe
Chatarium's prepared input, not server-side storage, use, or a byte-for-byte
cryptographic proof of the bridge's network transmission. They do not
grant execution authority or any new context admission.

## Focused call review (desktop)

Above the bounded historical **Tool call audit**, the desktop surfaces the
newest **actionable, non-dispatched** call for the selected provider. A call
must have a correlated replayed route, and both route and outcome projections
must succeed. Denied calls, consumed one-shot routes, and completed calls
are not presented as new execution tasks. The guide exposes the exact
immutable recorded intent behind a separate expander.

Calls awaiting a user decision expose distinct **Allow** and **Deny**
buttons; neither launches a process. Once a user Allow is recorded, the
same call remains focused, but external MCP **Run** appears only behind
a separate expansion that shows the exact wire request, active-provider
review and confinement-plan checks. The real launcher still repeats the
full preflight and requires the independently consumed route permit.
The side-effect-free builtin hello retains its separate explicit Run
path. Neither workflow auto-advances or retries, and the historical
per-call audit controls remain available.

If a route/outcome projection fails, the focus declines to render an
actionable call; it cannot infer authorization from partially replayed
journal evidence. This focus is UI-only and never changes journal or
transport semantics.

## Selected provider setup guide (desktop)

The **Tools / MCP** panel now shows a read-only next-step guide for the
currently selected provider. It derives prerequisites from replayed durable
provider, transport, activation, and local routing state. In order, it
identifies whether the provider needs a bound routing endpoint, immutable
Linux stdio configuration, explicit activation, or an addressable source
conversation. Once these are in place, it explains the existing separately
approved **Record call → Allow → review → Run once** workflow. The builtin
hello adapter is explicitly distinguished from an external stdio provider.

Optional `chatarium.internal.tools-list` inspection is shown as optional
only if the immutable transport allowlist actually includes it. Changing
catalog advertisements never grants operations. If a required audit cannot
be replayed, the guide declines to declare readiness and directs the user
to the detailed audit errors. The guide has no controls that perform a
transition or call, never launches a provider, never appends to the
journal, and cannot cache a sandbox or route permission.

## External MCP one-shot workflow (strict Linux sandbox)

In the desktop tool area, register a provider and bind an endpoint. Select
it, configure its immutable Linux stdio executable, argv and exact allowed
operations, then explicitly Activate. An executable must be a root-owned,
nonsymlinked file under /usr/bin to be eligible for this restricted runner;
configured paths elsewhere remain inert for execution.

Record the tool call from a locally addressable conversation, review its
immutable arguments, and explicitly Allow its approval route. Expand the
**Activation-aware MCP preview**, read the exact wire request and sandbox
limits, and separately click **Run sandboxed MCP tool · one shot**. If
prlimit/bwrap is missing or untrusted, preflight refuses **before** consuming
the permit. A separate harmless real-process namespace probe additionally
tests the host's current isolation capability before reservation. An
unsupported host fails without consuming approval. The later provider launch
may still fail despite a successful probe (for example, after the host policy
changes); that result becomes a checked Error observation after dispatch
and cannot be automatically rerun. Upon acceptance the journal records
dispatch **before** spawning, the UI shows the permanent dispatch, and a terminal
observation is separately committed. A crash or failed append remains
unresolved, with no Retry control.

Successful tool-result admission to future context remains a separate
explicit action and is disabled by default. This workflow is a bounded
local-process client, not a general executable, network, or filesystem MCP
runtime.

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

## Extension invariants

The current adapter already has durable provider identity, bounded legacy and
native MCP framing, immutable call/route correlation, user-authorized one-shot
dispatch, a bounded asynchronous Linux runner, exact terminal outcome auditing,
and separately authorized context admission. These are prerequisites for any
additional adapter, not features to reimplement as another execution pipeline.

The next compatibility work should prioritize *actual host constraints*:
independent EndeavourOS/Arch namespace tests, user-visible failures, and
supported root-owned provider executables. Expanding to other executable
locations or HTTP transports requires a new reviewed security policy rather
than silently bypassing the existing restricted launcher.

Neither route approval nor call recording is permission to execute without a
trusted, explicitly activated adapter and an individual Run action. Do not add
arbitrary shell execution, automatic model-proposed tool dispatch, or hidden
model-facing tool-result admission as a shortcut.
