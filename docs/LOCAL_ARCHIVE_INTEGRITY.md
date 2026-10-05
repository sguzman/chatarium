# Local archive integrity and recovery

Chatarium's append-only `journal.jsonl` is the authoritative local archive. The history cache is
a rebuildable convenience projection. Archive maintenance is local-only and never starts a
browser, authentication probe, or remote request.

The QA binary exposes structural maintenance commands:

```text
chatarium-qa archive-check
chatarium-qa archive-backup --output PATH
chatarium-qa archive-verify PATH
chatarium-qa archive-restore PATH --data-dir PATH
```

Reports contain counts, sequence metadata, warning/error counts, and queue state counts only.
They do not print conversation titles, message text, remote identities, raw response bodies,
cookies, or credentials. Backup manifests contain relative file names, byte sizes, SHA-256
hashes, and structural archive counts; backup contents remain local user data and are not Git
fixtures.

`archive-check` reads the journal without repairing it. An unterminated final fragment is
reported as a warning, while malformed complete records, unknown event kinds, sequence gaps,
invalid cache data, conflicting identities, invalid snapshots, and inconsistent queue state fail
closed. The mirror, transcript, identity, selection, and read-observation projections are all
replayed before a report is successful.

Backup verification checks every manifest size/hash and then repeats the archive audit against
the package. Restore validates the package in isolation, stages and audits a candidate directory,
retains any existing active directory as a timestamped pre-restore safety copy, and only then
atomically installs the candidate. Existing backups are never deleted automatically.

The desktop Diagnostics panel provides CHECK ARCHIVE, CREATE BACKUP, VERIFY BACKUP, and RESTORE
BACKUP. Restore always requires an explicit confirmation and preserves the previous active data.
