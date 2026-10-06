# Local inference capability surface

Status: ACTIVE REFERENCE, 2026-10-05.

This document inventories the local-first inference controls available to Chatarium before higher-level lifecycle/orchestration work.

## Evidence baseline

Chatarium pins `openai/sign-in-with-chatgpt-devkit` at commit `f723814abdccec135b519c451fb6e1992ee5e933`.

Relevant pinned files:

- `packages/local/src/types.ts`
- `packages/local/src/models.ts`
- `packages/local/src/responses.ts`
- `packages/local/src/index.ts`

Relevant Chatarium files:

- `tools/siwc-bridge/bridge.mjs`
- `tools/siwc-bridge/capability-probe.mjs`
- `apps/desktop/src/capability_probes.rs`
- `apps/desktop/src/local_inference_contract.rs`
- `apps/desktop/src/siwc_bridge.rs`
- `apps/desktop/src/main.rs`

The current OpenAI Sign in with ChatGPT documentation was also reviewed on 2026-10-05. Do not infer support merely because a control exists in the general Responses API.

## Core local-first behavior

The current inference path is intentionally local-state-driven:

- Chatarium chooses an account-visible model.
- Chatarium assembles required conversation context locally.
- Server-side response storage is disabled.
- Streaming is required.
- Chatarium persists resulting user/assistant state locally.
- HTTP continuation resends required history in input rather than relying on a persistent server conversation.

Therefore Chatarium owns the most important behavioral control: context construction.

## Capability matrix

Legend:

- AVAILABLE NOW: exposed in the current desktop.
- BRIDGE-READY: already supported by Chatarium's Node bridge but not fully surfaced in Rust/UI.
- DEVKIT-READY: supported by the pinned DevKit but not fully carried through Chatarium.
- ROUTE-DOCUMENTED: OpenAI documents the capability for this plan-usage route, but the pinned wrapper used by Chatarium does not expose it.
- UNSUPPORTED: explicitly unsupported for this route.
- PROBE-READY: not established for this route, but a credential-safe authenticated probe is available from desktop Diagnostics and the developer CLI.
- UNKNOWN: not established by the pinned SDK or the current route documentation reviewed here.

| Capability | Status | Notes |
| --- | --- | --- |
| Account-specific model discovery | AVAILABLE NOW | Runtime catalog, not a hardcoded model list. |
| Model selection | AVAILABLE NOW | Desktop picker exists. |
| Text user input | AVAILABLE NOW | End-to-end. |
| Multi-message context input | AVAILABLE NOW | Current desktop sends local transcript context. |
| user role | AVAILABLE NOW | Supported. |
| assistant role | AVAILABLE NOW | Supported. |
| developer role | AVAILABLE NOW | Per-conversation Developer context is inserted as the first developer-role input message. |
| Top-level instructions | AVAILABLE NOW | Per-conversation Instructions editor persists locally and is sent through the top-level field. |
| Streamed text deltas | AVAILABLE NOW | End-to-end. |
| Completed-response signal | AVAILABLE NOW | Required for success. |
| Stop/cancel active inference | AVAILABLE NOW | Desktop Stop generation targets the active request through a bridge-owned AbortController. |
| Image input | ROUTE-DOCUMENTED | Documented when selected model accepts it; current pinned wrapper is text-only. Developer probe: `image_input`. |
| File input | ROUTE-DOCUMENTED | Documented when selected model accepts it; current pinned wrapper is text-only. Developer probe: `file_input`. |
| Function/custom tools | ROUTE-DOCUMENTED | Documented for the route; not exposed by current pinned wrapper. Developer probe uses a namespaced function tool. |
| additional_tools input items | ROUTE-DOCUMENTED | Documented for the route; not exposed by current pinned wrapper. Developer probe: `additional_tools`. |
| Web search | ROUTE-DOCUMENTED | Subject to model/account/workspace policy; not exposed by current pinned wrapper. Developer probe: `web_search`. |
| Audio/video input | UNSUPPORTED | Explicitly unsupported. |
| Image-generation tool | UNSUPPORTED | Explicitly unsupported. |
| File-search tool | UNSUPPORTED | Explicitly unsupported. |
| Code Interpreter | UNSUPPORTED | Explicitly unsupported. |
| Native computer use | UNSUPPORTED | Explicitly unsupported. |
| Hosted MCP/connectors | UNSUPPORTED | Explicitly unsupported. |
| Responses tool_search | UNSUPPORTED | Explicitly unsupported. |
| Persistent Responses conversation | UNSUPPORTED | Local context ownership is required instead. |
| HTTP previous_response_id continuation | UNSUPPORTED | Required history must be supplied in input. |
| Server-side response storage | UNSUPPORTED BY DESIGN | store=false is required. |
| Non-streaming HTTP inference | UNSUPPORTED BY DESIGN | stream=true is required. |
| temperature | UNSUPPORTED | Explicitly unsupported. |
| top_p | UNSUPPORTED | Explicitly unsupported. |
| top_logprobs | UNSUPPORTED | Explicitly unsupported. |
| max_output_tokens | UNSUPPORTED | Explicitly unsupported. |
| max_tool_calls | UNSUPPORTED | Explicitly unsupported. |
| background | UNSUPPORTED | Explicitly unsupported. |
| metadata | UNSUPPORTED | Explicitly unsupported. |
| prompt | UNSUPPORTED | Explicitly unsupported. |
| prompt_cache_retention | UNSUPPORTED | Explicitly unsupported. |
| safety_identifier | UNSUPPORTED | Explicitly unsupported. |
| truncation | UNSUPPORTED | Explicitly unsupported. |
| top-level user request field | UNSUPPORTED | Explicitly unsupported. |
| moderation | UNSUPPORTED | Explicitly unsupported. |
| multi_agent | UNSUPPORTED | Explicitly unsupported. |
| Explicit system-role message item | UNSUPPORTED | Use instructions or developer messages instead. |
| Reasoning controls | PROBE-READY | General Responses supports `reasoning.effort`; SIWC route acceptance is not yet established. Developer probe: `reasoning`. |
| Text verbosity control | PROBE-READY | General Responses supports `text.verbosity`; SIWC route acceptance is not yet established. Developer probe: `verbosity`. |
| Structured-output controls | PROBE-READY | General Responses supports `text.format`; SIWC route acceptance is not yet established. Developer probe: `structured_output`. |

