# Chatarium importer

The importer moves already-controlled evidence into Chatarium's durable local journal.

## Commands

`openai-export <conversations.json> <data-dir>` imports an extracted ChatGPT account-export conversation array as historical local snapshots. The filename may also be a sharded `conversations-NNN.json` file; the command keys off the top-level JSON array rather than the filename.

The account-export path is intentionally archival rather than a claim of live synchronization:

- the exact input file is archived content-addressably under the local data directory;
- each raw conversation object is also archived content-addressably without flattening its `mapping` tree/branches;
- exported remote conversation identity, title/timestamps, and `current_node` are preserved when present;
- the same remote identity keeps one stable Chatarium-local conversation identity across later exports;
- an unchanged conversation snapshot is idempotent, while a changed later snapshot is appended under the same local identity;
- legacy `id` and newer `conversation_id` identity fields are accepted when unambiguous;
- import does **not** create a live remote binding and does not imply that Chatarium can currently read or write that chatgpt.com thread.

This command currently expects an extracted JSON conversation array, not the outer account-export ZIP/container.

`flight-recorder <export.json> <data-dir>` imports the existing browser Flight Recorder export path used for recovered local conversational evidence.

`read-fixture <sanitized-fixture.json> <read-index> <data-dir>` imports exactly one explicitly selected C01/C02 read observation from a publication-safe fixture.

The read-fixture path is intentionally strict:

- it accepts only the controlled C01/C02 experiment identifiers;
- the caller must provide the zero-based `read_responses` index;
- Chatarium never guesses which captured endpoint has semantic meaning;
- raw body text, headers, cookies, auth material, query values, and request bodies are rejected;
- body strings/scalars must already be reduced to publication-safe placeholders;
- the selected metadata is validated by `chatarium-protocol::read::ReadObservation`;
- the exact sanitized fixture is archived content-addressably;
- durable provenance records the fixture SHA-256 and selected read index;
- importing the same fixture/index again is idempotent.

This command does not promote a read flow to a validated semantic baseline. Compatibility remains governed by the named protocol baseline in `chatarium-protocol`.


## Source-attribution audit (read-only)

`source-audit <source-sha256> <data-dir>` identifies journal events carrying recognized importer provenance for the exact source-file SHA-256. It requires a pre-existing, explicitly named data directory and an intact newline-terminated journal. It uses the non-mutating journal inspector, not the importer's repair-capable journal open path.

The report gives counts by importer source and event kind, the total journal event count and the first/last matching event sequence. It does **not** show conversation bodies, event payloads or titles. Recognized sources are Flight Recorder exports, account-export historical snapshots and sanitized read fixtures.

**This is not quarantine.** A source match is not yet proof that an event can safely be excluded: later native events or imported history may depend on it. The command never appends, removes, rewrites, repairs, or rebuilds local data. In particular, it refuses a torn final journal record rather than truncating it. Do not manually delete matching JSONL lines; the journal is immutable evidence, while SQLite and other read-side projections are rebuildable. A safe source-exclusion transaction with durable audit provenance remains separate work tracked by issue #3.


## Fail-closed quarantine preflight

`quarantine-preflight <source-sha256> <data-dir>` is a more restrictive read-only step before any exclusion or quarantine. It currently supports the Flight Recorder format used by the original accidental-import incident; unrelated account-export and read-fixture sources are deliberately blocked.

It verifies the exact content-addressed source-file SHA-256 against the existing archived bytes, its export format/version, recognized journal event kinds, unique event keys, complete import start/end framing, expected semantic count, and the absence of foreign events under any affected exact journal scope. Unknown/mixed provenance claims and conflicting scope ownership are explicit blockers. A blocked result returns a nonzero exit status for automation, after listing fixed blocker codes. The report includes a journal SHA-256 that downstream implementation can bind to, avoiding a stale preflight. It contains only hashes, counts, and fixed blocker codes, never imported text or account identifiers.

A report of **CANDIDATE** is **not authorization to quarantine**. It does not prove that cross-scope references, later tool/routing records, or application projections are safe to exclude; no mutation or projection rebuilding is implemented by this command. No authoritative journal data or SQL cache changes are made, and torn/corrupt journals fail closed. Any future apply operation must re-run these checks against the exact unchanged journal and implement a durable, dependency-aware event-sourced exclusion rather than deleting matching JSONL records or merely editing SQLite.
