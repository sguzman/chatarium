# MCP demo tools: Hello, Time, and Shell

This is an actual one-shot MCP 2026-07-28 **stdio server**. It advertises three tools and responds with correlated, single-frame JSON-RPC. It is deliberately not a hidden model-driven executor and does not bypass Chatarium's durable ToolCall approval gates.

## Tools

| Name | Example JSON arguments | Result |
| --- | --- | --- |
| hello | {} | A deterministic greeting |
| time.now | {} | UTC ISO-8601 timestamp and Unix epoch milliseconds |
| shell.command | {"command":"echo","args":["hello from Chatarium"]} | Bounded stdout, stderr, exit code |

The time tool reports the wall clock in the provider process. It is **not** transport or server acknowledgment telemetry.

The shell tool is a deliberately narrow demonstration, **not arbitrary host shell access**. Only echo, pwd, id, date, and uname are allowed; all except echo accept no arguments, apart from uname -s or -m. It launches a fixed absolute executable with exact argv and no shell string interpolation. Arguments and output are bounded, and extra flags are refused. The desktop's existing one-shot MCP runner additionally isolates the provider from the host home and network. Standalone execution outside Chatarium does **not** imply a sandbox.

## Build and test

From the repository root:

    cargo build -p chatarium-desktop --bin chatarium-mcp-demo
    python3 tools/qa-mcp-demo/smoke.py target/debug/chatarium-mcp-demo

The smoke test starts a new real subprocess for each JSON-RPC request, checking tools/list, hello, clock, allowed echo, and rejected arbitrary shell. Separate Linux runner tests cover bubblewrap/prlimit sandbox readiness and durable one-shot dispatch. CI runs the provider smoke test on Linux.

## Register in the existing Chatarium desktop UI

The current runner **only permits trusted root-owned executables in /usr/bin**; a compiled binary in target/debug is *not* permitted for execution by Chatarium. After building, if you explicitly choose to install this example, use your normal administrator-controlled installation process to install the binary as /usr/bin/chatarium-mcp-demo, owner root, mode 0755. Do not weaken the executable trust checks or symlink a user-writable binary.

In the local conversation, open **Context & inference controls → Tools / MCP**:

1. Use **Check Linux sandbox host** (no tool call). This requires /usr/bin/bwrap, /usr/bin/prlimit, and usable Linux namespaces.
2. Register a new external provider such as chatarium.demo and bind its routing endpoint.
3. Configure executable /usr/bin/chatarium-mcp-demo with **no argv**, and an immutable operation allowlist containing these names, one per line:

       hello
       time.now
       shell.command
       chatarium.internal.tools-list

4. Save the configuration, then separately **Activate**.
5. Use **Tool call audit**: select provider, operation, and JSON arguments. **Record call**, explicitly **Allow** the route, inspect the MCP request, then separately **Run sandboxed MCP tool · one shot**.
6. Inspect the durable outcome. Optionally **Admit** its exact result to make it eligible as local conversation context.

To list the provider's advertised tools, use the reserved local operation chatarium.internal.tools-list with {}. This maps to MCP tools/list, not a provider tool by that name; it also requires separate authorization and a one-shot Run.

**Important:** Tools do not execute merely because the model asks for them. This demo exercises current manual controls and audits, not automatic model-to-tool routing. The shell demo has no access to the host home or network when invoked through the confined runner. General user-owned MCP binaries and broader permissions are future work; this demo intentionally leaves the strict /usr/bin-only policy intact.

If the host check fails on EndeavourOS, see LOCAL_TOOL_INTEGRATION.md. Do not disable OS security policies to make the demo work.
