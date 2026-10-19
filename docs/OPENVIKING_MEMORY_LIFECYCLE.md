# OpenViking memory lifecycle for Chatarium

Status: **in progress**. Fixed scoped MCP transport, host-triggered
checkpoint detection, bounded model summarization, a private durable outbox,
create-only OpenViking writes and exact read-back verification are implemented.
Confirmed checkpoint retrieval now automatically prefetches for the active
conversation on a background worker and freezes provenance-marked evidence
into authored Send context. Live verification against the user's running server,
relevance-aware cross-conversation retrieval and semantic history compaction
remain pending. A bounded 128 KiB inference context window is now wired to
live dispatch and its inspector.
The user must not have to remind an agent to save memories. Merely offering
an MCP tool or adding an agent prompt is not completion of this feature.

## Architectural boundary

Chatarium owns the complete, append-only local conversation journal, the
inference context budget, user/worker identity, permissions, and lifecycle
events. The user's existing OpenViking service at
`http://127.0.0.1:1933/mcp` is the optional *shared* external memory and
retrieval backend. Do not launch or install a duplicate OpenViking service or
add a second local embedding/database pipeline. Do not re-enable paused
ChatGPT website mirroring or substitute a new inference transport.

OpenViking is never the authoritative project repository: GitHub and files
remain the source of current code, issues, and documentation. The journal
remains the source of conversation truth. Memory entries are compact
navigation and historical evidence, not privileged instructions.

The installation was reported ready externally. Chatarium has a mock-tested fixed MCP adapter but has **not**
connected to or authenticated against this host in a live test;
the actual installed server's `tools/list` and authentication mode must be
verified before enabling provider operations. A localhost URL is not proof
that authentication is disabled.

## Version-sensitive external write semantics

