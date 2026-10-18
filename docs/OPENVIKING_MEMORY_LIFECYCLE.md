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
`NO_MEMORY` transitions to `skipped`, generating no external write.

The persistence worker then records `ready -> dispatched` **before**
launching a separate thread to call OpenViking `write`, with `mode=create`.
It reads the exact file back through OpenViking MCP and transitions to
`confirmed` only if the text matches exactly. A failed write/read, bridge
crash or interruption becomes `uncertain`; the same external write is
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

**Still missing:** topic-relevant selection, explicitly granted
cross-conversation/project recall, semantic lifecycle compaction and end-to-end
testing against the user's actual running service. The initial reader intentionally
does not search other conversation namespaces even when sharing OpenViking
storage. The composer now bounds outgoing context at 128 KiB UTF-8, keeps the
latest messages and critical control sources, prefers verified checkpoints to
old raw transcript, and marks excluded sources in the exact-request inventory.
This prevents unbounded input growth without deleting any journal events, but
is **not** a model-token guarantee or complete semantic summary of omitted turns.

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

This document records the host implementation and its remaining
validation work. Automated tests do not substitute for a verified
authenticated end-to-end save/retrieval against the user's own OpenViking
service.
