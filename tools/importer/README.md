# Chatarium importer

The importer moves already-controlled evidence into Chatarium's durable local journal.

## Commands

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
