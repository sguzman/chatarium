# Architecture

Chatarium is organized around one asymmetry: local state can be made durable and inspectable; the remote ChatGPT service and the network path to it cannot be assumed to be either.

## System model

```text
                         observed evidence
                              |
                              v
+----------------+      +--------------+      +----------------+
| protocol corpus|----->| protocol     |----->| remote adapter |
+----------------+      | interpretation|      +-------+--------+
                        +--------------+              |
                                                      |
+----------------+      +--------------+              |
| desktop / P0 UI|<---->| application  |<-------------+
+----------------+      | core         |
                        +------+-------+
                               |
                               v
                        +--------------+
                        | durable store|
                        +--------------+
```

The protocol corpus is not generated from Rust types. Rust types are generated or written from protocol evidence.

## Durable authority

The authoritative local record consists of append-oriented events plus materialized projections for fast UI access. A future schema may evolve, but the semantic split is fixed:

- **events** answer what was observed or attempted and when;
- **projections** answer what the UI should currently render;
- **remote identifiers** allow later reconciliation without making remote state authoritative over local authorship.

Remote conversation identity is now a first-class domain separate from `LocalConversationId`. A `RemoteConversationId` is an exact opaque string: core does not assume UUID syntax, parse structure, case semantics, or normalization. The authoritative journal can bind one local conversation one-to-one to one remote conversation identity and records the named protocol observation revision that justified that correlation. The remote identifier stays in the private event payload rather than the journal scope. A binding is provenance only; it does not imply authentication, connectivity, fetch success, synchronization completeness, or remote mutability.

Read-side observation is a separate evidence layer from remote identity binding. `crates/protocol::read` validates the structural metadata permitted by the Flight Recorder read surface: C01/C02 experiment identity, named observation revision, GET/HEAD, normalized `/backend-api/` path, query-key names, status/content type, truncation/body-presence, and optional JSON top-level type. Flight Recorder v0.7.2 additionally permits narrowly typed query-value evidence only for exact C02 `/backend-api/conversations/<id>` reads and only for `include_has_versions` plus `num_turns`. It preserves occurrence order and duplicates; the only publishable literal grammar is empty, lowercase boolean, or optional-minus decimal integer up to 10 digits. Anything outside that grammar is represented as unsupported/redacted rather than copied. Historical v0.7.0/v0.7.1 evidence has no such field and therefore means query values are unknown, not absent. `crates/store::remote_read_audit` can journal that metadata under a local `RemoteReadObservationId`, but raw response bodies and private account/conversation content are forbidden from the typed event. `chatarium-importer read-fixture` is the evidence bridge from publication-safe C01/C02 fixtures into this durable audit. It requires the caller to choose one `read_responses` index explicitly, validates the selected entry through `ReadObservation`, archives the sanitized fixture by SHA-256, derives an idempotent local observation identity, and persists the fixture hash/index as provenance. It deliberately does not infer which request in a capture has conversation-list/fetch semantics. Semantic compatibility is flow-specific: the global C03 text-turn revision does not imply conversation-list or conversation-fetch support. Controlled C01 evidence now exists at `2026-09-30.001`: it proves a paginated `/backend-api/gizmos/snorlax/sidebar` surface with nested conversation summaries during the controlled sidebar-list action. That observation does not prove complete account-wide conversation enumeration, so it does not establish the `ConversationList` semantic baseline. C02 has both the historical rate-limited observation `2026-09-30.002` and a successful validated conversation-fetch baseline at `2026-10-01.001`. Consequently `ConversationFetch` can validate only that named revision, while `ConversationList` deliberately remains `Compatibility::NoBaseline`.

