# Deferred A2A agent interoperability and external supervision

Status: **FUTURE DESIGN ONLY — NOT ACTIVE OR IMPLEMENTED** (2026-10-25).
Owner: Chatarium local-first supervisory control plane.

**Scheduling gate:** Do not begin A2A implementation until the existing
[OpenViking first live QA](OPENVIKING_FIRST_LIVE_QA.md) passes its complete
normal-turn checkpoint -> exact write/read-back -> restart -> verified recall
acceptance. An MCP discovery PASS alone does not clear this gate. The
[OpenViking memory lifecycle](OPENVIKING_MEMORY_LIFECYCLE.md) is not replaced,
refactored or duplicated for this feature. The principal must explicitly
activate this future track after the gate; completing the gate by itself does
not authorize A2A implementation.

## Goal and non-goals

Let Chatarium act as a **supervisory A2A client** for independently deployed
agents that already have their own execution harnesses. Coordinate work,
observe exposed status, admit evidence, and intervene through permitted
interfaces. Add an optional A2A server only if a later demonstrated use case
requires other agents to call Chatarium.

This does **not**:
- replace OpenViking with an A2A memory agent or create another memory store;
- replace the remote agent's internal harness or grant Chatarium control over it;
- make MCP and A2A interchangeable: MCP exposes tools/context capabilities,
  while A2A exchanges agent messages, tasks, capabilities and artifacts;
- assume hosted ChatGPT has a native A2A endpoint or exposes hidden reasoning,
  hidden tool calls, internal state transitions, or a programmable agent loop;
- change the current local-first inference / Sign in with ChatGPT transport,
  re-activate paused browser transport work, or introduce a paid API dependency;
- authorize remote memory transfer, automatic dispatch, or unattended execution
  simply because an Agent Card advertises a capability.

## Planned topology

```text
User / egui supervisory UI
             |
Chatarium durable controller (policy, provenance, task/worker lifecycle)
             |                           |
             |                           +-- existing OpenViking integration
             |                               (scoped memory/retrieval)
             |
     approved route gate + one-shot dispatch
             |
     A2A boundary (Rust SDK adapter)
             |
     +------------------------+--------------------------+
     |                        |                          |
  native A2A peer      another native A2A peer      optional adapter
  (external harness)    (external harness)         to hosted ChatGPT
                                                     only via a verified,
                                                     permitted interface
```

OpenViking remains Chatarium's permanent context/memory infrastructure for
this architecture. Chatarium's append-only journal remains the authority for
its local conversations, routing decisions, task correlation, permissions, and
observed outcomes; OpenViking does not become the execution ledger. Remote
agents may use whatever memory/harness they independently own. Remote content
is evidence, never instructions with elevated local authority.

## Protocol / SDK choice (revalidate when work begins)

