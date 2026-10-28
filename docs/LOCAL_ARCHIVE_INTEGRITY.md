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
are now included in the SHA-256-verified local backup manifest and restored
alongside the authoritative journal. Source filename, path structure, regular
file type, and SHA-256 byte integrity are verified. Unknown import-directory
entries, symlinks and unlisted source archives fail backup verification;
restore only copies manifest-approved paths.

Restore re-hashes every staged file against its verified manifest entry
before publication, closing the gap between initial backup verification and
the later filesystem copy. A failed final staging-to-target rename triggers
an attempted rollback of the original target from its safety snapshot. If
that rollback itself fails, an explicit error identifies the retained safety
directory; it is never silently deleted. Failed staging is cleaned up.
Synthetic tests cover both staged-byte mismatch and publication rollback.

Archive integrity also verifies that every recorded Flight Recorder
quarantine decision still has its original, SHA-256-matching source archive.
An absent or tampered source is a hard error, not a recoverable warning.
A backup preserves the raw source evidence and journal, **not** an active
filtering state in user-facing projections; Q30 remains a separate boundary.

## Single-writer journal ownership

Every `JsonlEventStore::open` acquires **two** exclusive SQLite leases:
the existing `journal.jsonl.writer-lock.sqlite3` alongside the journal and a
stable, hashed `.chatarium-archive-writer-*.sqlite3` sibling in the archive
directory's parent. Both leases are held until the writer is dropped. Desktop,
importer and archive restore share the parent lease, so a restore cannot move
an actively written archive, nor can a new writer enter while a restore
replaces its directory. The older in-directory lease is retained, and restore
checks it for an already-running old-version writer before publication.
**Stop older Chatarium versions before restoring:** older binaries do not
honor the parent lease, so a later-starting old writer cannot be serialized.

`restore_backup` verifies the source, acquires the **parent** lease before
staging and publication, and refuses active writers and concurrent restores.
The parent lease is not moved during directory rename, which is essential for
safe replacement on Windows. Journal reads through `inspect_jsonl_journal`
remain available and never acquire a writer lease.

SQLite releases the lease automatically after process termination; the lock
database is non-authoritative runtime coordination state, not a projection
or source archive. **Do not delete the lock database as an unlock mechanism.**
Doing so could let two processes hold leases on different inodes.

The store also offers `JsonlEventStore::open_strict`, which acquires the
same exclusive lease but refuses a missing journal or torn tail **without
repairing, truncating, or creating the journal**. Evidence-sensitive mutation
commands must use this mode and revalidate their source while owning the lease.

The importer exposes `quarantine-record` for explicit durable decisions.
It opens the journal strictly and re-runs source preflight and replay admission
under the exclusive writer lease. Recording is idempotent, and re-import of the
same source is refused. A read-only preflight SHA alone is never a writer lock.
After restart, desktop/worker effective views honor the decision, while the
authoritative journal and physical SQLite cache continue to retain all events.
A full Q30 reader audit and Q40 recovery acceptance remain outstanding.

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
exposing its source records. Neither query deletes or mutates SQLite rows. These SQLite APIs are **opt-in**; desktop and worker effective replay are
integrated, but other direct raw readers and final recovery testing still
require audit. Do not equate a raw SQLite query with an effective view.

## Nonblocking journal bootstrap

Desktop startup now displays a lightweight loading frame while a named
background worker opens the authoritative JSONL journal and constructs the
validated `EffectiveEventStore`. This moves full raw journal disk IO and
quarantine replay out of the GUI constructor/render thread. Once validated,
the worker transfers ownership of the live single-writer lease to GUI startup;
the GUI hands that same store to the persistence worker, never reopening it.
Invalid or missing evidence fails closed to no-persistence UI state, not an
unfiltered fallback. Background loading is tested with an exclusive-writer
handoff and monotonic append sequence. Remaining GUI-side catalog/cache
construction is separate work and may still be expensive for huge archives.

## Active desktop/worker effective replay

The desktop now wraps the authoritative JSONL writer with a validated
`EffectiveEventStore` on startup. It validates the entire raw journal and
quarantine audit before recovery, catalog replay or background worker startup.
Its `EventStore::events()` is a borrowed read-side snapshot of the effective
history, preserving original sequence numbers and keeping native and other
source records. Appended native events are still written only through the
underlying authoritative journal. The wrapper refuses changes to any
quarantined source scope and refuses a quarantined source provenance payload,
and it reserves quarantine decisions for the importer command.

If validation fails, startup does not hand an unfiltered journal to the
desktop or persistence worker. All writes still use the same exclusive
single-writer lease.

The `authoritative_events()` API remains available only for integrity and
physical projection operations. The SQLite `effective_events_verified`
method is separate and requires matching the complete authoritative journal.
**The current raw SQLite query methods and any external direct consumers
remain physical evidence APIs, not effective views.** Q30 remains incomplete
until all user-facing readers have validated effective access; do not treat
raw-query APIs as cleaned data.

## Source quarantine decision audit and effective replay

The journal event `import_source_quarantine_recorded` is a versioned, unscoped
provenance fact for a specific Flight Recorder source SHA-256 and preflight
journal SHA-256. `check_archive` replays and validates its recorded event
span and import framing. It rejects duplicate decisions, forged counts,
shared imported/native scopes and subsequent source re-import or scope reuse.
The source journal events remain in place, with their original sequences.

The importer records this decision under a strict exclusive writer lease after
fresh source verification. Validated desktop/worker event replay excludes the
source on restart; no original record is deleted or renumbered. Physical journal
and SQLite evidence APIs remain intentionally unfiltered, and stale SQLite
effective reads fail closed. Q30 still requires complete read-surface and
nonblocking-startup review; Q40 requires end-to-end crash/restart verification.
An audit fact alone is not proof that every external consumer has refreshed.

For disaster recovery, stop the desktop, run `archive-verify` against a local backup, restore it
to the configured data directory only after verification succeeds, then run `archive-check` and
rebuild local search/read projections. No network or browser step is part of recovery, and old
backups are never deleted automatically.

## Attested SQLite effective read facade

The physical SQLite cache preserves every original event and its sequence.
`SqliteProjection::effective_read_view_verified(raw_events)` validates that
the *entire* cache matches the supplied authoritative journal (including
quarantine decisions), then constructs an owned `SqliteEffectiveReadView`.
Its event, exact-scope and event-kind query methods return **only effective
records**, without rewriting source sequences or querying excluded physical
rows. Stale/divergent caches fail closed; after new journal appends callers
must request a fresh attested view.

Raw `events()`, `events_for_scope()`, `events_of_kind()`, and physical
turn projections remain evidence APIs. They must not be used directly as
quarantine-aware user-facing views. Desktop/worker and external reader
integration still need comprehensive acceptance testing.
