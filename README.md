# 🟧 Chatarium

**A locally durable, empirically specified interface to the ChatGPT consumer service.**

Chatarium exists because a conversational client must not be able to lose authored text, partially received responses, or the factual history of what happened merely because a browser tab, frontend deployment, or network connection failed.

The project has two equal pillars:

1. **Protocol Observatory** — observe, preserve, version, and document the behavior of the ChatGPT web client as an empirical, timestamped interface. No stability horizon is assumed.
2. **Resilient Client** — build a local-first Rust client and supporting recovery tools whose durable state is authoritative over transient UI state.

The protocol corpus is evidence about ChatGPT as observed. The Rust implementation is our interpretation of that evidence. They intentionally remain separate.

## Long-term product direction

Chatarium is intentionally broader than a single reliable chat window. Its long-term endpoint is a **user-controlled ChatGPT desktop workstation** with a visible local control plane: native conversation interaction, MCP/tool integration, multiple coordinated ChatGPT sessions, master/controller -> worker routing, explicit worker lifecycle/completion states, and an egui supervisory surface where routed actions can be inspected, allowed, blocked/forbidden, approved, redirected, or interrupted.

The original XML-like tool-call/result envelope has been recovered from the ChatGPT Tool Shim and ported into a strict, bounded Rust compatibility layer. Native MCP 2026 JSON-RPC is represented separately. Neither wire format grants tool execution authority; external providers remain disabled until explicit activation and one-shot approval.

These capabilities are sequenced after the reliability/protocol foundations; they are not a reason to skip them, and they are not optional evidence that Chatarium should be narrowed back to “just a chat client.”

See [`docs/PRODUCT_VISION.md`](docs/PRODUCT_VISION.md) for the formal product intent and [`docs/SELF_SUSTAINING_TRANSPORT.md`](docs/SELF_SUSTAINING_TRANSPORT.md) for the hard native-transport viability contract.

A browser may be used for protocol observation or occasional authentication bootstrap, but **Chromium-per-request is not an acceptable end state**. Ordinary conversation listing, retrieval, creation, continuation, send, and response streaming must ultimately be native Chatarium operations. Hiding or backgrounding Chromium does not satisfy this requirement.

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
- **Browser QA is isolated from personal Edge.** The canonical automation browser is Playwright-managed bundled Chromium with persistent QA state at `~/.local/share/chatarium-qa-browser/`. Microsoft Edge is not part of the QA control plane.

## Repository map

```text
chatarium/
├── apps/desktop/              # Native egui client
├── browser/                   # Emergency browser durability + retired browser prototypes
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

## Current active direction

Remote ChatGPT website mirroring/browser-transport work is **formally paused**.
Active development is local-first: Chatarium owns conversation durability,
context composition, explicit cross-conversation routing, controller/worker
orchestration, and user-controlled local memory while inference runs through the
plan-backed Sign in with ChatGPT Responses path.

The governing rule is visible authority: durable local facts and explicit user
decisions must exist before routed content, orchestration results, or memory can
affect another request. Model output is never promoted into control authority
merely because it was generated.

See [`docs/LOCAL_FIRST_EXPLORATION.md`](docs/LOCAL_FIRST_EXPLORATION.md),
[`docs/LIFECYCLE_STATE.md`](docs/LIFECYCLE_STATE.md), and
[`docs/LOCAL_MEMORY.md`](docs/LOCAL_MEMORY.md) and
[`docs/LOCAL_TOOL_INTEGRATION.md`](docs/LOCAL_TOOL_INTEGRATION.md) for the current executable
boundaries. The native transport viability contract remains in force but is not
the current work track.

## Status

Chatarium's authoritative application state is an append-only local journal with
rebuildable projections. The desktop has a working local-first inference path
and an increasingly typed local control plane; remote/browser history work is
preserved as evidence and migration research rather than the active product
dependency.

This README intentionally does not duplicate the implementation chronology.
Current work state lives in the focused documents linked above; protocol history
lives under [`protocol/`](protocol/), and incident history lives under
[`docs/postmortems/`](docs/postmortems/README.md).

## Non-goals

Chatarium is not a claim that ChatGPT exposes a supported public consumer API. It does not assume undocumented interfaces will remain stable, and it will not treat browser internals as timeless contracts. It also does not aim to bypass authentication, access controls, rate limits, or anti-abuse mechanisms.

## Development

For the current desktop alpha, launch normally:

```sh
cargo run -p chatarium-desktop
```

Terminal diagnostics are enabled by default at INFO level. They are stage-oriented rather than payload-oriented: startup/journal replay, browser-bridge command timing, history discovery, candidate counts, mirror capture, validation, and durable persistence are printed with elapsed milliseconds and thread names.

```text
[chatarium +  1842ms INFO  history    chatarium-history-discovery] discovery complete: candidates=7 unique-items=85 responses=189 backend-200=63 ...
[chatarium +  2410ms INFO  mirror     chatarium-remote-history-open] browser capture started for …12ab34cd
```

Use `CHATARIUM_LOG=debug` for bridge command delivery/timing details, `CHATARIUM_LOG=warn` for warnings/errors only, or `CHATARIUM_LOG=off` to silence terminal diagnostics. The logger intentionally does not dump conversation bodies, account identifiers, cookies, authorization material, or raw browser request headers.

The desktop now prepares the pinned official Sign in with ChatGPT DevKit automatically in a background worker before starting its credential-owning sidecar, so the user does not need to run a separate bootstrap command. The first authenticated launch requires Node.js 22+ and network access to initialize/install the exact lockfile-resolved DevKit dependencies; later launches reuse the verified build. On Linux, authenticated use also requires a working Secret Service implementation plus the `secret-tool` helper. Chatarium never installs OS packages automatically; if that protection backend is unavailable, authentication fails closed while the local-only application remains usable.

`node tools/run-desktop.mjs` remains a development convenience wrapper, but it is no longer required for ordinary repository launches.

The workspace is Rust-first. Some legacy capture/bootstrap machinery is Windows-specific, but protocol observation, ingestion, typed interpretation, and the durable core are not architecturally Windows-bound. The desktop shell uses `eframe`/`egui`; asynchronous and blocking work must remain off the render thread. Protocol captures are data, not hand-maintained folklore: when behavior changes, preserve a new observation and adapt against it.

The repository is intentionally documentation-heavy because the hardest part of this system is not drawing a chat window. It is maintaining epistemic clarity across a mutable remote service, unreliable transport, local persistence, and recovery.
