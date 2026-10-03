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
