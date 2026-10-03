# Roadmap

Chatarium is ordered by reliability value, not by visual completeness.

## P0 — stop losing work

The immediate objective is a browser-side flight recorder that can be deployed before the native client is ready.

Deliverables:

- autosave composer mutations locally;
- commit the exact outgoing user text before submit;
- retain conversation identity/URL with each local record;
- snapshot assistant output incrementally;
- journal reloads, disconnects, visible errors, and completion observations;
- export/recover a conversation-independent local transcript;
- never store reusable authentication secrets in exported evidence.

Exit criterion: a browser/network failure may interrupt a turn, but it cannot erase text already authored or already observed on the machine.

## P0.5 — establish the protocol baseline

Create controlled observation sets of the official ChatGPT web client while minimizing human QA.

The protocol baseline now has two complementary empirical snapshots:

- `2026-09-29.001`: manual Edge/Linux HAR of a completed text turn, establishing prepare/send/token-linkage/persisted-state and websocket completion surfaces;
- `2026-09-29.002`: canonical C03 captured by Flight Recorder, establishing the actual `text/event-stream` body, `delta_encoding = "v1"`, incremental message operations, explicit completion patching, `message_stream_complete`, and terminal `[DONE]`.

Flight Recorder v0.6.0 then passed live protocol-backed reconciliation: a parsed user `input_message` confirmed the pending send under the canonical conversation identity and the final-channel assistant stream populated durable assistant state without depending on current DOM selectors.

Manual/browser-local evidence remains acceptable when it produces protocol knowledge faster and more safely than finishing legacy capture automation first.

The legacy Windows CDP harness remains documented in `docs/CAPTURE_HARNESS.md`, but it is no longer a prerequisite for protocol progress. The active cross-platform path is:

```text
Flight Recorder export
        ↓
chatarium-recorder snapshot-flight
        ↓
sanitized selected-run evidence + structural inventory
        ↓
protocol snapshot / diff / typed interpretation
```

Longer-term automation should preserve the useful harness properties:

- dedicated Chatarium Edge profile with one-time normal login;
- read-only diagnostics (`chatarium-capture doctor`);
- automated canonical experiment execution;
- incremental private capture journal;
- reusable centralized sanitization;
- one portable sanitized artifact per run;
- explicit ambiguous-outcome semantics;
- no automatic retry after a mutating experiment becomes ambiguous.

Canonical experiments:

1. clean page load with no action;
2. conversation list load;
3. open an existing conversation;
4. create a new conversation;
5. send a deterministic text prompt;
6. observe a complete streamed response;
7. stop generation mid-stream;
8. regenerate/retry;
9. edit or branch from a previous user turn where supported;
10. upload a tiny non-sensitive text attachment;
11. switch model/settings where exposed;
12. repeat selected experiments under an intentionally interrupted connection.

The first machine-executable experiment definitions live under `protocol/experiments/`. Each experiment should produce its own capture or clearly delimited action log. Human QA is reserved for observations the harness genuinely cannot obtain itself.

Current state: Flight Recorder v0.7.1 has an explicit, bounded protocol-read evidence mode that is off by default and captures only explicitly armed same-origin `GET`/`HEAD` responses under `/backend-api/` with JSON-family content types. Query values, request headers, cookies, authorization values, request bodies, browser storage, and third-party traffic are not captured. Each armed interval has isolated run identity and counters; legacy v0.7.0 evidence is bounded Arm-through-Disarm so late responses cannot contaminate a later or public run. Offline `snapshot-flight` reduces private read bodies to deterministic object/array/key/type structure and stable identity placeholders before public evidence is written; malformed declared JSON fails closed and truncation remains explicit.

Controlled C01 snapshot `2026-09-30.001` is now committed. It observes the official client using a paginated `/backend-api/gizmos/snorlax/sidebar` JSON surface during sidebar loading and scrolling, with nested `conversations.items` and nested/top-level cursors. This is real conversation-list-side evidence, but it does **not** prove that the surface enumerates the complete account-wide remote conversation set. Therefore `LATEST_VALIDATED_CONVERSATION_LIST_OBSERVATION` deliberately remains `None` and conversation-list compatibility remains `NoBaseline`. C02 now has both the historical rate-limited snapshot `2026-09-30.002` and a successful baseline snapshot `2026-10-01.001`: the official client issued `GET /backend-api/conversations/<id>` with `include_has_versions` and `num_turns`, and the successful run returned HTTP 200 JSON containing a conversation envelope with a `messages` array and `page_info`. The public fixture preserves only value-minimized structural shape. `LATEST_VALIDATED_CONVERSATION_FETCH_OBSERVATION` is now `2026-10-01.001`; C01 remains `NoBaseline`. The evidence-gated `crates/protocol::conversation_fetch` parser accepts only that named revision, verifies optional remote-identity correlation, parses the observed message/content/page envelope, and preserves observed `metadata.parent_id` links without inferring missing ancestry. `crates/store::remote_mirror_snapshot_audit` now imports an already-fetched validated C02 body into the append-only journal only when durable selection/readiness evidence matches the bound remote identity and exact read-observation provenance. The private response body is retained exactly in local durable state while public protocol fixtures remain sanitized; replay reparses the body against its named revision, duplicate identical imports are idempotent across restart, and SQLite remains a rebuildable projection rather than authority.