Candidate: official [A2A Rust SDK](https://github.com/a2aproject/a2a-rs),
targeting the upstream [A2A specification](https://github.com/a2aproject/A2A/blob/main/docs/specification.md).
Begin with the published `a2a-client-lf` and `a2a-lf` crates (Cargo aliases
`a2a-client` and `a2a`). `a2a-server-lf` is a **later optional** inbound
surface, not a requirement for consuming remote agents. Prefer the smallest
supported transport/binding (HTTP+JSON / JSON-RPC, with streaming only when
advertised); do not implement gRPC, push/webhooks, or a second gateway by
default. At implementation time pin a tested SDK version/commit and protocol
revision, verify licenses, build requirements, and interoperability against
an independently implemented A2A peer. Today's candidate is not a dependency
decision for the active build.

A2A provides Agent Cards/discovery, messages, tasks, task status, artifacts and
optional update delivery. It does **not** implement Chatarium's supervisor,
verification rules, checkpoint policy, durable workflow, or user authorization.

## Chatarium identity / state mapping

Keep the identity domains in [local orchestration identity](LOCAL_ORCHESTRATION_IDENTITY.md)
separate. An external A2A Task ID is **not** a `WorkerId`, `SessionId`,
`ChatContainerId`, `LocalConversationId`, or ChatGPT remote conversation ID.
Add an explicit durable correlation binding with remote endpoint identity,
task/context identifiers, requesting local conversation, admitted route ID,
protocol revision and last evidenced status. Never infer a binding from text.

Planned interaction:

1. Discover an explicitly configured peer, obtain its Agent Card, validate
   endpoint identity / URL / authentication and record capabilities. Discovery
   is not consent to execute, fetch arbitrary URLs, or expose local data.
2. A user or authorized controller proposes a goal as a Chatarium route.
   Existing `RouteGate` allow/deny/Ask rules and one-shot permission apply
   **before** any outbound agent request.
3. Freeze the explicitly admitted request context. Scoped OpenViking retrieval
   may inform that request only under existing local conversation, provenance,
   size, privacy and cross-conversation admission rules; no automatic export
   of raw journals, credentials, memories or unrelated project context.
4. Record the outbound attempt durably before dispatch. Correlate the
   A2A response/Task ID to that attempt and source peer; never equate an
   accepted request with delivered execution or completed work.
5. Observe supported states by task polling, or streaming if advertised.
   Preserve status, source timestamps, artifacts, and transport uncertainty
   in the local journal. Unsupported remote operations remain unavailable.
6. Verify delivered artifacts independently against the original goal's
   acceptance evidence (e.g., repository commit, tests, reproducible output),
   subject to actual access. A remotely reported `completed` state is not
   Chatarium's verified `goal completed` state.
7. Continue, request input, cancel, or escalate only through supported
   remote capabilities, fresh goal correlation, valid policy and bounded
   continuation authority. User veto always wins.

Suggested local outcome distinctions: `Proposed`, `AwaitingApproval`,
`Dispatching`, `DeliveryUnknown`, `RemoteWorking`, `NeedsInput`,
`RemoteTerminalUnverified`, `Verified`, `Blocked`, `Failed`.
These are **design vocabulary**, not a claim that A2A itself defines these
states. Build a precise mapping from upstream Task states rather than
overloading existing Chatarium worker lifecycle types.

## Hosted ChatGPT boundary

The current Chatarium hosted-ChatGPT/Sign-in-with-ChatGPT interaction is
**not presumed to be an A2A server**. Do not advertise that a ChatGPT
conversation can be controlled through A2A. Any future ChatGPT-facing
adapter would translate only commands/results that a *separately validated
and permitted* conversation interface actually exposes. The adapter can
publish an A2A view of Chatarium-owned task state, not fabricate remote
agent acknowledgements, remote task IDs, completion, or hidden tool evidence.
When the necessary remote capability is unavailable, return a typed
`Unsupported` or `Unknown` outcome and preserve local work without
simulating success. No bypass of auth, rate limits or service protections.

## Reliability, security, and QA invariants

- Fail closed on malformed/untrusted Agent Cards, URL redirects, unknown
  protocol revisions, invalid status transitions, mismatched task ownership
  or permissions. Restrict peer discovery and arbitrary network targets;
  prevent SSRF and local-network access via untrusted peer metadata.
- Authenticate configured peers; never print/store bearer credentials or
  secrets in public protocol fixtures. Keep connection material outside
  version control.
- Treat messages, Agent Cards, files and artifacts as untrusted data; never
  execute returned instructions or elevate model-produced policy changes.
- Persist request intent, approvals, outbound attempt identity and observed
  response separately. Timeout after a possibly accepted remote mutation is
  `DeliveryUnknown`, **not** `Failed`; reconcile by supported task ID or
  protocol-level idempotency before any replay. No blind retry/duplicate goal.
- Bound concurrency, polling/retries, timeouts and result sizes. Support
  cancellation only when the peer advertises and actually honors it. Remote
  status cannot silently renew a Chatarium continuation lease.
- Keep network calls, parsing and verification off the egui rendering thread.
  Local-only conversations and existing OpenViking checkpointing must work
  identically with A2A disabled or unavailable.
- Test with a synthetic A2A peer and at least one independently implemented
  peer; verify task lifecycle, multi-turn input requests, unsupported
  streaming, ambiguous delivery/restart, authorization failures, replay,
  explicit user veto, memory isolation, artifact evidence, and downgrade
  protection. No principal-operated routine QA.

## Proposed implementation slices (all blocked, not started)

- **A2A-0 — prerequisite:** finish host-specific OpenViking checkpoint,
  exact read-back, restart and recall acceptance; preserve current
  orchestration/reliability gates.
- **A2A-1 — specification/ADR:** revalidate A2A protocol and Rust SDK;
  choose binding, pin version, enumerate peer capabilities and threat model.
- **A2A-2 — opt-in outbound client:** local mock peer first; explicit Agent
  Card configuration, credentials, route approval and typed errors.
- **A2A-3 — durable task correlation:** separate external task identity,
  status/artifact journaling, polling/optional SSE, idempotency and recovery.
- **A2A-4 — supervisory evaluation:** evidence-based verification, bounded
  continuation, needs-input, interruption, user-visible egui controls.
- **A2A-5 — interoperability:** independent A2A peer test matrix; optional
  ChatGPT adapter only after its real supported surface is demonstrated.
- **A2A-6 — optional inbound server:** expose an authenticated, minimized
  Chatarium A2A Agent Card / endpoint only with explicit user authorization
  and demonstrated need.

**Exit criterion (future):** One authorized remote A2A agent receives a
user-approved task, reports an observed lifecycle, returns an attributable
artifact, survives an interrupted/restarted Chatarium session without
duplicate dispatch, and undergoes independent acceptance verification.
With A2A switched off, all existing local conversations, OpenViking memory,
and MCP integration behave unchanged.

## Related contracts

- [Roadmap](ROADMAP.md)
- [Product vision](PRODUCT_VISION.md)
- [Local-first exploration](LOCAL_FIRST_EXPLORATION.md)
- [Local orchestration identity](LOCAL_ORCHESTRATION_IDENTITY.md)
- [OpenViking memory lifecycle](OPENVIKING_MEMORY_LIFECYCLE.md)
- [OpenViking first live QA](OPENVIKING_FIRST_LIVE_QA.md)
- [Local tool integration](LOCAL_TOOL_INTEGRATION.md)
