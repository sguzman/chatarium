# Development

## Toolchain

Chatarium is Rust-first and Windows-first initially. The repository tracks the stable Rust channel with `rustfmt` and `clippy` components.

Use normal Cargo dependency resolution. Do not add bootstrap scripts that silently download or execute opaque binaries. If extra Windows tooling becomes necessary, document the dependency and prefer a reproducible Scoop installation path.

## Baseline commands

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

## Automated QA ownership

For browser-facing and integration work, Codex owns the complete local QA/debug/Git loop. The principal is not a manual regression runner. Use the Playwright-managed bundled Chromium QA browser with persistent state at `~/.local/share/chatarium-qa-browser/`; do not automate the principal's normal Edge installation. Follow [Codex-owned QA workstation](CODEX_QA_WORKSTATION.md) plus [Human QA protocol](HUMAN_QA.md).

If the automation path itself is missing, build or repair it before requesting another live validation.

## Protocol-facing work

Before changing code because ChatGPT behavior appears to have changed:

1. identify the smallest failing canonical flow;
2. capture the new behavior;
3. sanitize it;
4. create a new protocol snapshot;
5. structurally diff it against the last known-good observation;
6. document what is observed versus inferred;
7. adapt code and add/update fixtures;
8. test degraded/mismatch behavior as well as the happy path.

## Undocumented browser integration gate

The 2026-10-03 history-bridge incident established that this project cannot treat partial browser evidence as sufficient merely because implementation is convenient. See [the full postmortem](postmortems/2026-10-03-chatgpt-history-bridge.md).

For any undocumented consumer-web operation, **do not implement beyond scaffolding until a current request-complete capture exists**. Acceptable evidence is a raw HAR, CDP capture, or equivalent first-party trace from the exact target flow. If the evidence does not include a property needed to reproduce the request, that property is unknown.

Before implementation, record parity for:

- HTTP method, host, path, query keys, literal values, duplicates/order where observed;
- request body and content type;
- credentials/origin/referrer behavior where relevant;
- context-bearing headers such as account/workspace/project selection;
- challenge/Sentinel or other dependency-bearing request material;
- request ordering and prerequisite calls;
- expected status/content type/response schema;
- pagination and identity echoes.

Every observed request header relevant to the target flow must be classified as one of:

- semantic context;
- credential/private;
- anti-abuse/challenge context;
- incidental telemetry;
- unknown.

Unknown fields are not silently omitted and later called equivalent.

### Mandatory capture rule

When the product target depends on undocumented first-party behavior and current evidence is incomplete, the engineering response is:

> Current evidence is insufficient; capture is required.

Do **not** tell the operator that HAR/CDP evidence is unnecessary unless the repository already contains equivalent current evidence and the issue names it explicitly.

### Browser prototype promotion rule

Userscripts are prototype/observation tools by default. They may become a runtime dependency only after they pass:

- a documented execution-world model;
- deterministic browser-to-desktop roundtrip;
- browser-version compatibility testing;
- reliable background/worker lifetime;
- first-party request-context parity;
- end-to-end integration diagnostics;
- one focused live validation without speculative manual debugging.

If a browser prototype fails two focused live validations at the same architectural boundary, stop and escalate the architecture instead of requesting another speculative human test.

## UI work

Keep egui rendering cheap. The app should display immutable/cheap snapshots of state and emit commands. Persistence, networking, capture parsing, indexing, and reconciliation run elsewhere.

## Commit shape

When practical, keep evidence + documentation + adaptation atomic. A good protocol maintenance commit can say exactly which observation changed and how the implementation was updated in response.
