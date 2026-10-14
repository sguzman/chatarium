# Native host-shell MCP (Fish default)

Chatarium's `chatarium.builtin::shell.command` is a separate host-shell tool,
not the restricted `chatarium.demo::shell.command` stdio example.

## Configuration and access

Open **Context & inference controls → Tools / MCP → Host shell · Fish / Bash / Zsh / Sh**.

- Enabled by default, with **Fish** selected and an empty command blacklist
  (all command names initially permitted).
- Choose Fish, Bash, Zsh, or POSIX sh.
- Enter command names on separate lines to block them; save to
  `host-shell-mcp.json` alongside other local Chatarium settings.
- The policy in **Agent defaults** determines `automatic`, `ask`,
  `manual`, or `disabled` for this exact operation.
- For untouched default policies the shell tool uses `ask` (per-call
  approval). Customized permission files are not overwritten; you may
  explicitly change the exact operation to `automatic` in Agent defaults.

Agent tool request:

```json
{"chatarium_mcp_call":{"provider":"chatarium.builtin","tool":"shell.command","arguments":{"command":"pwd; fish --version","cwd":"/home/user"}}}
```

The `cwd` field is optional and defaults to the user's `$HOME`.

This executes `/usr/bin/fish -c <command>` (or the selected shell) using
the invoking user's HOME, environment, Fish configuration, Fish functions,
and configured PATH. Output is returned as a journaled MCP result with
stdout, stderr, exit code and timeout flag. The shell is bounded by 30 seconds
via `/usr/bin/timeout` and stdout/stderr are capped at 24 KiB each. A new
call has a new immutable route ID. Re-running a crashed or ambiguous call
is never automatic.

## IMPORTANT: authority and trust

**This is unrestricted, unsandboxed host execution as the Linux user running
Chatarium.** Shell commands can read, overwrite and delete files, use the
network, run programs, and affect the entire login session. The blacklist
is a convenience filter and is **not** a security boundary. Shell quoting,
aliases, functions and command substitution can circumvent name-based
blacklists. A prompted or compromised model may execute destructive commands
under an `automatic` policy, including instructions embedded in web search
results or documents.

Use `ask` in Agent defaults if you want approval for every command, or
`disabled` to deny the operation. For stronger isolation, use the existing
restricted external MCP stdio sandbox instead. The host shell never weakens
that separate sandbox's original executable checks.

Chatarium records the proposal, authorization, route dispatch, terminal
result or error, and context-admission decision. The process runs off the
UI/persistence thread; tool results resume the agent only through the audited
admission path.

## Verification

Run `cargo +stable test --locked -p chatarium-desktop host_shell` and
`cargo +stable build --release --locked -p chatarium-desktop --bin chatarium-desktop`.

For manual QA, select Fish, save the settings, and ask the agent to use
`chatarium.builtin::shell.command` with `{"command":"echo $fish_version; pwd"}`.
Inspect the corresponding MCP activity card for the journaled result.