`crates/store::remote_mirror_readiness` composes those two durable layers without adding a new authoritative event. For one local conversation it requires a durable remote identity binding plus C02/open-conversation evidence for the same protocol revision, then requires exactly one semantically `ValidatedAgainst` observation. Missing evidence, `NoBaseline`, protocol mismatch, revision mismatch, or ambiguous validated evidence remains an explicit blocked state. The derivation is read-only: it does not fetch, parse, import, or mutate conversation state. A binding and one unambiguous `2026-10-01.001` C02 observation can now reach `Ready`; other revisions continue to fail closed.

`crates/store::remote_mirror_selection_audit` records only durable user intent about which already-bound conversations should participate in mirroring. Selection requires the remote binding to predate the selection event; later bindings cannot retroactively legalize earlier intent. Selection and deselection survive restart, but neither implies authentication, fetch success, synchronization, or import. A derived selected-mirror plan composes current intent with `remote_mirror_readiness`, yielding `Unselected`, `SelectedButBlocked`, or `SelectedAndReady` without appending any new evidence.

`crates/protocol::conversation_fetch` is the semantic parser for the successful C02 envelope. It is revision-gated, verifies optional expected remote identity, preserves message content/metadata structurally, and exposes only parent links that are actually present as `metadata.parent_id`; it does not infer missing ancestry from array order. `crates/store::remote_mirror_snapshot_audit` then provides the durable import boundary for an already-fetched private body. Import re-derives the selected/readiness plan from journal history, requires the parsed conversation identity to match the durable binding, records the supporting protocol revision plus binding/read-observation sequences and observation ID, stores the exact private response body locally, and is idempotent for an identical snapshot across restart. Replay verifies that the selection/readiness evidence predated the snapshot and reparses the body against the recorded revision. SQLite can carry the snapshot event through rebuild, but the append-only journal remains authoritative.

The first persistent implementation is a newline-delimited JSON event journal in `crates/store`. Each complete record has a stable event-kind name, monotonic sequence, local Unix-millisecond timestamp, and exact textual payload. A successful persistent append does not return until the line has been written, flushed, and `sync_data()` has succeeded.

Startup treats only an **unterminated final fragment** as a torn last write and truncates it back to the previous newline. A malformed complete record is a hard integrity error; it is not silently skipped.

SQLite now exists as a rebuildable projection/index layer in `crates/store`. It does not replace the append journal as the evidence history. The projection database is versioned independently, can be deleted and rebuilt entirely from durable journal events, and is updated/rebuilt transactionally so a failed rebuild cannot replace a previously valid projection.

Projection schema v2 also materializes typed locally authored turns derived from typed `UserMessageCommitted` events plus same-turn durable evidence. Each row preserves local conversation/turn/message identities, exact committed user text, commit/last-applied sequences, and replayed local/remote/assistant evidence. Legacy text-only commits remain in the event projection but are intentionally absent from the typed turn table because Chatarium does not invent local IDs retroactively. After a projection-schema migration, the projection is not considered current until a full journal rebuild materializes the new schema.

The minimum event vocabulary includes:

- draft changed;
- user message committed;
- remote dispatch attempted;
- remote acceptance observed;
- assistant stream started;
- assistant delta observed;
- assistant completion observed;
- connection interrupted;
- remote failure observed;
- reconciliation attempted;
- reconciliation result observed.

## The local-commit gate

Remote mutation has a hard precondition:

```text
exact user text
    |
    v
append UserMessageCommitted
    |
    v
flush + sync_data
    |
    v
local acknowledgement
    |
    v
ONLY NOW may a remote adapter dispatch
```

A UI click is not a commitment. Queueing a disk write is not a commitment. Starting an HTTP request is not a commitment. `LocalEvidence::MessageCommitted` means the persistence boundary positively acknowledged the exact user text.

The desktop shell already exercises this contract without networking. Composer edits are sent to a background persistence worker so render work never waits on disk. The UI visibly distinguishes `saving…` from `durable`; the local commit action waits for a journal acknowledgement before changing turn evidence.

The store crash matrix independently verifies the same boundary through process-style reopen/rebuild tests: once a typed commit has returned, later torn-tail recovery or stale SQLite state cannot erase its exact text or local identities.

