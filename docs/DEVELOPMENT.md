# Development

## Toolchain

Chatarium is Rust-first and Windows-first initially. The repository tracks the stable Rust channel with `rustfmt` and `clippy` components.

Use normal Cargo dependency resolution. Do not add bootstrap scripts that silently download or execute opaque binaries. If extra Windows tooling becomes necessary, document the dependency and prefer a reproducible Scoop installation path.

## Baseline commands

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

## Native conversation integration gate

The desktop binary includes a deterministic integration journey exercised by
CI on Linux and by the Windows workspace test suite. The dedicated command is:

```text
cargo test -p chatarium-desktop --bin chatarium-desktop integration_journey_ -- --nocapture
```

The journey uses the production authored-message commit, local display
projection, Context Composer, route permission gate, checked tool-result
observation, explicit result admission, outgoing dispatch manifest, and
reverse provenance lookup. It writes a temporary, real JSONL journal and
reopens it to verify restart replay. Negative cases cover an unexecuted
denied tool, attempted cross-conversation admission, and revocation after
a Send-click admission snapshot was frozen.

Remote assistant responses in this test are **synthetic typed observations**;
the test does not contact an inference provider, execute an untrusted
process, authenticate a browser, or prove rendering fidelity. Those
boundaries remain covered by separate transport/sandbox CI and targeted
desktop or browser QA. The owner does not need to run this test manually.

## Native conversation search scaling

The sidebar's native-chat search is local-only and scoped to typed authored
user turns and their matching assistant snapshot/completion observations.
Tool output, controller events, provider diagnostics, and imported remote
mirror bodies cannot become native-chat search documents simply because
they share a journal or turn scope.

The search corpus groups journal events by the typed owning conversation in
one chronological projection pass, then applies the existing display-message
deduplication rules. It is cached in egui's transient session memory.
Append-only draft, tool, controller, and other non-transcript journal events
advance the cache cursor **without** rebuilding the native message corpus.
An authored user message or assistant snapshot/completion invalidates it and
rebuilds from the authoritative journal. A replaced/truncated journal prefix
is not eligible for reuse. Conversation titles
remain owned by the local catalog; the index is rebuildable and contains
no separate authoritative database, persisted search query, or background
provider requests. A regression test compares indexed messages with the
existing single-conversation projector, including streamed snapshot
replacement and cross-conversation isolation.
Native search retains Ctrl+Shift+K focus and adds Up/Down selection with
Enter to open the highlighted local conversation. Changing or clearing the
query resets that selection; Escape clears query and focus. Selection is
transient and cannot change journal history or tool permissions.
For message-text matches, the sidebar displays a bounded excerpt around the
matching term, drawn exclusively from the indexed local user/assistant
messages for that conversation. Unicode case-folding can change byte
length; the excerpt uses original-character boundaries so accented and
multilingual searches never slice invalid UTF-8. No raw tool result or
remote-mirror content is searched or surfaced through this control.
Opening a native message hit, by mouse or Enter, passes the exact trimmed
search query to the existing transcript reader and selects the first hit so
the matching bubble scrolls into view. A title-only match never fabricates
a transcript hit. The handoff is local UI state, not an additional
inference-context admission or durable mutation.

## Automated QA ownership

For browser-facing and integration work, Codex owns the complete local QA/debug/Git loop. The principal is not a manual regression runner. Use the Playwright-managed bundled Chromium QA browser with persistent state at `~/.local/share/chatarium-qa-browser/`; do not automate the principal's normal Edge installation. Follow [Codex-owned QA workstation](CODEX_QA_WORKSTATION.md) plus [Human QA protocol](HUMAN_QA.md).

If the automation path itself is missing, build or repair it before requesting another live validation.

## Protocol-facing work

Before changing code because ChatGPT behavior appears to have changed:

1. identify the smallest failing canonical flow;
2. capture the new behavior;
3. sanitize it;
4. create a new protocol snapshot;
5. structurally diff it against the last known-good observation;
6. document what is observed versus inferred;
7. adapt code and add/update fixtures;
8. test degraded/mismatch behavior as well as the happy path.

## Native transport anti-drift gate

The hard product contract is [SELF_SUSTAINING_TRANSPORT.md](SELF_SUSTAINING_TRANSPORT.md).

Before adding browser-control, retry, bridge, cooldown, or remote-health machinery to the ordinary conversation path, answer:

> Does this materially advance native browser-independent ChatGPT reads/writes, or produce protocol evidence required to determine whether those reads/writes are viable?

If not, the work needs explicit architectural justification.

For undocumented first-party transport work, prefer existing HAR/Flight Recorder evidence, local raw mirror specimens, and passive recording of traffic the real client already generated. Active browser automation should answer a specific remaining question rather than manufacture broad duplicate traffic.

The existence or absence of a documented public API is not a substitute for this investigation.

## Undocumented browser integration gate

The 2026-10-03 history-bridge incident established that this project cannot treat partial browser evidence as sufficient merely because implementation is convenient. See [the full postmortem](postmortems/2026-10-03-chatgpt-history-bridge.md).

For any undocumented consumer-web operation, **do not implement beyond scaffolding until a current request-complete capture exists**. Acceptable evidence is a raw HAR, CDP capture, or equivalent first-party trace from the exact target flow. If the evidence does not include a property needed to reproduce the request, that property is unknown.

Before implementation, record parity for:

- HTTP method, host, path, query keys, literal values, duplicates/order where observed;
- request body and content type;
- credentials/origin/referrer behavior where relevant;
- context-bearing headers such as account/workspace/project selection;
- challenge/Sentinel or other dependency-bearing request material;
- request ordering and prerequisite calls;
- expected status/content type/response schema;
- pagination and identity echoes.

