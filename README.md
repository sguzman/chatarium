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

Raw HAR and Flight Recorder exports remain private evidence. `chatarium-recorder snapshot-flight` remains the active evidence-ingestion path. The next active work is P3: acquire and encode read-side protocol evidence for conversation listing/fetching, then mirror selected remote conversations into the local durable model before enabling direct remote mutation.

Automated browser capture remains a longer-term goal rather than a prerequisite for protocol discovery. The existing Windows capture harness is parked; the active protocol workflow is cross-platform and Linux-friendly.

See [`docs/ROADMAP.md`](docs/ROADMAP.md), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/CAPTURE_HARNESS.md`](docs/CAPTURE_HARNESS.md), [`docs/IMPORT_BRIDGE.md`](docs/IMPORT_BRIDGE.md), [`docs/HUMAN_QA.md`](docs/HUMAN_QA.md), and [`protocol/README.md`](protocol/README.md).

## Non-goals

Chatarium is not a claim that ChatGPT exposes a supported public consumer API. It does not assume undocumented interfaces will remain stable, and it will not treat browser internals as timeless contracts. It also does not aim to bypass authentication, access controls, rate limits, or anti-abuse mechanisms.

## Development

The workspace is Rust-first. Some legacy capture/bootstrap machinery is Windows-specific, but protocol observation, ingestion, typed interpretation, and the durable core are not architecturally Windows-bound. The desktop shell uses `eframe`/`egui`; asynchronous and blocking work must remain off the render thread. Protocol captures are data, not hand-maintained folklore: when behavior changes, preserve a new observation and adapt against it.

The repository is intentionally documentation-heavy because the hardest part of this system is not drawing a chat window. It is maintaining epistemic clarity across a mutable remote service, unreliable transport, local persistence, and recovery.
