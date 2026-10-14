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

## Scope limit

This initial workspace does NOT autonomously wake an idle receiving chat or spawn its next model turn when a message arrives. Messages are available in the recipient's next normal inference turn. Do not call this a complete autonomous multi-agent execution engine yet.

Full cross-file transactional journal delivery and idle-worker auto-dispatch remain follow-on work.