Every observed request header relevant to the target flow must be classified as one of:

- semantic context;
- credential/private;
- anti-abuse/challenge context;
- incidental telemetry;
- unknown.

Unknown fields are not silently omitted and later called equivalent.

### Mandatory capture rule

When the product target depends on undocumented first-party behavior and current evidence is incomplete, the engineering response is:

> Current evidence is insufficient; capture is required.

Do **not** tell the operator that HAR/CDP evidence is unnecessary unless the repository already contains equivalent current evidence and the issue names it explicitly.

### Browser prototype promotion rule

Userscripts are prototype/observation tools by default. They may become a runtime dependency only after they pass:

- a documented execution-world model;
- deterministic browser-to-desktop roundtrip;
- browser-version compatibility testing;
- reliable background/worker lifetime;
- first-party request-context parity;
- end-to-end integration diagnostics;
- one focused live validation without speculative manual debugging.

If a browser prototype fails two focused live validations at the same architectural boundary, stop and escalate the architecture instead of requesting another speculative human test.

## UI work

Keep egui rendering cheap. The app should display immutable/cheap snapshots of state and emit commands. Persistence, networking, capture parsing, indexing, and reconciliation run elsewhere.

### Native conversation discovery

The local conversation sidebar has an independent, ephemeral filter for
native Chatarium chats. It matches explicit or derived titles as well as
locally projected user and assistant message text, case-insensitively.
Search results indicate whether the title or transcript matched. Empty
queries preserve the full list, including empty native conversations;
archived visibility stays under its existing explicit toggle.

This is distinct from the imported ChatGPT mirrored archive search.
**Ctrl+Shift+K** focuses the native conversation filter, and **Esc**
clears it and returns keyboard focus. The mirrored archive retains its
separate **Ctrl+K** shortcut. Searching local conversations does not
query a network service, change the active conversation, admit model
context, or read untrusted MCP tool bodies. The matching policy has focused tests in
`apps/desktop/src/native_conversation_search.rs` and the desktop is checked
through the existing GitHub Actions Rust/Linux matrix. The native sidebar
computes its match and Unicode-safe excerpt in one scan of each candidate
conversation, avoiding a second full message pass merely to display previews.
Title hits do not fabricate message previews.

### Keyboard-first native chat navigation

**Ctrl+Shift+N** creates a new isolated native conversation and focuses its
message composer. Any previous native-chat search filter is cleared so the
new chat remains visible. **Ctrl+Tab** switches to the next unarchived
native conversation; **Ctrl+Shift+Tab** switches in the other direction.
The cyclic order is newest-created first, independent of last-opened timestamps
or mutable titles, so repeated shortcut presses walk every chat instead of
oscillating between recent ones. Cycling clears any stale native search
filter after a successful switch so the active row is visible. Archived
conversations are never restored implicitly by cycling. When viewing a remote mirror, the first switch opens
the newest available native chat.

These shortcuts call the same existing persistence-backed create/select
operations as the sidebar. An in-flight local turn still blocks switching
or creation; no draft, model state or tool permissions are moved between
conversations. Cycling decisions are pure/tested in
`apps/desktop/src/conversation_navigation.rs`. Keyboard-focus behavior in
the actual Wayland window remains an interactive QA concern.

### Shared native transcript projection

The local conversation reader and the sidebar now borrow the same
`native_conversation_search::cached_index` projection for visible native
user/assistant messages. Previously the reader separately scanned the entire
journal and cloned all current conversation message text on every egui frame
even when nothing changed. The shared index is rebuilt only when relevant
durable user/assistant observations advance, reusing it across unrelated
draft, MCP tool and lifecycle journal appends.

This is a derived, transient performance optimization, not a new source of
truth. The append-only journal remains authoritative. Regression coverage in
`native_conversation_search.rs` compares cached messages with the original
per-conversation display projection, including assistant snapshots, completion,
and foreign-conversation isolation. Indexing never admits tool output as
chat text.

### Native visible transcript export

The selected **native** conversation can be explicitly copied as readable
Markdown or versioned structured JSON from the local sidebar. JSON preserves
exact message whitespace and immutable event sequence; Markdown blockquotes
message bodies so user-supplied headings cannot impersonate Chatarium's role
headers. The caller passes only its current
`projected_local_display_messages` result. Tool results, memory artifacts,
draft WAL, foreign conversations and imported read-only mirrors are not
automatically included.

The clipboard action is user-triggered, produces no new journal events, and
does not silently create a file. An observed, in-progress assistant stream
may be partial. This is a convenient portable copy, **not** a data backup or
verified remote transcript. Pure serializer and conversation-isolation
tests live in `apps/desktop/src/native_transcript_export.rs`.

### Keyboard focus ownership

**Ctrl+L** focuses the native message composer from elsewhere in the local
workspace without submitting or modifying its draft. It is not claimed in
read-only historical/mirrored views. This is separate from **Ctrl+Shift+N**
which creates a new conversation.

Ctrl+Enter commits a message **only while the message composer has keyboard
focus**. The Send / Commit locally button remains separately accessible.
Other focused inputs (archive search, settings, conversation rename) must
not accidentally send a draft.

Reader-navigation keys (Home, End, PageUp, PageDown, Ctrl+N, Ctrl+P) are
handled by the transcript reader only if no text input currently requests
keyboard input. This ensures multiline editing, search, and settings fields
retain their standard editing/navigation keys. Pure focus-boundary tests
live in `apps/desktop/src/conversation_keybindings.rs`. Native visual
QA of actual focus transitions is distinct from CI keyboard-policy tests.

## Commit shape

When practical, keep evidence + documentation + adaptation atomic. A good protocol maintenance commit can say exactly which observation changed and how the implementation was updated in response.