## Turn state is evidence, not optimism

A turn is not modeled with a single `sent` boolean. Local and remote knowledge are separate.

A representative lifecycle is:

```text
Draft
  |
  v
CommittedLocal
  |
  v
Dispatching
  |--------------------------+
  |                          |
  v                          v
AcceptedObserved        OutcomeUnknown
  |                          |
  v                          +--> ReconciliationPending
Streaming                         |
  |                               +--> AcceptedObserved
  |                               +--> FailedObserved
  |                               +--> StillUnknown
  +--> CompletedObserved
  +--> Interrupted
  +--> FailedObserved
```

`OutcomeUnknown` is a valid state. A socket closing after request bytes were written does not prove that the remote service rejected the request.

## Crate boundaries

### `crates/core`

Owns domain identifiers, local/remote evidence state, events, commands, and recovery decisions. Local conversation, turn, and message identities are distinct UUIDv7-backed Rust types; their UUID ordering is not treated as semantic chronology. Stable event names used by durable storage are defined here. It knows nothing about egui and should know as little as possible about concrete HTTP shapes.

### `crates/protocol`

Owns typed interpretations of empirically observed ChatGPT request/response/event shapes. Every supported interpretation identifies the protocol observation revision from which it was derived.

### `crates/store`

Owns durable persistence, migrations, event append, projection rebuild, and transactional guarantees. The authoritative implementation is the crash-recoverable JSONL journal. SQLite is a subordinate query projection with explicit schema migration, sequence validation, scope/kind indexes, a typed authored-turn materialization, and transactionally safe rebuild semantics. No network logic belongs here.

### `tools/recorder`

Owns offline capture ingestion, sanitization checks, structural request inventories, snapshot manifests, schema extraction, and revision diffs. Browser/CDP capture integration can be added behind this boundary.

### `apps/desktop`

Owns presentation and user interaction. The render thread emits persistence/network commands and consumes cheap state snapshots; it does not perform blocking network or storage operations. A background persistence worker currently owns the journal file.

## P0 browser flight recorder

Before the full native client is capable of direct interaction, Chatarium provides a browser-side reliability layer. Its purpose is narrow:

- persist composer state continuously into a per-conversation emergency WAL;
- snapshot outgoing send intent into a separate synchronous journal before the site's submit path;
- capture conversation identity and URL;
- persist the latest rendered assistant output into an independent emergency WAL and IndexedDB projection;
- record connectivity, navigation, visible error/toast observations, and unresolved sends;
- provide copy/export recovery surfaces independent of the current DOM.

For controlled protocol observation only, Flight Recorder v0.7.0 also exposes a separate read-capture mode. It is in-memory and off by default on every page load. While explicitly armed it may clone same-origin `GET`/`HEAD` fetch responses under `/backend-api/` only when their response content type is JSON-family. It records path plus query-key names, never query values, request headers, cookies, authorization values, request bodies, browser storage, or third-party traffic. Per-response and per-run byte limits are explicit, and any clone/capture failure is isolated from ChatGPT's original response branch.

Raw read bodies exist only in the private recorder export. `snapshot-flight` fail-closes them into structural public evidence: keys, containers, array occurrence/length, JSON pointer/type observations, typed scalar placeholders, and deterministic identity placeholders. Arbitrary titles/message strings are not publishable output. A truncated read body is omitted rather than parsed; malformed declared JSON aborts derivation rather than copying raw text. The browser recorder deliberately does not label any observed endpoint as "conversation list" or "conversation fetch"; those semantics require committed C01/C02 evidence.

This layer is intentionally disposable once the native client supersedes it, but its evidence vocabulary should align with `crates/core` so recovered histories can be imported later.

## Browser integration boundary

The 2026-10-03 account-history incident established a hard architectural distinction between **browser observation prototypes** and **critical runtime interoperability**.

Tampermonkey remains valid for:

