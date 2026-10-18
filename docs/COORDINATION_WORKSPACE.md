# Chatarium coordination containers — first QA build

This is an explicit, human-configured local coordination workspace. It does not replace existing durable chat-container/session-lifecycle, controller/worker, or MCP route audit systems.

## Construction

1. Open **Open coordination topology** in the local-chat sidebar.
2. Create a container first. It starts empty.
3. Attach existing local conversations as named workers with roles. A conversation may belong to only one coordination container.
4. Choose Sender and Recipient and save directed authority edges. Directive permission and report/request permission are separate. Sharing a container grants no authority by itself.
5. Set delivery to allow, block, queue, approve, or rate_limit (messages per minute). Queued/approval messages can be released or rejected; release rechecks current authority.
6. Inspect the graph, message history, statuses, and explanations in the topology dashboard.

## Worker operations

- chatarium.builtin::coordination.identity, arguments: empty JSON object. Returns the verified active worker identity, container, outgoing edges, and inbox.
- chatarium.builtin::coordination.send, arguments: JSON object with to (numeric worker ID), kind (directive/report/request/result/ack), and body (text). Sender identity always comes from the active local conversation, never model-supplied text. A side-effecting message is saved before its success result is returned to the model.

These operations also honor the exact MCP execution policy. Generated default settings gain the new grants; existing user-customized policies are not silently elevated.

Delivered worker messages are inserted as labeled coordination context on the recipient's next turn, NOT as forged human-authored messages. Denied, queued, and rejected messages never enter the delivered inbox.

## Persistence

The local workspace file is coordination-workspace.json beside Chatarium's journal. Writes use atomic replacement. The workspace file includes immutable message IDs, origin, destination, message kind, timestamps, current delivery status, and decision notes, including blocked attempts. This is a separate persistence surface from the append-only session and controller lifecycle audit.

Reload rejects duplicate conversation ownership across containers and reuse of any globally allocated container, worker, or message ID. The automatic rate-limit queue preserves send-order FIFO even when capacity opens just before another message is sent or after a restart. Manual approvals and explicit edge blocks are not bypassed.

## QA matrix

- Create an empty container, then attach two local chats.
- Assign director -> worker directive permission and worker -> director report-only permission.
- Verify identity for each chat using the built-in operation.
- Have director send a directive. Switch to worker chat and query its inbox.
- Have worker report back; verify source and target IDs.
- Attempt reverse directive and verify it is blocked and absent from delivered inbox.
- Test queue, approve, block, and rate_limit at 1 message/minute.
- Revoke authority while a message waits; release must reject revoked permission.
- Restart and verify container, identities, graph, queues, message history and inbox persist.


## Opt-in worker wake lease (dispatcher boundary)

The workspace now persists a default-off, operator-issued `WakeLease` for an existing member conversation. The lease allows at most **8** eligible directive claims per grant, permits only **one active claim** at a time, and remembers claimed message IDs (up to 256) across restarts. `claim_next_wake` selects a previously delivered, currently authorized directive in message-ID order; it does **not** invoke inference, approve a route, change transcript authorship, or consume a delivered inbox item. Denied, pending approval, rate-queued, and report messages cannot be claimed.

`validate_wake_claim` rechecks current sender directive authority immediately before any future dispatch. `stop_wake_lease` cancels remaining permission and invalidates an outstanding claim without refunding it or retrying that message. A claimed message does not silently restart after a crash. The `claim_next_wake_durable` handoff now atomically saves a selected claim before returning a `WakeDispatchEnvelope` containing that exact directive and its verified sender/message provenance. The JSON-encoded, user-level context is independent of the ordinary truncated inbox preview and cannot masquerade as an authored human turn. Operator grants/stops and claim completion also have save-first wrappers; failed persistence leaves the caller's in-memory state unchanged. The active-chat auto-wake checkbox now durably grants or stops the 8-claim lease for the explicitly selected worker. The existing local inference lifecycle consumes one saved claim per activation, journals a non-authored start, revalidates current authority, includes the exact directive as **user-level** provenance, journals dispatch before the sidecar send, and closes the claim only after the remote turn's completed/failed/interrupted event is durably appended. Separately opted-in inactive worker conversations can now wake without switching the visible chat. All turns use one serialized remote inference pipeline and are selected by oldest authorized directive ID. The dispatcher uses each worker's own saved model, behavior profile, instructions and developer context, never the visible chat's settings. The global maximum is eight automatic starts per session (across all workers), and the durable per-worker allowance remains independently bounded. All session arms start disabled after restart even when historical lease state persists. A stop-all button can disarm workers without switching chats. Stopping blocks future starts and dispatches not yet sent, not a remote request already on the wire. Inactive workers may only continue exact, policy-admitted built-in coordination.identity/send tool operations; other MCP proposals are denied. Intermediate model completions during a same-turn coordination-tool continuation do not close the claim. Model-provided message bodies cannot grant a lease.

## Scope limit

An explicitly armed inactive local worker can now run a serialized automatic inference turn without navigation, while Chatarium remains open and connected. This is not an always-on daemon, parallel worker executor, or automatic permission escalation; the session starts disarmed after restart. Cross-file transactional journal/coordination updates remain future work.