# Nix packaging — Phase A (candidate)

This is the initial Linux x86_64 slice of the [agentic workstation roadmap](AGENTIC_WORKSTATION_ROADMAP.md). The existing Windows and Cargo development paths are unchanged.

## Build contract

- Nixpkgs, rust-overlay and crate2nix source revisions are pinned explicitly in the flake. The toolchain comes from rust-toolchain.toml (currently Rust 1.98.1).
- crate2nix generates the workspace graph from Cargo.lock with that Cargo toolchain. Import-from-derivation is deliberate; Rust crates must be independently cached rather than using an all-dependencies Cargo build.
- The initial output is named desktop-unwrapped, not default: it is a compilation acceptance target, not a packaged runnable desktop.
- The lockfile is not yet committed. The first CI run must generate it and retain the actual output for review; no fabricated NAR hashes.
- Local development uses the pinned toolchain and Linux GUI libraries. Existing host Secret Service and graphics drivers remain host responsibilities. No system packages are installed automatically.

## Authentication sidecar packaging plan

The current Rust bridge finds its bootstrap and bridge scripts relative to CARGO_MANIFEST_DIR. Its normal launch can invoke Git and the pinned upstream package-manager bootstrap. Neither is acceptable in an immutable installed Nix package.

The next slice must package the existing, exact DevKit submodule revision (not upgrade it), preserve the upstream package-lock, compute and verify actual dependency hashes, and provide the installed bridge with a trusted explicit runtime location. The packaged launch must not run Git or package installation, and must fail closed if its runtime is missing. Existing source-tree development behavior must remain intact. The credential boundary, Secret Service and removal of API-key environment variables remain unchanged.

## Verification

The Nix CI candidate generates a real flake.lock, verifies the toolchain, and builds desktop-unwrapped. It uses the public repository and synthetic build data only, never a private user archive or account.

Pending: lockfile read-back/commit, first build result, native runtime wrapper, immutable authentication sidecar, offline packaged smoke/GUI acceptance. Phase A earns no roadmap points until all its acceptance criteria pass.
