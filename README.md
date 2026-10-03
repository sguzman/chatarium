# 🟧 Chatarium

**A locally durable, empirically specified interface to the ChatGPT consumer service.**

Chatarium exists because a conversational client must not be able to lose authored text, partially received responses, or the factual history of what happened merely because a browser tab, frontend deployment, or network connection failed.

The project has two equal pillars:

1. **Protocol Observatory** — observe, preserve, version, and document the behavior of the ChatGPT web client as an empirical, timestamped interface. No stability horizon is assumed.
2. **Resilient Client** — build a local-first Rust client and supporting recovery tools whose durable state is authoritative over transient UI state.

The protocol corpus is evidence about ChatGPT as observed. The Rust implementation is our interpretation of that evidence. They intentionally remain separate.

## Long-term product direction

Chatarium is intentionally broader than a single reliable chat window. Its long-term endpoint is a **user-controlled ChatGPT desktop workstation** with a visible local control plane: native conversation interaction, MCP/tool integration, multiple coordinated ChatGPT sessions, master/controller -> worker routing, explicit worker lifecycle/completion states, and an egui supervisory surface where routed actions can be inspected, allowed, blocked/forbidden, approved, redirected, or interrupted.

The project should reuse/adapt the user's existing XML-oriented MCP/tool envelope from the Braizen/ChatGPT-shim work where practical rather than inventing an unrelated tool language by default. The exact XML schema is not yet imported into this repository and remains a future versioned design dependency.

These capabilities are sequenced after the reliability/protocol foundations; they are not a reason to skip them, and they are not optional evidence that Chatarium should be narrowed back to “just a chat client.”

See [`docs/PRODUCT_VISION.md`](docs/PRODUCT_VISION.md) for the formal product intent.

## Core invariants

- **User-authored text must never exist only in transient UI state.**
- **Received assistant output is persisted incrementally.**
- **Network uncertainty is represented explicitly; it is never silently collapsed into success or failure.**
- **Observed protocol revisions are timestamped facts, not promises of future compatibility.**
- **Raw captures and derived documentation retain provenance.**
- **Secrets are not fixtures.** Session cookies, authorization material, anti-CSRF tokens, and equivalent credentials must be removed from committed evidence.
- **The UI thread does not perform heavy work.** Network, persistence, parsing, capture processing, and reconciliation stay outside rendering.
- **Chatarium does not require a second OpenAI API subscription as its architectural premise.** Its target is the consumer ChatGPT service already used through the official web client.
- **Human QA is automation-first.** If Chatarium can create directories, drive a synthetic experiment, collect evidence, hash, sanitize, import, diff, or validate a result itself, the program does that work rather than delegating it to the operator.

## Repository map

```text
chatarium/
├── apps/desktop/              # Native egui client
├── browser/                   # Emergency browser-side durability layer
├── crates/
│   ├── core/                  # Domain model and state machines
│   ├── protocol/              # Typed interpretation of observed protocol
│   └── store/                 # Durable local state
├── tools/
│   ├── capture/               # One-command Edge/CDP experiment harness
│   ├── importer/              # Flight-recorder export -> native journal bridge
│   └── recorder/              # Capture sanitization, inspection, and diff tooling
├── protocol/                  # Empirical protocol corpus
│   ├── experiments/           # Machine-executable canonical observations
│   ├── snapshots/             # Timestamped observations
│   ├── flows/                 # User action -> observed network behavior
│   ├── endpoints/             # Cross-snapshot endpoint documentation
│   ├── schemas/               # Derived schemas
│   └── fixtures/              # Sanitized executable evidence
└── docs/                      # Architecture, reliability, capture and QA policy
```

## Status

Chatarium has completed **P1 protocol observatory** and **P2 durable application core** work and is entering **P3 read-only remote integration**.

The protocol baseline includes complementary observations from a manual Edge/Linux HAR and a canonical C03 Flight Recorder capture. Snapshot `2026-09-29.002` establishes the observed v1 SSE text-turn grammar, and Flight Recorder v0.6.0 has passed live protocol-backed send/assistant reconciliation without relying on current DOM selectors. The recorder pipeline can ingest private cumulative exports into fail-closed sanitized evidence, validate the committed corpus, classify structural changes, and diff comparable observations.

The durable core now has typed local conversation/turn/message identities, fsync-backed user-message commits, an append-only JSONL authority, rebuildable schema-v2 SQLite projections, replayable evidence state, and a persistent crash-transition matrix. Once a typed local commit succeeds, later torn-tail recovery or stale projection state cannot erase authorship or fabricate remote certainty.

The native desktop shell is also locally runnable now as a deliberately local-only vertical slice. Its current surface uses a real chat-oriented layout rather than the original engineering scaffold, commits new local messages through the typed durable authored-message path, restores the same local conversation identity after restart once a typed message exists, derives a conversation title from durable transcript state, and renders durable user/assistant transcript observations with repeated assistant snapshots collapsed by observed message identity. This does not imply remote transport is finished: the UI still labels itself local-only until the authenticated provider path is evidence-complete.