- the emergency flight recorder;
- disposable protocol experiments;
- temporary observation/instrumentation.

Tampermonkey is **not** the production account-history transport. The retired `account-bridge.user.js` remains in the repository as evidence of the experiment, not as the intended runtime boundary.

The replacement account-history integration is a purpose-built Edge/Chromium extension. Its job is narrow:

- identify the intended `https://chatgpt.com` tab;
- execute evidence-backed first-party reads in the correct page execution context;
- observe required first-party account/workspace request context without exporting reusable credentials;
- exchange typed commands/results with Chatarium over a local transport;
- emit proof-level diagnostics for each boundary;
- never become an arbitrary browser/HTTP proxy.

The extension transport remains subordinate to the existing protocol/store layers. It must reuse:

- evidence-gated C01/C02 request models;
- exact remote-ID validation;
- live-mirror durability;
- active-branch transcript projection;
- explicit pagination uncertainty;
- local/remote/historical provenance classes.

The browser component does **not** decide that a result is semantically trustworthy merely because HTTP succeeded. The desktop/protocol layer must still perform schema, identity, and contradiction checks.

Before first human QA, the extension must self-report distinct evidence for:

`extension alive -> ChatGPT tab found -> desktop roundtrip -> page MAIN-world execution -> authenticated session -> account context -> target HTTP result -> parser result -> semantic sanity -> durable mirror`.

No aggregate `connected` state substitutes for that chain.

See `docs/postmortems/2026-10-03-chatgpt-history-bridge.md` for the failure history that created this boundary.

## Protocol revisions

A protocol revision is an observation, not a semantic version of ChatGPT. IDs use a timestamp-oriented form such as `2026-09-17.001`. Multiple observations on the same day may increment the suffix.

The implementation advertises the newest observation it was validated against. Compatibility with later revisions is unknown until tested.

## Authentication boundary

Authentication authority is transient **at each remote operation boundary**, while renewable OAuth credentials may be persisted in a separate protected credential store when the official authentication flow requires it. Reusable credentials never belong in protocol fixtures, the authoritative conversation journal, SQLite projections, logs, or ordinary diagnostics.

`crates/core::authenticated_session` defines the mechanism-agnostic runtime boundary. A concrete `UserAuthenticatedSessionProvider` owns session/authentication material and exposes only `Authenticated`, `Unauthenticated`, or `Unknown` evidence. Only positive evidence can create an `AuthenticatedSessionLease`. The lease remains borrow-scoped, non-`Clone`, non-`Copy`, non-serializable, and contains no credential fields or durable identifier. Adapter use revalidates authentication evidence immediately before accessing the provider, so a lease cannot silently retain authority after the underlying session becomes unusable. The provider itself may load protected renewable credentials after restart; journal replay still cannot recreate an authenticated lease.

The primary desktop provider now uses OpenAI's documented **Sign in with ChatGPT DevKit** for open-source/local apps. Chatarium pins the official upstream repository at a reviewed gitlink and builds only its local SDK. A trusted Node sidecar owns OAuth/OIDC Authorization Code + PKCE, the loopback callback on `127.0.0.1`, dynamic registration, ID-token/JWKS verification, rotating refresh credentials, profile/model discovery, and Responses streaming. The Rust desktop communicates with that sidecar over stdin/stdout NDJSON and deliberately has no credential-bearing message type. On Linux, the sidecar's encrypted DevKit state uses AES-256-GCM with the encryption key stored through Secret Service/`secret-tool`; there is no plaintext fallback. The core remains ignorant of concrete token fields and endpoint mechanics. This official path is for new Chatarium inference and does not grant access to existing ChatGPT conversation history.

For official Responses inference, Chatarium's local-first transcript becomes the persistent conversation substrate: authored text is durably committed before dispatch; requests use `store:false` and `stream:true`; required history is reconstructed from durable local state and supplied in `input`; streamed assistant output is appended incrementally; and ambiguous transport outcomes remain explicit. OAuth credentials live outside that conversation history in protected local credential storage.

