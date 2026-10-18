# OpenViking memory lifecycle for Chatarium

Status: **approved implementation design; integration not yet implemented**.
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

The installation was reported ready externally. Chatarium has **not**
connected to or authenticated against this host in this implementation;
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

This document records a design contract, **not a claim that automatic
OpenViking checkpointing works in today's Chatarium build**.
