# Chatarium capture harness

`chatarium-capture` is the Windows-first automation boundary for controlled observations of the official ChatGPT web client.

Current status: **substrate only**. The experiment model, dedicated-profile safety rules, Edge discovery, and read-only `doctor` command exist. Live CDP capture is intentionally not enabled until the transport/finalization implementation lands and passes review.

## Commands

```text
chatarium-capture doctor
chatarium-capture init
chatarium-capture run C00-idle-load
chatarium-capture run C03-send-text
```

At the current scaffold stage, `init` and `run` fail explicitly without launching a browser or mutating remote state.

## `doctor`

`doctor` is read-only. It reports:

- harness version;
- derived Chatarium-owned Edge profile path;
- whether that path is distinct from known default Edge profile trees;
- a discovered Edge executable, if present in standard Windows locations;
- whether the dedicated profile already exists;
- embedded canonical experiment definitions.

Run through Cargo during development:

```powershell
cargo run -p chatarium-capture -- doctor
```

## Invariants

- Never use the user's default Edge profile.
- Never copy/import cookies or credentials from another browser profile.
- Exact synthetic experiment text is versioned under `protocol/experiments/` and embedded in the harness build.
- Unknown experiment IDs fail; the program does not improvise a new experiment.
- A mutating experiment must not automatically retry after an ambiguous remote outcome.
- Live capture must journal incrementally before the project treats it as usable evidence.
- Portable artifacts and private local evidence are different products.

See `docs/CAPTURE_HARNESS.md` for the authoritative v0.1 contract and GitHub issue #1 for implementation acceptance criteria.
