# Behavior Profile

Status: **ACTIVE**, first executable slice landed 2026-10-06.

Behavior Profile is the first higher-level local policy layer above Context
Composer.

It is intentionally per-conversation, durable, inspectable, and subordinate to
the active Local Inference Contract.

## Current scope

The first profile exposes only three non-default behaviors:

- low reasoning effort;
- low verbosity;
- web search allowed.

These are not generic approximations of the ChatGPT website control surface.
They are the exact request shapes accepted by the first real SIWC capability
run:

- `reasoning: {"effort":"low"}`;
- `text: {"verbosity":"low"}`;
- `tools: [{"type":"web_search"}]`.

No other reasoning or verbosity values are inferred from those successful
probes.

## Durable local ownership

Behavior profiles are stored per `LocalConversationId` in
`behavior-profiles.json`.

Conversation switching restores that conversation's profile independently of
other conversations.

The profile file is included in Chatarium archive backup and restore. It is not
server-owned state and does not depend on a persistent Responses conversation.

## Capability admission

A non-default Behavior Profile value may generate a request patch only when the
active machine-readable Local Inference Contract:

1. is `ready`;
2. matches the currently connected SIWC profile;
3. matches the selected model;
4. marks the corresponding capability `supported`.

If any of those conditions fail, the behavior is blocked. Chatarium does not
silently drop the behavior and send a different request.

The default profile requires no optional capability and therefore generates an
empty request patch.

## Send-time snapshot rule

When a connected local user message is committed, Chatarium snapshots:

- selected model;
- top-level instructions;
- developer context;
- the capability-admitted Behavior Profile request patch.

That snapshot is attached to the pending local inference intent before the
durable-message acknowledgement completes.

Changing the UI after commit therefore cannot mutate the request that will be
dispatched for that already-committed turn.

## Guarded SIWC transport

The pinned Sign in with ChatGPT DevKit currently reconstructs normal Responses
requests with only model, input, instructions, `store:false`, and
`stream:true`.

Chatarium therefore uses its sidecar to apply the admitted Behavior Profile
fields at the actual Responses request boundary while leaving the pinned DevKit
unmodified.

This is **not** a generic raw Responses patch API.

Normal inference currently accepts only these guarded patch shapes:

- `reasoning={"effort":"low"}`;
- `text={"verbosity":"low"}`;
- `tools=[{"type":"web_search"}]`.

Unknown fields or wider shapes fail closed with `invalid_stream_patch`.
The sidecar smoke test permanently checks that forbidden fields remain rejected.

## Inspection

The **Exact next-request context** inspector uses the same Context Composer and
the same Behavior Profile request-patch derivation as live dispatch.

If the profile is admitted, its exact patch is merged into the displayed
request preview.

If it is not admitted, the inspector displays the blocking reason rather than
pretending the request will omit the profile silently.

## Explicitly not implemented yet

Behavior Profile does not yet provide:

- medium/high or other reasoning levels;
- medium/high or other verbosity levels;
- arbitrary tool definitions;
- file or image attachment policy;
- structured-output schemas;
- retrieval or memory policy;
- lifecycle state;
- controller/worker inheritance;
- routing or cross-conversation sharing;
- automatic capability choice.

Those should be added only as typed policy backed by evidence and explicit
product semantics.

## Next boundary

The first local-first **Lifecycle State** slice is now landed and documented in
[LIFECYCLE_STATE.md](LIFECYCLE_STATE.md).

Behavior Profile remains independent of worker lifecycle. A lifecycle
transition does not silently mutate inference controls, and Behavior Profile
does not grant orchestration/routing authority.

The next boundary is explicit local conversation identity bridging into the
existing session/routing plane, followed by visible routing/memory policy.
