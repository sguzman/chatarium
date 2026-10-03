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

The Tampermonkey `account-bridge.user.js` experiment is **retired from the critical runtime path**. It produced useful protocol evidence and one successful browser↔desktop roundtrip, but repeated live QA exposed unstable transport/execution-world behavior and inadequate first-party request-context parity. Most importantly, the supplied HAR later showed that first-party conversation-list requests include account-selection context that the initial bridge omitted.

The replacement browser runtime is now implemented under `browser/edge-bridge/` as a purpose-built Edge/Chromium Manifest V3 extension, tracked by #102. The retired userscript transports are rejected by the Rust critical path rather than retained as hidden fallbacks. The extension uses narrow host permissions, keeps the raw account selector browser-local, executes evidence-backed reads in the exact ChatGPT tab's MAIN world, and carries explicit proof metadata across the existing typed loopback protocol.

The first Edge Bridge 0.1.0 live validation has now occurred. It proved extension/version, desktop roundtrip, exact ChatGPT tab selection, MAIN-world execution, authenticated session, account context, HTTP 200, and parser success, but the ordinary-history result was `items=0 · total=0` and was correctly held at `semantic=unconfirmed-zero` rather than promoted to success.

A re-audit of the private HAR then found that the frozen ordinary-history request had been captured under HTTP 429. That artifact proves the global C01 request shape/context but not successful result semantics. The successful conversation-list-like responses in the same HAR are gizmo/project surfaces and are not treated as substitutes for ordinary global history.

Per the human-QA stop rule, Edge Bridge 0.2.0 is the **single evidence-driven correction** for this architecture. It observes the current first-party global C01 request, retains only a narrow allowlist of application-controlled request headers in browser session storage, and replays that observed context in MAIN world. Rust receives only safe proof metadata: whether request context was observed, the first-party HTTP status when available, and the replay-header count. Raw values remain browser-local.

No second live run is allowed until automated checks are green. The proof ladder for that final validation is:

1. extension/version alive;
2. exact ChatGPT tab found;
3. desktop roundtrip;
4. MAIN-world execution;
5. authenticated session;
6. required account/workspace context;
7. observed first-party request context;
8. exact request profile/revision;
9. first-party and replay HTTP evidence;
10. parser result;
11. semantic sanity result;
12. durable mirror result.

The desktop status row treats authentication as only partial proof. A list result becomes healthy only after extension/tab/MAIN/account/request-context/profile/HTTP/parser/semantic evidence succeeds; an exact-conversation sync reaches complete proof only after the matching durable mirror event. A zero-history result remains explicitly non-success.

The full failure chain and permanent gates are documented in `docs/postmortems/2026-10-03-chatgpt-history-bridge.md`.

A product-shaped native desktop shell has been pulled forward as a local test surface while P3 completes. It is not counted as P5 completion: it currently exercises durable composition, typed local authored-message identity, restart recovery, and durable transcript rendering without claiming remote connectivity. This keeps the reliability work continuously visible/testable instead of waiting for all remote phases before exposing the application surface.

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
