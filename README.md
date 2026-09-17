# Chatarium

**A locally durable, empirically specified interface to the ChatGPT consumer service.**

Chatarium exists because a conversational client must not be able to lose authored text, partially received responses, or the factual history of what happened merely because a browser tab, frontend deployment, or network connection failed.

The project has two equal pillars:

1. **Protocol Observatory** — observe, preserve, version, and document the behavior of the ChatGPT web client as an empirical, timestamped interface. No stability horizon is assumed.
2. **Resilient Client** — build a local-first Rust client and supporting recovery tools whose durable state is authoritative over transient UI state.

The protocol corpus is evidence about ChatGPT as observed. The Rust implementation is our interpretation of that evidence. They intentionally remain separate.

## Core invariants

- **User-authored text must never exist only in transient UI state.**
- **Received assistant output is persisted incrementally.**
- **Network uncertainty is represented explicitly; it is never silently collapsed into success or failure.**
- **Observed protocol revisions are timestamped facts, not promises of future compatibility.**
- **Raw captures and derived documentation retain provenance.**
- **Secrets are not fixtures.** Session cookies, authorization material, anti-CSRF tokens, and equivalent credentials must be removed from committed evidence.
- **The UI thread does not perform heavy work.** Network, persistence, parsing, capture processing, and reconciliation stay outside rendering.
- **Chatarium does not require a second OpenAI API subscription as its architectural premise.** Its target is the consumer ChatGPT service already used through the official web client.

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
│   ├── importer/              # Flight-recorder export -> native journal bridge
│   └── recorder/              # Capture, sanitize, inspect, and diff tooling
├── protocol/                  # Empirical protocol corpus
│   ├── snapshots/             # Timestamped observations
│   ├── flows/                 # User action -> observed network behavior
│   ├── endpoints/             # Cross-snapshot endpoint documentation
│   ├── schemas/               # Derived schemas
│   └── fixtures/              # Sanitized executable evidence
└── docs/                      # Architecture, reliability, development and QA policy
```

## Status

Chatarium is in **P0 durability / P1 observation** work. The browser flight recorder protects drafts, send intents, assistant snapshots, and visible failures; the native client has a crash-recoverable append-only journal; the import bridge moves browser recovery evidence into scoped native events; and the protocol recorder is ready for the first controlled ChatGPT web captures.

See [`docs/ROADMAP.md`](docs/ROADMAP.md), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/IMPORT_BRIDGE.md`](docs/IMPORT_BRIDGE.md), [`docs/HUMAN_QA.md`](docs/HUMAN_QA.md), and [`protocol/README.md`](protocol/README.md).

## Non-goals

Chatarium is not a claim that ChatGPT exposes a supported public consumer API. It does not assume undocumented interfaces will remain stable, and it will not treat browser internals as timeless contracts. It also does not aim to bypass authentication, access controls, rate limits, or anti-abuse mechanisms.

## Development

The workspace is Rust-first and Windows-first initially. The desktop shell uses `eframe`/`egui`; asynchronous and blocking work must remain off the render thread. Protocol captures are data, not hand-maintained folklore: when behavior changes, preserve a new observation and adapt against it.

The repository is intentionally documentation-heavy because the hardest part of this system is not drawing a chat window. It is maintaining epistemic clarity across a mutable remote service, unreliable transport, local persistence, and recovery.
