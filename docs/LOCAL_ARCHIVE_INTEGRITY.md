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

## Backup and restore of source-quarantine evidence

Content-addressed archives under `imports/flight-recorder/<sha256>.json`
are now included in the signed-by-hash *local backup manifest* and restored
alongside the authoritative journal. Source filename, path structure, regular
file type, and SHA-256 byte integrity are verified. Unknown import-directory
entries, symlinks and unlisted source archives fail backup verification;
restore only copies manifest-approved paths.

Archive integrity also verifies that every recorded Flight Recorder
quarantine decision still has its original, SHA-256-matching source archive.
An absent or tampered source is a hard error, not a recoverable warning.
A backup preserves the raw source evidence and journal, **not** an active
filtering state in user-facing projections; Q30 remains a separate boundary.

## Single-writer journal ownership

Every `JsonlEventStore::open` now acquires an exclusive SQLite transaction in
`journal.jsonl.writer-lock.sqlite3`, alongside its journal. It holds that
lease until the writer is dropped. Desktop and importer writers share this
entrypoint, so concurrent journal opens refuse with a lock error rather than
racing append sequence numbers or tail recovery. Journal reads through
`inspect_jsonl_journal` remain available and never acquire a writer lease.

SQLite releases the lease automatically after process termination; the lock
database is non-authoritative runtime coordination state, not a projection
or source archive. **Do not delete the lock database as an unlock mechanism.**
Doing so could let two processes hold leases on different inodes.

The store also offers `JsonlEventStore::open_strict`, which acquires the
same exclusive lease but refuses a missing journal or torn tail **without
repairing, truncating, or creating the journal**. Evidence-sensitive mutation
commands must use this mode and revalidate their source while owning the lease.

The importer exposes `quarantine-record` for explicit, durable,
**non-operative** source quarantine decisions. It opens the journal strictly,
then re-runs the source preflight and replay admission while holding the
exclusive lease. Repeated recording is idempotent; source re-import is refused.

An active source-quarantine apply operation is *not* implemented yet.
No desktop, worker, or SQLite projection currently excludes the recorded
source's events. A read-only preflight journal SHA by itself cannot prevent
concurrent changes; use the strict writer lease for any future operation.

## Replay-verified effective event selection (opt-in)

`EffectiveJournalView::from_raw` selects effective journal references
without editing the raw append-only evidence or renumbering source sequences.
It first replays and validates all quarantine decisions and rejects malformed
or noncontiguous input. The resulting excluded-sequence set and visible
events are distinct from the original evidence view. A decision that cannot
be verified is an error, not permission to hide source data.

SQLite's `SqliteProjection::events()` continues to return the **full raw**
materialization. Its separate `effective_events_verified(authoritative_events)` method
first requires byte-equivalent logical rows to the supplied raw journal
snapshot, then applies the same replay-verified exclusion. This prevents a
stale SQLite cache from silently omitting a newer quarantine decision and
exposing its source records. Neither query deletes or mutates SQLite rows. These are **opt-in APIs**, not
an active desktop-wide cleanup: Q30 still requires consistent integration
through GUI, worker, recovery and other read surfaces. Do not claim imported
content is hidden until those integrations and regression tests are verified.

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