This integration requests `write(mode="create", wait=false)` and durably
dispatches **at most one** such write per checkpoint. OpenViking **v0.5.0**
[documents `create` as exclusive](https://github.com/volcengine/OpenViking/blob/v0.5.0/docs/en/api/12-content.md):
an existing target returns 409. However the [upstream main
documentation](https://github.com/volcengine/OpenViking/blob/main/docs/en/api/12-content.md)
currently describes `create` as a compatibility alias for `replace`.
These are materially different contracts. An at-most-once client dispatch
does **not** make a non-exclusive server operation safe against overwriting
a preexisting remote file.

The user's installed server version and exclusive-create behavior must be
confirmed in live integration QA before claiming non-overwrite guarantees
or finishing this milestone. The default read-only probe does not mutate
the server. The one-create synthetic probe verifies exact read-back, but
**does not** establish exclusive-create semantics. A separate explicit
`--verify-exclusive-create` diagnostic creates a unique disposable synthetic
checkpoint, verifies it, then intentionally attempts a second `create` on
**that synthetic file only** with different content. It requires an explicit
AlreadyExists/409 rejection AND re-verifies the original bytes. An alias that
overwrites existing content fails the test and may overwrite the disposable
synthetic QA file; no personal data or previous checkpoint is used. The
diagnostic never runs automatically against the user's service.
If the installed version aliases `create` to `replace`, it is
not suitable for this checkpoint-writing contract without an additional
collision-safe storage mechanism or a verified compatible server version.
Never silently treat a successful read-back as proof that no preexisting
object was overwritten.

## Non-negotiable: automatic behavior is application-owned

The model receives explicit memory discipline in its **agent bootstrap**,
but the model deciding to call an MCP tool is only a supplemental path.
Chatarium's host lifecycle enforces the following independent of the model:

1. **On meaningful task start or resumed work**, perform bounded,
   scope-authorized OpenViking retrieval and admit a small, provenance-marked
   context only when relevant. Repeated trivial messages do not mandate a
   search, and no unrelated scope may leak into a worker's context.
2. **After a successfully completed substantive task/milestone**, queue a
   durable checkpoint candidate without waiting for a user reminder.
3. **Before context compaction/rollover**, persist the checkpoint request
   before replacing any historical material in the inference input. Full
   history remains untouched in the Chatarium journal.
4. **At a bounded pending-work threshold** (e.g. completed turns or estimated
   unsummarized tokens), checkpoint incrementally even if no explicit task
   boundary was declared. The actual limits must be configured and tested.
5. **On normal conversation switch, archive, or application close**, flush
   already queued checkpoints if safe; do not promise a last-second network
   write on a crash, kill, or unplugged machine. Recover pending work from a
   local durable outbox on the next launch.

Each trigger must record its outcome: queued, generating, ready, dispatched,
confirmed, failed, or **uncertain**. Do not report a checkpoint saved solely
because an instruction was generated or a transport call was queued. Never
blindly retry an ambiguous external write.

## Landed: automatic metadata-only checkpoint scheduling

After a **normal, non-tool** assistant completion, Chatarium queues a
`QueueMemoryCheckpoint` request on the same persistence channel as its
terminal journal event. The worker checks that the corresponding completion
is already durable and contains at least 160 non-whitespace characters,
then atomically appends a metadata-only record to
`openviking-checkpoint-outbox.json` under Chatarium's own data directory.

The durable record contains only the owning conversation ID, turn ID,
terminal journal sequence and `pending_summary` state. It contains no
assistant/user message bodies, credentials or external MCP writes. A
duplicate same-turn request does not generate another entry. If writing
the outbox fails, the worker reports an error rather than claiming a saved
checkpoint. The original journal remains intact; a crash after a
successful enqueue preserves the request, and restart reconciliation scans journal completions to recover a request
that fell into the gap between terminal append and outbox enqueue. The first
enabled run uses a durable high-water baseline to avoid mass historical export.

A pending outbox entry is **not** a stored memory. Chatarium now runs a
bounded separate summarization inference when idle, using at most eight
recent authorized user/assistant messages and a 5,400-character source
budget. The model is explicitly told to extract only durable decisions,
milestones and open tasks, to avoid private secrets and unsupported claims,
or to return exactly `NO_MEMORY` for routine conversation.

Summaries are saved in Chatarium's local
`openviking-checkpoint-summaries/<conversation>/<turn>.md` directory.
On Linux these new files are created with owner-only permissions; their
contents never enter the metadata-only outbox. The summary transitions
`pending_summary -> ready` only after its own local write succeeds.
On Unix, the summary's directory and newly created ancestors are synced
before the outbox can mark it ready. If a create-new summary write fails,
the partial file is removed so a later attempt is not blocked; a preexisting
different summary remains protected against overwrite. A restart after an
exact completed summary but before its Ready transition can safely finish
the metadata transition without rewriting the source.
`NO_MEMORY` transitions to `skipped`, generating no external write.

The persistence worker then records `ready -> dispatched` **before**
launching a separate thread to call OpenViking `write`, with `mode=create`.
It reads the exact file back through OpenViking MCP and transitions to
`confirmed` only if the text matches exactly. Because the create uses
`wait: false`, verification allows up to five bounded **read-only**
attempts (exponential pauses of 250, 500, 1,000, and 2,000 ms) to tolerate
delayed materialization. No repeat `write` is permitted; if exact content
never appears, the result remains `uncertain`. A failed write, bridge
crash or interruption likewise becomes `uncertain`; the same external write is
**not** blindly replayed. Startup reconciliation re-queues journaled
completions that fell into the crash gap between journal append and outbox
enqueue. First-run activation writes a high-water baseline so existing
historical chats are not exported wholesale. The outbox does not silently
drop entries when its hard size limit is reached.

The memory pipeline is automatic by default when Chatarium is running
and the user's existing inference/model and OpenViking service are
available. `CHATARIUM_OPENVIKING_AUTOCAPTURE=0` in the Chatarium process
environment disables both automatic summarization and writes; the
agent-facing `write` MCP operation remains independently Ask-by-default.
Summary generation is throttled to at most one attempt per 90 seconds,
and already-ready local memories can be flushed without model sign-in.

**Not yet verified**: actual MCP connectivity/auth with the user's
running `127.0.0.1:1933` service, Studio inspection, selective startup
retrieval, context-budget compaction, background auto-wake worker memory
access, and long-term pruning of confirmed/skipped outbox items. The
current pipeline cannot yet solve large-model-context exhaustion by
itself. It adds separate bounded summarization inference requests and
should be measured for added usage and latency before enabling broader
memory scopes.

## Landed: scoped read-back retrieval and automatic current-chat admission

`apps/desktop/src/openviking_memory_delivery.rs` now provides
`retrieve_confirmed_for_conversation`, a host-owned read path for a single
conversation's newest **confirmed** checkpoint entries. It loads the durable
outbox, rejects foreign-conversation and non-confirmed records, selects at
most three entries ordered newest first, and caps whole-message retrieval
at 12 KiB total. It never truncates a checkpoint to fit a budget.

Each selected checkpoint is read by exact `viking://` URI, then compared
byte-for-byte with the local durable summary that was previously verified at
write time. Missing local evidence, oversized data, errors and remote drift
fail closed rather than being admitted as trusted history. Returned values
retain source conversation, source turn, source journal sequence and URI.

Chatarium now schedules this read automatically for the active local
conversation, outside the UI thread. A per-conversation cache and in-flight
claim prevent repeated fetches every frame; failures preserve ordinary chat
operation and are retried after a bounded delay. When a new checkpoint is
confirmed, the cache is invalidated, including reads that were already in
flight. Authored Send freezes verified memories with the other scoped context
sources; the exact-request inspector uses the same cache and Context Composer.
Memories remain **user-level historical evidence**, with the source URI, owner,
turn and journal sequence visible in its provenance. The original journal is
untouched.

**Current scope:** bounded lexical topic selection and directed,
explicitly granted project recall have been implemented. Unbounded semantic
search, full history compaction and end-to-end testing against the user's
actual running service remain incomplete. Owner-only retrieval never scans
other conversation namespaces; project recall requires a source-to-destination
grant with an independently validated project-directory binding. The composer now bounds outgoing context at 128 KiB UTF-8, keeps the
latest messages and critical control sources, prefers verified checkpoints to
old raw transcript, and marks excluded sources in the exact-request inventory.
This prevents unbounded input growth without deleting any journal events, but
is **not** a model-token guarantee or complete semantic summary of omitted turns.

**Checkpoint-aware history selection:** when a request exceeds that ceiling, the
composer preserves its mandatory controls and two latest durable messages,
then considers four recent durable messages, followed by the verified OpenViking
checkpoints (already ranked for topic relevance), before filling any remaining
budget with the other twelve recent messages and older evidence. This prevents
the previous sixteen-message window from consuming the entire budget ahead of
confirmed historical decisions. Each checkpoint is included in full or excluded
with a recorded budget decision; no journal event is removed. The newest
messages remain mandatory and a too-large checkpoint cannot force truncation.

This is **budget-aware history selection**, not a guarantee that a model has
semantically preserved every omitted turn. Gaps in checkpoint coverage and
model-authored summary accuracy still require separate end-to-end validation.

## Landed: bounded owner-scoped historical lookup

An OpenViking background read can now preselect historical checkpoints from
the **latest 64 locally confirmed checkpoints of the requesting conversation**
instead of only considering its three newest entries. The host scores only
the checkpoint's memory body against a bounded lexical query, never the
source's owner/turn/sequence header. It always retains the newest rolling
checkpoint for continuity, plus up to two older topic-relevant entries. With
no matches it retains the original three-newest behavior. Only the selected
three are remotely read and **byte-for-byte verified against their original
durable source** before they can become user-level inference evidence. The
total verified-memory budget remains 12 KiB. The selection reads only
on-disk summaries, off the egui thread; it does **not** search or download
another conversation's OpenViking namespace.

The foreground background-prefetch uses the current authored draft if one
exists at the time of the read, otherwise the conversation's last durable
user message. An inactive coordination worker uses the **exact authorized
wake directive** as its query before spending the at-most-once wake claim.
Each conversation now tracks the bounded lexical terms of the active query.
When its topic changes, both its owner checkpoint cache and explicitly
granted project cache are immediately invalidated, including older in-flight
results and obsolete retry delays. Foreground draft edits settle for 700 ms
before scheduling a new background read, avoiding network calls per keystroke;
whitespace, punctuation, case and word order do not trigger a re-fetch.
Inactive worker directives bypass typing debounce. If Send occurs before a
new result is verified, the request excludes old-topic evidence rather than
silently reusing it. This is explicit bounded lexical lookup, not a claim of
embedding-based or unrestricted semantic search.

Cross-conversation sharing now retains the newest confirmed checkpoint for
each of up to three selected explicitly granted sources and can fill any
remaining slots with older topic-matching checkpoints. The older candidate
scan is capped at 64 confirmed entries drawn only from those exact authorized
sources. Each candidate is validated against its local checkpoint header
before lexical scoring, and selected remote evidence must still match exactly.
No broad project-directory or OpenViking namespace scan is authorized; a
shared project directory alone is never permission to read memory.

The query is captured by the background retrieval command from the current
draft/last user message or the exact authorized coordination directive.
Changing the normalized topic refreshes both conversation and shared-history
selection after the draft settles; stale in-flight completions cannot populate
either cache. All retrieval remains bounded, verified and off the UI thread.

## Landed: query-aware ordering of verified memory context

At authored Send, and identically in the exact-request inspector, Chatarium
ranks **already read-back-verified** own-conversation and explicitly granted
project checkpoints against the current message. Local, bounded Unicode-aware
term overlap gives relevant content priority *only when the bounded Context
Composer must choose which whole checkpoint fits*. The composition pipeline
examines checkpoints from the end backward, so the most relevant is placed
last. No memory content is truncated, rewritten, elevated to instructions,
or silently admitted across conversations. The source URI, turn, journal
sequence and project-grant checks are unchanged and run **before** ordering;
revocations still fail closed at Send.

This is a first-stage lexical ranking, **not** full semantic retrieval:
it only considers the latest verified checkpoints already in the background
cache (currently at most three own and three explicitly shared). Queries
with no meaningful overlap preserve the former admission order. Broader
topic-aware discovery and exhaustive recall across historical checkpoints
remain outside this completed slice.

## Read-only recovery for uncertain writes

After a crash, a previously dispatched OpenViking create can have an
**uncertain** outcome. On startup, Chatarium now examines at most eight
uncertain entries, on a background worker. For each, it validates the local
durable checkpoint's exact owner, turn, journal sequence, and size, then makes
an OpenViking **read** call to its exact scoped URI. Only a byte-for-byte
response match permits the persistence worker to durably transition
`Uncertain -> Confirmed`. A missing remote object, mismatch, local corruption,
or offline service leaves the entry uncertain. There is **no automatic
write retry**, no overwrite, no mass scan, and no UI-thread network call.

This improves restart resilience without weakening the at-most-once create
guarantee. Local and remote failure modes remain observable through the
checkpoint state; confirm-only recovery invalidates the active memory cache.

## Rolling checkpoint continuity

Each new automatic checkpoint can now include the newest **earlier confirmed**
checkpoint for the **same conversation** as lower-trust historical evidence,
alongside the fresh bounded eight-message excerpt. The host reads the prior
checkpoint's private local source file only after the durable outbox says its
write was confirmed. Owner, exact source header, earlier journal sequence and
strict file size are validated; unknown, future, foreign, incomplete and
corrupted checkpoints never enter the summarizer. No extra localhost network
round trip occurs during summary preparation.

The model is instructed to retain still-valid decisions and unresolved work,
remove superseded details, and produce one new concise immutable summary.
A later checkpoint must not overtake an earlier in-progress same-conversation
checkpoint, so a confirmed state can be carried forward. If an older summary
is permanently blocked or its write is uncertain, the journal remains the
fallback source; Chatarium never invents or retries an uncertain write.

This improves long-running semantic continuity beyond the latest eight raw
messages. It is still **lossy summarization**, not full replay or proof of
complete fact preservation. End-to-end verification on the user's local
OpenViking service is still outstanding.

## What to save

A checkpoint is short, structured, and useful for resumption:
- source conversation and exact journal high-water sequence;
- active project/repo identifiers and source links;
- stable choices and constraints; material corrections;
- work completed with verifiable commit/issue IDs;
- open problems, blockers, next actions, and uncertainty.

Prefer an accurate 200-600 word checkpoint over a transcript dump. Skip
routine chatter, repeated instructions, generic tool output, retrieved memory
repetitions, unverified assertions, passwords, cookies, auth tokens, and other
secrets. No private venting text may be copied into public repositories.
The user does not have to type "remember this" for normal progress capture.
For a trivial turn with no durable content, a recorded no-op is valid.

The model can draft candidate summaries, but **Chatarium** assigns the
authoritative owner, project scope, source sequences, destination URI,
permissions, and idempotency key; the model must not author those fields.

## OpenViking operations

Start with verified server `tools/list`. For the reported CPU-only
embedding setup with VLM disabled, do **not** depend on `remember` to
extract memory autonomously: its one-shot memory-extraction job can be
accepted without producing a useful extracted memory. Write an explicit
already-summarized checkpoint through `write` (or carefully use `edit`
after a verified read). `remember` may become an optional later mode only
after a tested extraction result.

Use separate, scope-checked, versioned `viking://` text files with
stable source IDs. Do not overwrite a previously confirmed checkpoint
silently; record supersession, or write another immutable checkpoint. The
shared user/agent namespace is not, by itself, conversation isolation:
enforce scope both at tool dispatch and when retrieved content is admitted.
Do not enable broad `forget`, `add_resource`, `add_skill`, or access-control
operations by default.

Reads must have a hard result/token cap and deduplicate previously injected
material. Treat retrieved text as lower-trust evidence, not system or
developer instructions. A missing/unavailable OpenViking service must not
block ordinary conversation or erase the local journal.

## Agent bootstrap instructions (secondary defense)

Use the following guidance **only when Chatarium confirms that the memory
backend and required tool permissions are available**:

> OpenViking is the external memory backend. At the beginning of work that
> depends on past choices, use scoped memory retrieval and inspect the
> authoritative repository for current code. Preserve meaningful milestones,
> corrections and unresolved tasks in concise proposed checkpoints when the
> host requests one. Do not indiscriminately store messages or duplicate
> existing notes. Never claim a memory was saved unless Chatarium confirms
> the write. Do not cross conversation/project scopes or store credentials.
> Keep the model context small and cite original source references.

Agent text must **not** be substituted for the host-controlled checkpoint
triggers, durable queue, scope checks, and write acknowledgment.

## Transport and authority

Extend the existing local Streamable HTTP MCP broker for a fixed loopback
OpenViking endpoint; do not introduce an arbitrary URL proxy. Confirm the
installed MCP protocol version, `initialize` requirements, schemas, and
authentication against the *actual running service*. No secrets in the repo,
logs, journal, model prompt, or public configuration.

Default read-only `find`/`search`/`read` to a separately saved, explicit
scope permit. Checkpoint writes need a distinct user-authorized memory
capture policy and durable one-shot execution path; no blanket elevation of
all provider tools. Inactive auto-wake workers presently allow only built-in
coordination tools. Expand their memory read/write permissions only through
exact reviewed operations and worker-specific scopes.

## Acceptance criteria

- A normal multi-turn coding task produces an OpenViking checkpoint **without
  any user reminder**, and a new Chatarium conversation can retrieve it.
- Trivial exchanges do not generate noisy memories or repetitive searches.
- OpenViking Studio shows the committed writes and relevant retrievals.
- A failed or uncertain write is visible in Chatarium and recoverable without
  duplicating external side effects; a server-reported accepted extraction
  task is not mistaken for a completed memory.
- Wrong-conversation and wrong-project scopes are rejected in tests.
- Restart/close/context-rollover tests preserve the full journal and pending
  checkpoint state. The shorter model input still cites its source history.
- Offline or disabled backend: Chatarium remains usable, with honest status.
- No manual "save memory" command is needed for routine operation.

## Implementation sequence

1. Probe the installed server, pin the safe operation schemas and auth
   contract, and test the local MCP adapter without touching the service's
   configuration.
2. Land the host-owned durable checkpoint outbox and lifecycle triggers with
   synthetic crash/stop/restart tests.
3. Add selective retrieval with journal provenance and hard context budget.
4. Connect background workers only after independent scope and authority
   tests; verify an authenticated end-to-end session and inspect Studio.

The synthetic host lifecycle regression exercises durable enqueue,
summary-file persistence, at-most-once create, delayed read-back, confirmation,
authorized cross-conversation retrieval, model-context admission, restart and
revocation using a fake OpenViking backend. The real HTTP/SSE broker has a
separate async-readback regression. These tests require no user credentials
or network mutation.

This document records the host implementation and its remaining
validation work. Automated tests do not substitute for a verified
authenticated end-to-end save/retrieval against the user's own OpenViking
service.


## Project-scoped memory permission foundation (development branch)

A source conversation's existing project-folder binding is **not** permission
to publish that conversation's memories to other conversations. Project-scoped
recall has a separate, explicit, **directed** authorization model: a user-reviewed
source-to-destination grant is bound to the project's verified canonical local
directory. Permission in one direction never implies permission in reverse.

The backend stores these metadata-only grants in
`openviking-project-memory-grants.json` under the Chatarium data
directory. The schema has strict identity, duplicate, size and count validation,
with an atomic replacement save and a specific revocation operation.
Both the grant file and checkpoint outbox use create-new, private temporary
files and sync their parent directory after replacement on Unix, so a
successful durable state transition does not rely on rename alone.
A missing grant file means no shared memory access. A malformed file rejects the read
instead of allowing broader access. When the directory bindings no longer
match the stored grant, that grant is ineligible.

The scoped selection preserves the newest **confirmed** checkpoint from
each of at most three authorized sources. If there are fewer than three
such checkpoints, a bounded scan of up to 64 additional confirmed entries
from those same sources may fill the remaining slots with relevant older
checkpoints. It never scans OpenViking's global namespace. Read-back is against
each exact URI, and the entire response must match the locally persisted
checkpoint with its owner, turn and journal-sequence header, within the
12 KiB shared retrieval budget. Foreign, unconfirmed, uncertain, mismatched,
oversized and unbound entries are excluded or rejected.

**Native integration implemented on `main`:** under
**Manage selected chat → OpenViking project memory (opt-in)**, the operator can
explicitly allow or revoke read access from a same-project source conversation.
The application persists the directed grant, prefetches permitted confirmed
checkpoints on a background worker, invalidates stale caches after permission
or checkpoint changes, and rechecks grants at exact Send/inspector composition.
A fresh conversation still has no sharing enabled by default.

Live validation against the user's running OpenViking instance and complete
cross-chat permission QA remain pending. Development is committed directly
to `main` without a pull request or branch merge.


## Opt-in live-service diagnostic harness

The local QA agent can run the production fixed-loopback MCP transport
metadata-only probe with:

    node tools/local-http-mcp/openviking-live-probe.mjs --read-only

This establishes an MCP session and lists advertised tools through the pinned
127.0.0.1:1933/mcp route. It does not read or write personal memory. The
CHATARIUM_OPENVIKING_API_KEY environment variable is supported for servers
requiring authentication. Never commit or print the key.

A separate explicitly selected one-shot write test uses:

    node tools/local-http-mcp/openviking-live-probe.mjs --create-synthetic-checkpoint

A separate **explicit** compatibility test of exclusive-create behavior uses:

    node tools/local-http-mcp/openviking-live-probe.mjs --verify-exclusive-create

This test performs two writes **only on a new, synthetic QA URI** with
intentionally different bodies. A compatible server rejects the second write;
a replace-compatible implementation fails the diagnostic. The synthetic file
persists. The user must not confuse this opt-in test with a production
checkpoint or invoke it on a real memory URI.

The read-only probe now checks the server's advertised MCP input schemas
for bounded `read(uris, limit)` and `write(uri, content, mode, wait)` before
allowing any synthetic mutation. It rejects missing schemas and modes that
do not advertise `create`. Passing schema discovery is **not** evidence
that `create` is exclusive. An ordinary successful synthetic create/read-back
also explicitly reports `exclusiveCreateVerified=false`. Only the opt-in
collision test reports `exclusiveCreateVerified=true`; a generic HTTP 409
or unrelated transport/validation failure cannot establish this contract.

This first confirms the tools/list contract, then creates ONE new, uniquely
named checkpoint within a dedicated synthetic QA conversation namespace.
It reads back the exact URI and verifies byte-for-byte content. This synthetic
file is PERSISTENT; no delete, overwrite or retry of uncertain writes occurs.
The probe never alters a real chat, journal or existing checkpoint. GitHub CI
must never run write mode against the user's host. No automatic live writes.

The synthetic probe uses the same five-read verification limit and never
reissues a create request after a write was attempted. Transient read errors
or not-yet-visible content may settle; persistent mismatches fail closed.

The probe tests the installed transport, not the complete automatic
summarization, outbox, grants and Send-context pipeline. That broader live
integration remains unverified, and should be tested by the local QA agent,
not by demanding that the user manually operate a test harness.


## Landed: authorized inactive-worker recall at coordination wake

Inactive coordination workers now follow the same background OpenViking
prefetch path as the foreground chat, but their **own local conversation ID**
is the retrieval owner and destination. Before spending a durable, at-most-once
coordination wake claim, the host schedules pending owner-scoped reads and
explicitly granted project reads on persistence workers. It waits for those
pending read completions, never runs OpenViking network operations on the egui
thread, and never borrows the currently displayed chat's cache or the
controller's permissions.

At durable wake dispatch, Chatarium snapshots the worker's own confirmed
checkpoint evidence and currently authorized directed project evidence,
ranks that verified material against the exact claimed directive, and includes
it as **user-level historical evidence** in the same bounded inference
context as the non-authored activation. Original provenance, owner, turn,
sequence and URI are preserved. Final grant checks are repeated immediately
before context assembly. Revocation, project changes or permission-epoch
invalidation prevent old shared memory from reappearing. A worker does not
gain access merely because it shares a project directory or belongs to the
same coordination group as a controller.

Offline/failed reads use normal bounded retry backoff; they do not claim that
the missing memory was loaded or indefinitely prevent a worker from
responding. The existing coordination tool allowlist remains unchanged:
**this adds scoped historical context, not autonomous memory write permission
or an unrestricted memory search tool**.

Automated tests cover in-flight versus offline retry states, exclusion of
foreign/controller-only history, bounded model context admission, and
revocation after retrieval. Validation against the user's installed OpenViking
server remains outstanding.

## Background project-read epoch and revocation safety

The project-memory retrieval cache uses an explicit generation token on every
background command and completion. A saved grant/revocation, saved project-folder
change or newly verified source checkpoint increments the generation and removes
cached results. Already-running workers cannot publish their old results after
an invalidation, even if the same source is granted again before they return.
Each completion must match the outstanding destination and generation, and
current directed permissions are rechecked when the result is admitted.
Stale failures do not poison retry backoff. The in-flight claim remains until
the old worker finishes, preventing redundant concurrent retrievals; once it
returns, the current generation can retrieve afresh. Authored Send still
rechecks authorization on the final context snapshot. These guarantees do not
cancel an already-dispatched network read, nor imply that a live OpenViking
instance has been tested.
