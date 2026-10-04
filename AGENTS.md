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

## Product-scope guard

- `docs/PRODUCT_VISION.md` records the intended long-term product shape. Do not silently narrow Chatarium to a single-chat API client merely because current milestones are focused on protocol reliability.
- Preserve future architectural room for multiple ChatGPT sessions, a user-designated master/controller -> worker relationship, explicit lifecycle/control messages, MCP/tool routing, and an egui supervisory policy surface.
- Cross-session/tool automation must remain visible, durable, attributable, and user-contestable; the user is above any master/controller session.
- Prefer explicit machine-readable completion/blocked/input-needed states over prompt-text heuristics that could produce unbounded mutual continuation loops.
- The existing XML-oriented MCP/tool envelope from the user's Braizen/ChatGPT-shim work is the preferred compatibility starting point when that schema is recovered. Until then, do not invent a replacement schema and attribute it to the user.

## Architecture

- Keep the egui render path cheap. Network, disk I/O, parsing, capture processing, reconciliation, and expensive transforms stay off the UI thread.
- Domain state belongs in `crates/core`; observed ChatGPT shapes belong in `crates/protocol`; persistence belongs in `crates/store`; presentation belongs in `apps/desktop`.
- Do not make the desktop app the source of truth for protocol knowledge.
- Prefer explicit state machines and typed uncertainty over booleans such as `sent = true`.

## Codex-owned QA / zero operator regression labor

- The principal is not Chatarium's manual QA runner. Routine browser/application validation has an operator QA budget of **zero**.
- Codex owns the engineering loop end to end: Git sync/status/diff, edit, build, launch/restart, QA-browser control, extension load/version verification, browser interaction, log/console/screenshot/trace collection, regression reruns, commit, push, and final evidence report.
- The canonical QA browser is **Playwright-managed bundled Chromium**, not Microsoft Edge, Google Chrome, or an Arch-installed Chromium package.
- The canonical persistent QA browser state is `~/.local/share/chatarium-qa-browser/`. It is Chatarium-owned private local state and must never be copied from the principal's normal browser profile.
- The principal's normal Microsoft Edge installation, profiles, windows, cookies, and browser data are completely outside the automation boundary. Do not install Playwright tooling into personal Edge and do not attach MCP/CDP/WebDriver automation to it.
- The retired UI-created Edge QA profile is not a security boundary and is no longer a Chatarium automation target. A second profile under one Edge user-data root is insufficient isolation.
- Preferred browser control is Playwright launching its own persistent Chromium context with the dedicated QA user-data directory. The Chatarium MV3 bridge is loaded automatically from the repository into that Chromium context.
- The principal may use the same headed QA Chromium manually through a durable launcher such as `chatarium-qa-browser` for unavoidable account login/MFA. The browser profile persists so ordinary authentication is not repeated every run.
- Do not claim that Codex can control a browser/native GUI merely because it is visible in the same Hyprland workspace. A control surface is usable only after its Playwright/CDP/CLI/native-automation path has been demonstrated. Track the matrix in `docs/QA_CONTROL_SURFACE.md`.
- If browser control is missing or broken, fixing/bootstraping that automation is the next engineering task. Do not convert missing automation into instructions for the principal to click, reload, watch logs, copy output, take screenshots, or "try again."
- If a native egui action is repeatedly needed, add a CLI/test hook, typed localhost/test RPC, or other deterministic machine-controlled surface. Compositor/native input automation is a fallback; human repetition is not.
- Human involvement is reserved for a true identity/consent boundary that automation cannot satisfy, such as CAPTCHA, MFA, or an explicit security permission requiring the account holder. Stop with a precise `AUTH/CONSENT BLOCKED` state; do not package the remaining engineering work as a human QA checklist.
- Never ask the principal to copy cookies, authorization values, CSRF/session tokens, browser profile databases, or other reusable credentials.
- Repeated QA for a previously observed failure shape must become an automated regression test.

## Scope boundaries

- For new conversations, Chatarium targets the user's eligible ChatGPT plan through OpenAI's official Sign in with ChatGPT + Responses route; a separate OpenAI API-key subscription is not the architectural premise. Undocumented chatgpt.com web-client protocol work is now optional compatibility/import research rather than the primary chat transport.
- Do not bypass authentication, access controls, rate limits, anti-abuse systems, or other service protections.
- Do not add mechanisms whose purpose is credential theft, session hijacking, or access to another user's account.

## Development environment

- Rust-first, Windows-first initially. The official Sign in with ChatGPT DevKit is an intentional trusted Node sidecar exception for OAuth/model discovery/Responses streaming rather than a reason to migrate the desktop out of Rust.
- Salvador's Node package manager is **pnpm**. Prefer pnpm in local setup/bootstrap paths and user-facing commands. Do not default to npm in handoffs. If an upstream artifact requires npm-specific lockfile semantics, invoke that compatibility path through pnpm rather than requiring Salvador to install or operate npm.
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
