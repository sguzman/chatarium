# Chatarium agent instructions

Chatarium is a reliability project first and a UI project second.

## Visual / Codex tracking namespace

- Permanent project color: **🟧 ORANGE — Chatarium**.
- Every Codex prompt gets a prompt color distinct from the immediately previous Chatarium Codex prompt. Prompt colors are transient; the project color never changes.
- Every Codex final report must begin with both color lines and end with the same two color lines so the result is visually attributable even when several projects are running concurrently.
- The director must explicitly state goal state. Use one of: `START NEW GOAL`, `CONTINUE CURRENT GOAL`, `CORRECTION / FOLLOW-UP`, or `DO NOT START CODEX`.
- Never silently convert a follow-up into a new goal.
- When a new goal is required from the principal, say exactly: **I NEED A NEW CODEX GOAL NOW**. Do not imply or hint that a new goal is needed.

## Authority and provenance

- Treat `protocol/` as empirical evidence about the observed ChatGPT web client.
- Never invent endpoint fields, event names, authentication behavior, or compatibility claims to fill a gap. Mark unknowns explicitly.
- A protocol change should normally add or reference an observation before adapting implementation code.
- Preserve raw evidence outside Git when it contains secrets or private content; commit only sanitized fixtures and metadata.
- Never commit cookies, bearer tokens, CSRF material, session identifiers that grant access, private conversation bodies, or other reusable credentials.
- Exact canonical experiment text and action definitions are evidence-bearing inputs. Do not silently rewrite, normalize, improve, or substitute them.

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

## Human QA budget

- Human QA is a scarce fallback for observations the program cannot reasonably obtain itself, not a substitute for automation.
- If code can create a temporary directory, launch a controlled process, insert synthetic text, collect logs, hash, sanitize, import, diff, validate, or clean up test state, implement that automation instead of writing a checklist for the operator.
- Prefer one command and one returned artifact over multi-command operator choreography.
- Never ask the operator to copy cookies, authorization values, CSRF/session tokens, browser profile databases, or other reusable credentials.
- A human handoff must state exactly what will be installed, launched, touched, persisted, and returned before the operator acts.
- Repeated QA for a previously observed failure shape should normally become an automated regression test.

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
