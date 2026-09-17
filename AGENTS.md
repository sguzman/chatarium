# Chatarium agent instructions

Chatarium is a reliability project first and a UI project second.

## Authority and provenance

- Treat `protocol/` as empirical evidence about the observed ChatGPT web client.
- Never invent endpoint fields, event names, authentication behavior, or compatibility claims to fill a gap. Mark unknowns explicitly.
- A protocol change should normally add or reference an observation before adapting implementation code.
- Preserve raw evidence outside Git when it contains secrets or private content; commit only sanitized fixtures and metadata.
- Never commit cookies, bearer tokens, CSRF material, session identifiers that grant access, private conversation bodies, or other reusable credentials.

## Reliability invariants

- User-authored text is durably committed before any remote send attempt.
- Incrementally received assistant output is durably persisted.
- Ambiguous transport outcomes remain ambiguous until reconciled. Do not reinterpret a timeout or disconnect as a confirmed remote failure.
- Recovery logic must be idempotent where possible and must not silently duplicate a user turn.
- The event journal is append-oriented. Mutable projections may be rebuilt from durable events.

## Architecture

- Keep the egui render path cheap. Network, disk I/O, parsing, capture processing, reconciliation, and expensive transforms stay off the UI thread.
- Domain state belongs in `crates/core`; observed ChatGPT shapes belong in `crates/protocol`; persistence belongs in `crates/store`; presentation belongs in `apps/desktop`.
- Do not make the desktop app the source of truth for protocol knowledge.
- Prefer explicit state machines and typed uncertainty over booleans such as `sent = true`.

## Scope boundaries

- Chatarium targets the consumer ChatGPT service used through the official web client; public OpenAI API billing is not the architectural premise.
- Do not bypass authentication, access controls, rate limits, anti-abuse systems, or other service protections.
- Do not add mechanisms whose purpose is credential theft, session hijacking, or access to another user's account.

## Development environment

- Rust-first, Windows-first initially.
- Prefer normal Cargo dependencies. For project tooling on Windows, document Scoop commands rather than silently installing tools.
- Do not download or execute opaque external payloads as part of build/bootstrap scripts.
- Keep documentation current when architectural or protocol assumptions change.

## Change discipline

A substantial protocol-facing change should answer:

1. What was observed?
2. In which snapshot/revision?
3. What changed from the previous observation?
4. What does the implementation now assume?
5. How is that assumption tested?
6. What happens when the assumption fails at runtime?