`crates/store::remote_mirror_execution` and `crates/store::remote_mirror_runtime` remain the composition path for the **optional existing-chatgpt.com mirror/import feature**. Persistent state can derive only `Unselected`, `ProtocolBlocked`, or `RequiresAuthenticatedSession`; a mirror read still requires a live authenticated lease and exact evidence-gated C02 semantics. The v0.7.2 query-evidence pipeline may not invent unknown values. Snapshot `2026-10-03.001` later established the current C02 safe query literals from the operator's raw Edge HAR. The store/runtime seam remains transport-agnostic and is intended to be reused by the replacement Edge extension. This mirror path is no longer a blocker for ordinary desktop chatting.

## Future orchestration and tool plane

The direct ChatGPT adapter is not the final architecture boundary. The long-term workstation adds a **local routing/policy plane** above reliable single-session primitives.

That plane will coordinate:

- human-authored conversation messages;
- master/controller-session instructions;
- worker-session status/results;
- goal updates and continuation/stop control messages;
- MCP/tool request/result envelopes;
- user approval/deny decisions.

The intended design is an explicit durable message bus, not hidden prompt automation. Every routed action should have a source, destination, identity/correlation, lifecycle state, provenance, and policy decision where applicable.

A controller/master session may coordinate worker sessions, but it is subordinate to user policy. The desktop UI must eventually let the operator inspect queued/dispatched/completed traffic and allow, block/forbid, require approval for, redirect, or interrupt routes.

Cross-session orchestration must use explicit lifecycle/control states rather than infer completion from ordinary prose. In particular, the system needs machine-recognizable completion/blocked/input-needed states so controller and worker sessions cannot accidentally enter unbounded mutual `continue` loops.

MCP/tool integration should reuse or adapt the user's existing XML-oriented envelope from the Braizen/ChatGPT JavaScript-shim work where practical. Until that schema is recovered and committed, its concrete wire fields remain unspecified; Chatarium must not invent them from memory.

The durable event model should be extensible to this plane. Orchestration state must survive restart, and tool/session actions must remain auditable. See [`PRODUCT_VISION.md`](PRODUCT_VISION.md).

The first durable routing audit now uses the same append-only JSONL authority as ordinary turn evidence. Typed route proposal, explicit user decision, one-shot dispatch, and generic result/error observations replay after restart through the core route gate semantics. SQLite carries these routing events through its existing generic projected-events table; there is intentionally no dedicated routing materialization or schema bump yet. This is still local control-plane state only: no live master/worker or MCP transport is implied.

Worker orchestration state now follows the same authority rule. Typed worker identity, goal assignment, and lifecycle transitions are journaled and replayed through the existing `WorkerLifecycle` state machine, so `Working`, `NeedsInput`, `Blocked`, and terminal states survive restart without inferring state from prose. Continuation authority history is also replayable, including lease identity, allowance, issued ordinals, and consumption, but replay never manufactures a fresh live move-only permit. SQLite again carries these facts generically without a dedicated worker materialization or schema bump.

Typed worker controls sit above that lifecycle state and below future transport. Admission validates start/resume, continue, stop, and status requests against the current goal/phase without changing lifecycle optimistically. Continue additionally consumes a move-only `ContinuationPermit`; routing consumes a move-only `DispatchPermit`. Session/endpoint binding and the eventual XML representation remain separate future layers.

Admitted worker controls are now also journaled as their own typed audit facts. Control identity, worker/goal correlation, semantic kind, and continuation permit ordinal survive restart, but control replay does not mutate worker lifecycle or imply dispatch, delivery, or execution. Those facts remain separate evidence layers. SQLite carries admitted-control events generically without a dedicated schema/materialization.

