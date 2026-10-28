# Local archive integrity and recovery

Chatarium's append-only `journal.jsonl` is the authoritative local archive. The history cache is
a rebuildable convenience projection. Archive maintenance is local-only and never starts a
browser, authentication probe, or remote request.

## Local data ownership

- Authoritative: `journal.jsonl`, including durable authored events, provenance, queue lifecycle,
  and validated mirror snapshots.
- Derived/rebuildable: `remote-history-cache.json` is retained in backups for offline catalog
  continuity and is structurally revalidated; search indexes, SQLite projections, and queue views
  are rebuilt from the journal plus this cache.
- Disposable UX state: reader positions, search selection/query state, and other presentation
  state are not authoritative and are not required for archive recovery.

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

`archive-check` reads the journal without repairing it. It reports `HEALTHY` only after the
complete configured suite passes; a recoverable unterminated final fragment reports `WARNING`.
Malformed complete records, unknown event kinds, sequence gaps, invalid cache data, conflicting
identities, invalid snapshots, and inconsistent queue state fail closed as `INVALID`. The mirror,
transcript, identity, selection, and read-observation projections are all replayed before a
healthy report is possible.

Backup verification checks every manifest size/hash and then repeats the archive audit against
the package. Restore validates the package in isolation, stages and audits a candidate directory,
retains any existing active directory as a timestamped pre-restore safety copy, and only then
atomically installs the candidate. Existing backups are never deleted automatically.

The desktop Diagnostics panel provides CHECK ARCHIVE, CREATE BACKUP, VERIFY BACKUP, and RESTORE
BACKUP. Restore always requires an explicit confirmation and preserves the previous active data.

## Source quarantine decision audit (non-operative)

The journal event `import_source_quarantine_recorded` is a versioned, unscoped
provenance fact for a specific Flight Recorder source SHA-256 and preflight
journal SHA-256. `check_archive` replays and validates its recorded event
span and import framing. It rejects duplicate decisions, forged counts,
shared imported/native scopes and subsequent source re-import or scope reuse.
The source journal events remain in place, with their original sequences.

This **does not yet implement quarantine**. No import command can record
such a decision, and ordinary GUI/worker/SQLite projections do not yet filter
on it. The apply path still needs an exclusive writer boundary, fresh source
verification, and consistent source-exclusion projections. Treating an
unapplied audit fact as proof that imported content disappeared is prohibited.

For disaster recovery, stop the desktop, run `archive-verify` against a local backup, restore it
to the configured data directory only after verification succeeds, then run `archive-check` and
rebuild local search/read projections. No network or browser step is part of recovery, and old
backups are never deleted automatically.
