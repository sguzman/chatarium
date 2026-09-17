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

## UI work

Keep egui rendering cheap. The app should display immutable/cheap snapshots of state and emit commands. Persistence, networking, capture parsing, indexing, and reconciliation run elsewhere.

## Commit shape

When practical, keep evidence + documentation + adaptation atomic. A good protocol maintenance commit can say exactly which observation changed and how the implementation was updated in response.