Local and remote conversation identity are separate first-class domains, with restart-safe one-to-one binding that preserves the protocol observation revision supporting the correlation. Publication-safe read fixtures can now be imported into the durable audit through an explicit-index bridge: the importer refuses to guess which captured request has semantic meaning, archives the sanitized fixture by content hash, and records the fixture SHA-256/index alongside the typed observation. Safe read observations are first-class typed durable records: Chatarium can persist controlled experiment identity, named protocol revision, GET/HEAD metadata, normalized backend path, query-key names, status/content type, truncation/body presence, and optional structural JSON top-level type without persisting raw response text. Flight Recorder v0.7.2 adds an evidence-only exception for C02 query values: occurrence order and duplicates may be retained only for `include_has_versions` and `num_turns`, and only as empty/lowercase-boolean/bounded-decimal literals; unsupported shapes are represented as redacted markers. The typed read model and durable V3 audit preserve that distinction while v0.7.0/v0.7.1 observations replay with query values explicitly unknown. The operator's 2026-10-03 Edge/Linux HAR then established the current exact safe C02 literals and order as `num_turns=10&include_has_versions=true`; snapshot `2026-10-03.001` preserves that evidence without committing the raw HAR. A composed remote-mirror readiness layer joins durable local/remote identity provenance to C02 conversation-fetch evidence and refuses semantic import unless the binding revision has one unambiguous `ValidatedAgainst` C02 observation. Durable user selection intent is restart-safe and separate from readiness. The authenticated-session boundary is typed and explicitly transient: a selected, protocol-ready mirror plan stops at `RequiresAuthenticatedSession` until a borrow-scoped runtime provider reports positive authentication evidence. The lease contains no credential representation, is not durable or clonable, and revalidates authentication before every provider use. `crates/store::remote_mirror_runtime` now composes those gates with a mechanism-agnostic `RemoteConversationFetchProvider`: unselected or blocked conversations never probe the provider, stale authentication fails before the fetch call, the provider receives only the exact durable remote identity plus validated protocol revision, and any returned private JSON body passes through the independently gated durable snapshot importer. Provider, authentication, parser/import, and durable-history failures remain distinct and there is no automatic retry.

Exit criterion: Chatarium can mirror selected existing conversations into local durable state and explain incompatibilities against a named protocol snapshot.

### P3 browser-integration reset after 2026-10-03 incident

The store/protocol side of live mirroring remains valid: exact remote identity, C02 parsing, durable snapshots, active-branch projection, and offline cached mirrors are retained.

The browser-integration history now has four explicitly separated generations.

**Tampermonkey** is retired from the critical runtime path. It produced useful protocol evidence and one successful browser↔desktop roundtrip, but repeated live QA exposed unstable transport/execution-world behavior and inadequate first-party request-context parity.

**Edge Bridge 0.1/0.2 exact-request replay** is also retired. 0.1 proved that a purpose-built MV3 extension could reliably cross the desktop/tab/MAIN/auth/account boundaries, but its reconstructed ordinary-history request returned an unconfirmed empty result. Re-auditing the private HAR showed that the frozen global `/backend-api/conversations?...limit=20&offset=0` specimens were HTTP 429, so they proved request shape/context rather than successful global-history semantics.

0.2 was the one allowed evidence-driven correction. It waited for the exact first-party frozen C01 request and intended to replay observed application context. The final live validation failed both before and after a full tab reload with `tab=yes`, `account-context=yes`, `request-context=no`, `first-party-http=unknown`, `context-headers=0`, and `first_party_request_context_unavailable`. Per the human-QA stop rule, #102 was closed and exact-C01 replay was retired.

**Edge Bridge 0.3 CDP discovery** replaced request replay with current first-party observation.

During one bounded discovery command it:

1. attaches `chrome.debugger` only to the selected ChatGPT tab;
2. enables the CDP Network domain;
3. reloads the ChatGPT tab itself;
4. observes actual current first-party `/backend-api/*` traffic;
5. reads successful list-like JSON responses with `Network.getResponseBody`;
6. extracts bounded typed conversation summaries and structural cursor metadata;
7. detaches the debugger.

### 0.3 live validation: history discovery works

The first 0.3 target-browser validation succeeded.

The extension visibly entered its bounded debugging session and Chatarium populated the sidebar with **85 real conversation summaries/titles** observed from the first-party traffic produced during that reload window.

This is the first live proof that Chatarium can automatically discover the user's current ChatGPT conversation surfaces without reconstructing or waiting for a frozen private endpoint.

It is **not** proof that 85 is the account-wide total. The old UI rendered `85/85` because an unknown total fell back to the observed count; that was a presentation bug. The active UI now labels the set as `85 OBSERVED` and keeps account-wide coverage explicitly unknown. `ConversationList` remains `NoBaseline` until pagination/completeness semantics are separately established.

The live run also exposed two downstream observations:

- clicking a discovered conversation caused the older synthetic exact C02 fetch to time out;
- one OS-level “Application Not Responding” dialog appeared for Chatarium during the browser-debugging interval.

Neither observation invalidates the history-discovery success. They are separate boundaries.

**Edge Bridge 0.4 CDP mirroring**, tracked by #104, removes the remaining synthetic exact-read step.

For one discovered remote conversation it:

1. keeps the user's active ChatGPT tab untouched;
2. creates a temporary background tab;
3. attaches CDP before target navigation;
4. enables the Network domain;
5. navigates the temporary tab to `https://chatgpt.com/c/<remote-id>`;
6. observes the exact first-party `/backend-api/conversations/<remote-id>` response generated by ChatGPT itself;
7. preserves actual first-party HTTP/content-type evidence;
8. reads the completed body through `Network.getResponseBody`;
9. applies body-size and JSON bounds;
10. passes the body through the existing exact C02 remote-identity/parser gate;
11. durably commits the local mirror;
12. detaches and closes the temporary tab on every outcome.

The independently evidenced C02 semantic profile remains `2026-10-03.001`. 0.4 changes the capture mechanism, not the right to infer new response semantics.

History discovery and mirror state are now separate UI domains. A failed individual mirror no longer turns successful history discovery into a failed bridge. Per-conversation labels distinguish discovered, mirroring, retryable failure, fully mirrored, and partial mirror states. The global Mirror row reports fetch/validation/persistence/durable stages separately.

The extension never returns raw cookies, authorization values, complete request-header sets, or browser storage to Rust. Exact private conversation bodies cross only into the local process for exact-ID validation and durable local storage.

Before 0.4 reaches live QA, the repository gate requires:

1. Edge extension syntax and permission invariants;
2. pure history-classifier tests;
3. exact-conversation response-matcher tests;
4. temporary-tab create/navigate/cleanup invariants;
5. stage-specific timeout/failure diagnostics;
6. explicit UI state tests;
7. Rust formatting;
8. full workspace compile;
9. lockfile stability;
10. protocol corpus validation;
11. full workspace tests;
12. Linux desktop compile/tests.

The full failure chain and permanent gates are documented in `docs/postmortems/2026-10-03-chatgpt-history-bridge.md`.

A product-shaped native desktop shell remains available as a local test surface while P3 completes. It exercises durable composition, typed local authored-message identity, restart recovery, and durable transcript rendering without claiming remote history completeness.

## P4 — direct text turns

Add controlled remote mutation support.

Deliverables:

- create conversation;
- send text message;
- stream assistant response;
- stop generation;
- reconcile uncertain outcomes;
- avoid duplicate sends after ambiguous disconnects;
- preserve raw observed events needed for debugging.

Exit criterion: ordinary text conversation is usable without the official site UI for the validated protocol revision.

## P5 — native desktop workstation

Complete the egui experience around the reliable core.

Deliverables:

- conversation browser;
- logical chat containers whose identity survives physical session rollover;
- healthy/aging/saturated/retired session lifecycle with explicit context handoff to successors;
- durable composer;
- transcript renderer;
- explicit local/remote/recovery status;
- attachments;
- search;
- export;
- diagnostic/event inspector;
- protocol compatibility indicator.

Exit criterion: Chatarium is the preferred daily interaction surface rather than merely a recovery utility.

## P6 — feature expansion

Only after the text path is reliable:

- branches/edit/retry semantics;
- model and reasoning controls;
- richer attachments;
- tool/event rendering;
- project-like organization;
- local full-text search and indexing;
- optional interoperability with other local project-state systems.

## P7 — supervisory orchestration and MCP workstation

Build the multi-session/tool control plane described in `docs/PRODUCT_VISION.md`.

This phase is intentionally sequenced after reliable direct session interaction. It is part of the intended product endpoint, not a redefinition of Chatarium into a generic browser.

Deliverables:

- first-class MCP/tool integration surface;
- import/adaptation of the existing user-owned XML tool/message envelope into a versioned Chatarium contract;
- multiple concurrent ChatGPT sessions with explicit local session identity;
- reuse logical chat-container rollover so long-running controller/worker conversations can replace saturated physical sessions without losing lineage;
- user-designated master/controller and worker relationships;
- durable goal assignment/update and continuation/stop messages;
- explicit worker lifecycle states such as working, blocked, needs-input, completed, and failed;
- correlation rules that prevent accidental unbounded master/worker `continue` loops;
- local routing/message bus with source/destination/provenance;
- egui supervisory console showing routed session/tool traffic;
- user policy controls for allow, block/forbid, require approval, redirect, and interrupt;
- durable/auditable orchestration and tool-call history.

Exit criterion: the user can supervise several ChatGPT sessions and MCP/tools from one Chatarium surface, delegate/continue work without manual copy-paste, positively recognize worker completion, and inspect or veto cross-session/tool traffic before or after dispatch according to policy.

## Continuous work — protocol revision response

Whenever the official client changes materially:

```text
capture -> sanitize -> snapshot -> diff -> document -> adapt -> test -> release
```

A protocol break is a maintenance event, not a reason to erase history or silently broaden assumptions.
