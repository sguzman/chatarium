#!/usr/bin/env python3
"""Exercise the compiled Chatarium MCP demo over real one-shot stdio pipes.

No filesystem, ChatGPT account, provider registration, or durable journal is used.
The production sandbox and approval path require their separate Linux tests.
"""
import json
import subprocess
import sys

BIN = sys.argv[1] if len(sys.argv) == 2 else "target/debug/chatarium-mcp-demo"


def exchange(request):
    payload = (json.dumps(request, separators=(",", ":")) + "\n").encode()
    process = subprocess.run(
        [BIN], input=payload, capture_output=True, timeout=5, check=True
    )
    assert process.stderr == b"", process.stderr.decode(errors="replace")
    lines = process.stdout.splitlines()
    assert len(lines) == 1, f"expected exactly one response: {lines!r}"
    response = json.loads(lines[0])
    assert response["jsonrpc"] == "2.0"
    assert response["id"] == request["id"]
    return response


def request(number, method, params):
    return {
        "jsonrpc": "2.0",
        "id": number,
        "method": method,
        "params": {
            **params,
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    }


catalog = exchange(request(5, "tools/list", {}))
assert catalog["result"]["resultType"] == "complete"
names = [tool["name"] for tool in catalog["result"]["tools"]]
assert names == ["hello", "time.now", "shell.command"], names
for tool in catalog["result"]["tools"]:
    assert isinstance(tool["inputSchema"], dict)

hello = exchange(request(6, "tools/call", {"name": "hello", "arguments": {}}))
assert hello["result"]["structuredContent"] == {"message": "hello"}
assert hello["result"]["isError"] is False
clock = exchange(request(7, "tools/call", {"name": "time.now", "arguments": {}}))
assert clock["result"]["structuredContent"]["utc"].endswith("Z")
assert clock["result"]["structuredContent"]["unix_epoch_ms"] > 0
echo = exchange(request(8, "tools/call", {
    "name": "shell.command", "arguments": {"command": "echo", "args": ["mcp", "works"]}
}))
assert echo["result"]["isError"] is False
assert echo["result"]["structuredContent"]["stdout"] == "mcp works\n"
blocked = exchange(request(9, "tools/call", {
    "name": "shell.command", "arguments": {"command": "sh", "args": ["-c", "cat /etc/shadow"]}
}))
assert blocked["result"]["isError"] is True
print("MCP demo stdio: catalog, hello, time, safe shell, and denied shell passed")