## Exact pinned inference interface

The pinned wrapper exposes:

- `model`
- `input`
- optional `instructions`
- optional cancellation signal
- optional streamed-text callback

Its text message type exposes:

- `user`
- `assistant`
- `developer`

with string content.

The wrapper fixes server storage off and streaming on.

## Authenticated route probe harness

The pinned DevKit and current upstream DevKit still validate only text
user/assistant/developer messages. Chatarium does not widen or fork that public
wrapper merely to discover route behavior.

The desktop Diagnostics runner and `tools/siwc-bridge/capability-probe.mjs`
drive the same guarded `probe_response` command inside the trusted Node
sidecar. The sidecar keeps OAuth acquisition/refresh entirely inside the
official DevKit, temporarily patches only the outgoing `/v1/responses` JSON
body, preserves the fixed `model`, `store: false`, and `stream: true`
fields, then restores the original fetch immediately.

`apps/desktop/src/capability_probes.rs` owns the fixed in-app matrix,
classification state, sequential dispatch, and sanitized local result ledger.
Probe output is diagnostic state only and is never inserted into conversation
context.

The patch allowlist is deliberately narrow: `input`, `reasoning`, `text`,
and `tools`. Probes cannot overlap normal inference.

Named probes now exist for:

- baseline text admission;
- image input using an inline data URL;
- file input using an inline tiny text file;
- namespaced function tools;
- `additional_tools` input items;
- web search declaration;
- `reasoning.effort`;
- `text.verbosity`;
- structured output through `text.format`.

A successful probe establishes support only for the selected local SIWC profile
and model at the time of the run. Chatarium binds the saved report to the
DevKit's renderer-safe `profileId`, the model slug, and the run timestamp; it
does not persist the account email in the probe report. A rejected probe remains
evidence, not a reason to invent product support.

## Derived Local Inference Contract

The sanitized probe report remains the empirical evidence source. Chatarium now
derives `local-inference-contract.json` from that report rather than treating a
second hand-maintained capability table as authority.

The contract separates:

- fixed Chatarium guarantees such as local context ownership, conversation
  isolation, `store:false`, streaming, supported text roles, top-level
  instructions, and cancellation;
- fixed transport constraints such as no persistent Responses conversation and
  no `previous_response_id`;
- profile/model/timestamp-scoped empirical probe classifications.

Contract states are:

- `ready` — the report is profile/model/timestamp scoped and every named probe
  has conclusive `supported` or `unsupported_route` evidence;
- `needs_review` — every probe reached a terminal response but at least one is
  merely `rejected`, which may reflect a malformed/obsolete probe shape rather
  than a genuine unsupported capability;
- `incomplete` — scope is missing or at least one probe is missing,
  `not_run`, `error`, or `model_unavailable`.

Downstream behavior must not consume empirical capabilities unless the typed
contract loader reports `ready_for(current_profile, current_model)`. See
[LOCAL_INFERENCE_CONTRACT.md](LOCAL_INFERENCE_CONTRACT.md).

## Current Chatarium implementation

The first local-first control milestone is now implemented.

The desktop supports:

- a fixed in-app SIWC capability probe suite under Diagnostics;
- explicit in-app disclosure that probes use the existing local sign-in, keep credentials inside the SIWC bridge, and consume small real plan-usage requests;
- sanitized local probe-result persistence to `siwc-capability-probes.json`;
- automatic reload of the last saved probe matrix across desktop restarts;
- profile + model + timestamp scoping with visible stale-profile/stale-model warnings;
- inclusion of the sanitized probe report in local archive backup/restore;
- deterministic derivation of `local-inference-contract.json` after probe completion and from saved evidence on startup;
- typed contract loading with profile/model freshness gating;
- inclusion of the derived inference contract in local archive backup/restore;
- normal Send blocking while a capability probe is active, preserving the one-Responses-request probe invariant;
- multiple isolated local conversations with durable active selection;
- create, switch, rename, archive, and restore;
- per-conversation draft isolation;
- per-conversation persisted model selection;
- per-conversation top-level instructions;
- per-conversation developer-role context;
- active response cancellation / Stop generation;
- an Exact next-request context inspector;
- local persistence of workspace metadata and inference settings;
- inclusion of those local metadata files in archive backup/restore.

The request inspector renders the request-shaping state Chatarium controls: selected model, optional instructions, developer context plus transcript input, and the fixed store=false / stream=true semantics.

Repository-wide validation is green on both Windows and Linux through commit `3e3f9f32838f097829d2b5436146fb089f897aa5`: bridge syntax/smoke, formatting, compilation, protocol corpus validation, Linux desktop tests, and the full Windows workspace tests all pass. That validated head includes durable probe reload, profile/model scoping, backup/restore preservation, and the Windows archive-restore handle fix.

## Local behavioral levers Chatarium owns

These can be explored without more remote protocol work:

- which local conversation's history is supplied;
- exactly which messages are included;
- transcript order;
- explicit developer messages;
- top-level instructions;
- local summaries replacing older raw turns;
- local memory/context artifacts inserted into selected requests;
- per-conversation default model;
- per-conversation instruction profile;
- worker/controller role context;
- lifecycle state represented as explicit developer context;
- routed messages between local conversations;
- context handoff between successor sessions;
- local tool results inserted back into context;
- provenance and policy-controlled context inclusion.

One local conversation must not acquire another conversation's state implicitly.

## Immediate capability backlog

The first known-control exposure set is complete: instructions, developer context, stop/cancel, per-conversation model persistence, conversation isolation, and request inspection are now user-controllable.

The route-documented and previously unknown capability probes are now executable from the desktop Diagnostics panel without widening the product API or exposing credentials. The same fixed suite also remains available through the standalone developer CLI.

Next run the in-app authenticated probe suite against the selected account-visible model. Chatarium will persist the sanitized evidence and derive the Local Inference Contract automatically.

Then follow the contract state:

1. `ready` — freeze that profile/model contract and decide which supported capabilities deserve first-class product exposure;
2. `needs_review` — inspect the rejected probe shapes and current route schema, fix the engineering probe if necessary, and rerun without converting that work into principal-operated terminal QA;
3. `incomplete` — diagnose the missing/error/model-unavailable evidence and keep lifecycle/context-composer consumers blocked.

Lifecycle/context-composer work may depend on empirical remote capabilities only after the contract is `ready` for the active profile/model.

## Completion criterion

Before elaborate lifecycle behavior, every desired control should be classified as:

- exposed and user-controllable;
- supported but awaiting implementation;
- explicitly unsupported;
- empirically rejected;
- or unknown with a named probe required.

Behavioral design should build on that known substrate rather than rediscovering inference constraints mid-project.