Control-to-route correlation is a separate durable provenance layer. A typed `ControlRouteBinding` permits only `OrchestrationControl` routes, and replay requires both the control admission and route proposal to predate the binding. The audit enforces one-control-to-one-route cardinality in both directions. It intentionally does not define `WorkerId` to `RouteEndpointId` mapping, session identity, transport, or dispatch semantics.

Local session identity is now explicit rather than implicit in routing endpoints. `SessionId`, `WorkerId`, and `RouteEndpointId` are separate local identity domains. The authoritative journal can register sessions, bind a session one-to-one to a routing endpoint, and bind a worker one-to-one to a session. These records remain independent of remote ChatGPT identity and do not create route proposals or worker lifecycle state merely by existing.

Logical conversation continuity is a separate layer above `SessionId`. `ChatContainerId` identifies a conversation that may span multiple physical sessions. The container audit keeps one linear lineage whose current leaf is `Healthy`, `Aging`, or `Saturated`; only healthy/aging leaves accept ordinary turns. A successor binding is legal only for the current saturated leaf, carries a unique opaque `ContextHandoffId`, atomically retires the predecessor, and joins the already-registered successor as the new healthy leaf. Direct phase transitions cannot fabricate `Retired`: retirement therefore always has successor provenance. This local lineage does not imply remote conversation creation, remote context attachment, or any particular context-pressure threshold. Those mechanisms remain future P4/P5 adapter work.

Controller/worker supervision is now a separate durable relationship layer above those identities. A controller session is explicitly designated, may supervise multiple worker sessions, and each worker session may have at most one controller. Controller and worker roles are intentionally disjoint in this first model to prevent accidental controller chains/cycles. Supervision does not alter route policy, issue continuation/dispatch permits, create controls, or mutate worker lifecycle; the user remains the authority above the controller.

Worker-control issuer provenance is now explicit as either direct user action or a designated controller session. Controller-issued controls are compositionally validated against the durable supervision topology before they are treated as correctly routed: the target WorkerId must resolve to its worker SessionId, the issuer must be that session's designated controller, both sessions must already have routing endpoints, and the route source/destination must match those endpoints. User-issued controls intentionally do not invent a user routing endpoint. This provenance/topology validation still grants no route-policy bypass or dispatch authority.

Control admission is also replay-verifiable against historical worker state. A durable control must have been valid for the target worker's goal and phase immediately before its admission event, and the same semantic control is revalidated immediately before `ControlRouteBound`. Controller-issued provenance additionally requires the controller-to-worker supervision and worker-session binding to predate issuance rather than becoming true retroactively. These checks reject stale replacement-goal controls and controls invalidated by attention/terminal transitions before routing.

Dispatch-time freshness is now a third independent historical boundary. For every dispatched `OrchestrationControl`, Chatarium requires a previously validated control-route/provenance chain and revalidates the control against worker state immediately before `RouteDispatched`. The composed dispatch view preserves admitted, bound, and dispatched phases plus the `DecisionAuthority` that passed `RouteGate`. User approval therefore does not authorize a stale command: if the worker changes after binding/approval, replay fails closed and no retry/reissue is inferred.

Bounded continuation authority is now durable as its own provenance chain. A `ContinuationLeaseId` binds an explicit allowance to one WorkerId/WorkerGoalId; the lease object itself is not copyable. Permit issuance is journaled ordinal-by-ordinal and replayed through the same core `ContinuationLease::authorize` semantics, so wrong phases, stale goals, skipped/duplicate ordinals, and exhausted allowances fail closed. A v2 `Continue` control records the originating lease identity and consumes exactly one already-issued permit. Restart reconstructs allowance, issued ordinals, consumption, and issued-but-unconsumed permits without minting fresh authority. Legacy v1 `Continue` records remain shape-readable but cannot enter validated admission/routing/dispatch because they lack lease provenance.

## Failure philosophy

Chatarium prefers visible uncertainty over fabricated certainty. If a failure cannot be distinguished from a successful remote action whose acknowledgement was lost, the UI should say so and offer reconciliation instead of resending blindly.