Raw HAR and Flight Recorder exports remain private evidence. `chatarium-recorder snapshot-flight` remains the active evidence-ingestion path. P3 now has a successful C02 conversation-fetch baseline in snapshot `2026-10-01.001`, an evidence-gated parser for that envelope, restart-safe durable import for selected/bound conversations, and a typed runtime seam that composes durable readiness with a live authenticated-session lease before a provider may fetch the exact bound remote conversation. Imported private C02 bodies remain local journal state with exact remote identity/read-observation provenance; public protocol fixtures remain sanitized. Flight Recorder v0.7.2 and the sanitizer/import/journal pipeline can preserve a deliberately tiny, publication-safe C02 query-value grammar for only `include_has_versions` and `num_turns`, but no empirical v0.7.2 capture has established their actual values yet; historical v0.7.1 evidence therefore remains explicitly values-unknown. C01 conversation-list semantics remain deliberately unbaselined.

For the primary **new-conversation/chatting** path, Chatarium now uses OpenAI's documented **Sign in with ChatGPT** DevKit for open-source/local apps rather than an undocumented browser-cookie bridge. The implementation pins the official DevKit at a reviewed commit and runs its trusted local SDK in a narrow Node sidecar: OAuth/OIDC + PKCE, loopback callback, token rotation, model discovery, and Responses streaming remain inside that credential-owning process. The Rust desktop receives only safe session/model state, stream text, completion, and sanitized errors over local NDJSON. On Linux, the DevKit credential file is encrypted with AES-256-GCM using a key held by the OS Secret Service; OAuth material never enters the authoritative conversation journal. Connected sends use the public Responses route with `store:false` and `stream:true`, and Chatarium durably commits the exact user message plus dispatch evidence before the sidecar is allowed to send it. Existing chatgpt.com conversation history is explicitly a separate capability: Sign in with ChatGPT does not grant access to the user's ChatGPT conversations/account context, so C02/#88 remains optional migration/sync research rather than a blocker for the desktop chat alpha.

Automated browser capture remains a longer-term goal rather than a prerequisite for protocol discovery. The existing Windows capture harness is parked; the active protocol workflow is cross-platform and Linux-friendly.

See [`docs/ROADMAP.md`](docs/ROADMAP.md), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/CAPTURE_HARNESS.md`](docs/CAPTURE_HARNESS.md), [`docs/IMPORT_BRIDGE.md`](docs/IMPORT_BRIDGE.md), [`docs/HUMAN_QA.md`](docs/HUMAN_QA.md), and [`protocol/README.md`](protocol/README.md).

## Non-goals

Chatarium is not a claim that ChatGPT exposes a supported public consumer API. It does not assume undocumented interfaces will remain stable, and it will not treat browser internals as timeless contracts. It also does not aim to bypass authentication, access controls, rate limits, or anti-abuse mechanisms.

## Live alpha status

As of 2026-10-02, the official **Sign in with ChatGPT** path has been exercised successfully on the target Linux desktop: Chatarium completed browser authorization, received a connected ChatGPT-plan session, and discovered the account's available models. This proves the local desktop can authenticate without an API key and can use the user's ChatGPT-plan authorization boundary.

The remaining live alpha gate is one successful streamed assistant completion. The first inference attempt reached the authenticated Responses path but coincided with a model-refresh loop that repeatedly re-requested the model catalog and triggered a rate-limit error. That loop is now guarded as single-flight model discovery and is under CI before the next live test.

## Development

For the current desktop alpha, launch normally:

```sh
cargo run -p chatarium-desktop
```

The desktop now prepares the pinned official Sign in with ChatGPT DevKit automatically in a background worker before starting its credential-owning sidecar, so the user does not need to run a separate bootstrap command. The first authenticated launch requires Node.js 22+ and network access to initialize/install the exact lockfile-resolved DevKit dependencies; later launches reuse the verified build. On Linux, authenticated use also requires a working Secret Service implementation plus the `secret-tool` helper. Chatarium never installs OS packages automatically; if that protection backend is unavailable, authentication fails closed while the local-only application remains usable.

`node tools/run-desktop.mjs` remains a development convenience wrapper, but it is no longer required for ordinary repository launches.

The workspace is Rust-first. Some legacy capture/bootstrap machinery is Windows-specific, but protocol observation, ingestion, typed interpretation, and the durable core are not architecturally Windows-bound. The desktop shell uses `eframe`/`egui`; asynchronous and blocking work must remain off the render thread. Protocol captures are data, not hand-maintained folklore: when behavior changes, preserve a new observation and adapt against it.

The repository is intentionally documentation-heavy because the hardest part of this system is not drawing a chat window. It is maintaining epistemic clarity across a mutable remote service, unreliable transport, local persistence, and recovery.
