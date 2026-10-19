import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { LocalMcpBroker } from "./client.mjs";
import { probeOpenViking } from "./openviking-live-probe.mjs";

function advertisedTools({ advertised = true, missingSchema = false, modeUnavailable = false } = {}) {
  if (!advertised) return [{ name: "read" }];
  const read = { name: "read", inputSchema: {
    type: "object", properties: { uris: { type: "string" }, limit: { type: "integer" } },
  } };
  const write = { name: "write", inputSchema: {
    type: "object", properties: {
      uri: { type: "string" }, content: { type: "string" },
      mode: { type: "string", enum: modeUnavailable ? ["replace", "append"]
        : ["replace", "append", "create"] },
      wait: { type: "boolean" },
    },
  } };
  if (missingSchema) delete write.inputSchema;
  return [read, write];
}

function syntheticBroker({ advertised = true, corruptRead = false, rejectWrite = false,
  toolWriteError = false, readNotReady = 0, missingSchema = false, modeUnavailable = false } = {}) {
  const calls = [];
  let stored = null;
  let readAttempts = 0;
  return {
    calls,
    async request(request) {
      calls.push(request);
      if (request.method === "list") {
        return { tools: advertisedTools({ advertised, missingSchema, modeUnavailable }) };
      }
      assert.equal(request.provider, "openviking");
      assert.equal(request.method, "call");
      if (request.name === "write") {
        if (rejectWrite) throw new Error("uncertain write");
        assert.equal(request.arguments.mode, "create");
        assert.equal(request.arguments.wait, false);
        assert.equal(request.arguments.uri.includes(request.conversation_id), true);
        assert.equal(stored, null, "a second write must never be attempted");
        stored = { uri: request.arguments.uri, body: request.arguments.content };
        if (toolWriteError) {
          return { isError: true, content: [{ type: "text", text: "create rejected" }] };
        }
        return { content: [{ type: "text", text: "created" }] };
      }
      if (request.name === "read") {
        assert.equal(request.arguments.uris, stored?.uri);
        assert.equal(request.arguments.limit, 100);
        if (++readAttempts <= readNotReady) {
          return { isError: true, content: [{ type: "text", text: "not yet materialized" }] };
        }
        return { content: [{ type: "text", text: corruptRead ? "tampered" : stored.body }] };
      }
      throw new Error("unexpected operation");
    },
  };
}
test("live probe defaults to discovery-only and never sends a write", async () => {
  const broker = syntheticBroker();
  const result = await probeOpenViking({ broker, nonce: "readonly1234" });
  assert.deepEqual(result, { ok: true, mode: "discovery-only",
    advertisedReadAndWrite: true, schemaCompatible: true,
    exclusiveCreateVerified: false, remoteMutation: false });
  assert.deepEqual(broker.calls.map(x => x.method), ["list"]);
});
test("explicit synthetic create reads same owner's exact bytes", async () => {
  const broker = syntheticBroker();
  const result = await probeOpenViking({ broker, nonce: "unique789123", createSyntheticCheckpoint: true });
  assert.equal(result.ok, true);
  assert.equal(result.verified, true);
  assert.equal(result.remoteMutation, true);
  assert.deepEqual(broker.calls.map(x => x.name || x.method), ["list", "write", "read"]);
  assert.equal(broker.calls[1].conversation_id, broker.calls[2].conversation_id);
  assert.match(result.uri, /chatarium_openviking_qa_unique789123\/checkpoints\/probe_unique789123.md/);
  assert.match(broker.calls[1].arguments.content, /No user content/);
});
test("absent tools fail before any mutation", async () => {
  const broker = syntheticBroker({ advertised: false });
  await assert.rejects(probeOpenViking({
    broker, nonce: "nomutation123", createSyntheticCheckpoint: true,
  }), /does not advertise/);
  assert.equal(broker.calls.length, 1);
});
test("uncertain write or corrupted readback is never retried", async () => {
  for (const options of [{ rejectWrite: true }, { corruptRead: true }]) {
    const broker = syntheticBroker(options);
    await assert.rejects(probeOpenViking({
      broker, nonce: "uncertain1234", createSyntheticCheckpoint: true,
      pause: async () => {},
    }));
    assert.deepEqual(broker.calls.map(x => x.name || x.method),
      options.rejectWrite ? ["list", "write"] : ["list", "write", ...Array(5).fill("read")]);
  }
});
test("eventually readable checkpoint is verified with reads only", async () => {
  const broker = syntheticBroker({ readNotReady: 2 });
  const delays = [];
  const result = await probeOpenViking({
    broker, nonce: "eventually1234", createSyntheticCheckpoint: true,
    pause: async ms => delays.push(ms),
  });
  assert.equal(result.verified, true);
  assert.deepEqual(broker.calls.map(x => x.name || x.method),
    ["list", "write", "read", "read", "read"]);
  assert.deepEqual(delays, [250, 500]);
});

test("tool-level write failure must not become a confirmed create", async () => {
  const broker = syntheticBroker({ toolWriteError: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "toolerr1234", createSyntheticCheckpoint: true,
  }), /MCP tool error; outcome unknown/);
  assert.deepEqual(broker.calls.map(x => x.name || x.method), ["list", "write"]);
});

test("discovery and write QA fail before mutation when advertised tool schema is incompatible", async () => {
  for (const options of [{ missingSchema: true }, { modeUnavailable: true }]) {
    const broker = syntheticBroker(options);
    await assert.rejects(probeOpenViking({
      broker, nonce: "bad_contract_1234", verifyExclusiveCreate: true,
    }), /schema|mode=create/);
    assert.deepEqual(broker.calls.map(call => call.method), ["list"]);
  }
});

test("ambiguous 409 or validation error cannot prove an exclusive-create collision", async () => {
  const broker = syntheticBroker();
  const inner = broker.request.bind(broker);
  let writes = 0;
  broker.request = async request => {
    if (request.name === "write" && ++writes === 2) {
      broker.calls.push(request);
      return { isError: true, content: [{ type: "text",
        text: "HTTP 409: request body validation failed" }] };
    }
    return inner(request);
  };
  await assert.rejects(probeOpenViking({
    broker, nonce: "ambiguous4091234", verifyExclusiveCreate: true, pause: async () => {},
  }), /unsafe for checkpoint writes/);
  assert.deepEqual(broker.calls.map(call => call.name ?? call.method),
    ["list", "write", "read", "write"]);
});

test("bad identity fails before contacting transport", async () => {
  const broker = syntheticBroker();
  await assert.rejects(probeOpenViking({
    broker, nonce: "../escape", createSyntheticCheckpoint: true,
  }), /Invalid/);
  assert.equal(broker.calls.length, 0);
});


// Integration tests below use the REAL production LocalMcpBroker's HTTP,
// JSON-RPC and SSE handling, with a disposable loopback service only.
// They never connect to the user's actual 127.0.0.1:1933 OpenViking.
async function withFakeHttpOpenViking(t, behavior = {}) {
  const calls = [];
  const files = new Map();
  let sessionCount = 0;
  let readAttempts = 0;
  const server = http.createServer(async (request, response) => {
    assert.equal(request.url, "/mcp");
    assert.equal(request.method, "POST");
    const pieces = [];
    for await (const chunk of request) pieces.push(chunk);
    const rpc = JSON.parse(Buffer.concat(pieces).toString("utf8"));
    const tool = rpc.params?.name;
    calls.push({
      method: rpc.method, name: tool, args: rpc.params?.arguments,
      session: request.headers["mcp-session-id"] ?? null,
    });
    const reply = result => {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ jsonrpc: "2.0", id: rpc.id, result }));
    };
    if (rpc.method === "initialize") {
      sessionCount++;
      response.setHeader("Mcp-Session-Id", "fake-session-" + sessionCount);
      reply({
        protocolVersion: "2025-03-26",
        serverInfo: { name: "fake-openviking", version: "test-only" },
        capabilities: { tools: {} },
      });
      return;
    }
    assert.match(request.headers["mcp-session-id"] ?? "", /^fake-session-/);
    if (rpc.method === "tools/list") {
      reply({ tools: advertisedTools({
        advertised: !behavior.missingWrite,
        missingSchema: behavior.missingSchema,
        modeUnavailable: behavior.modeUnavailable,
      }) });
      return;
    }
    assert.equal(rpc.method, "tools/call");
    if (tool === "write") {
      const args = rpc.params.arguments;
      assert.equal(args.mode, "create");
      assert.equal(args.wait, false);
      assert.match(args.uri, /^viking:\/\/~\/chatarium\/conversations\/chatarium_openviking_qa_/);
      if (files.has(args.uri)) {
        // A repeated write is permitted ONLY in the explicit synthetic
        // create-exclusivity diagnostic; never as a production retry.
        if (!behavior.exclusiveCreate && !behavior.permissiveCreate && !behavior.collisionFailure) {
          assert.fail("a second create must never be attempted");
        }
        if (behavior.exclusiveCreate) {
          reply({ isError: true, content: [{ type: "text", text: "409 ALREADY_EXISTS" }] });
          return;
        }
        if (behavior.collisionFailure) {
          reply({ isError: true, content: [{ type: "text", text: "500 temporary failure" }] });
          return;
        }
      }
      // The server may commit a write even if it only returns HTTP 202.
      files.set(args.uri, args.content);
      if (behavior.toolWriteError) {
        reply({ isError: true, content: [{ type: "text", text: "create rejected" }] });
        return;
      }
      if (behavior.uncertainWrite) {
        response.writeHead(202);
        response.end();
        return;
      }
      reply({ content: [{ type: "text", text: "created" }] });
      return;
    }
    assert.equal(tool, "read");
    const { uris, limit } = rpc.params.arguments;
    assert.equal(limit, 100);
    assert.ok(files.has(uris), "read must use exactly the newly created URI");
    if (++readAttempts <= (behavior.readNotReady ?? 0)) {
      reply({ isError: true, content: [{ type: "text", text: "not ready" }] });
      return;
    }
    const body = behavior.corruptRead ? "REMOTE CONTENT DRIFT" : files.get(uris);
    // The real transport supports SSE and stops on the correlated RPC
    // result; exercise that path rather than returning only JSON mocks.
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    response.end("data: " + JSON.stringify({
      jsonrpc: "2.0", id: rpc.id,
      result: { content: [{ type: "text", text: body }] },
    }) + "\n\n");
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  t.after(async () => {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  const url = "http://127.0.0.1:" + server.address().port + "/mcp";
  const broker = new LocalMcpBroker({ routes: {
    openviking: { url, health: "http://127.0.0.1:" + server.address().port + "/health" },
  }, timeoutMs: 5000 });
  return { broker, calls, files };
}

test("real MCP HTTP discovery probe remains metadata-only", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t);
  const result = await probeOpenViking({ broker, nonce: "metadata_only_123" });
  assert.equal(result.mode, "discovery-only");
  assert.equal(result.remoteMutation, false);
  assert.deepEqual(calls.map(call => call.method), ["initialize", "tools/list"]);
  assert.equal(files.size, 0);
});

test("real MCP HTTP plus SSE verifies exact synthetic create-once checkpoint", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t);
  const result = await probeOpenViking({
    broker, nonce: "roundtrip_qa_123", createSyntheticCheckpoint: true,
  });
  assert.equal(result.mode, "synthetic-create-once");
  assert.equal(result.verified, true);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", "read"]);
  assert.equal(files.size, 1);
  assert.equal(files.get(result.uri), calls[2].args.content);
  assert.equal(calls[2].session, calls[3].session);
});

test("real HTTP MCP delayed readback confirms one create without retry", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { readNotReady: 2 });
  const result = await probeOpenViking({
    broker, nonce: "delayed_http_123", createSyntheticCheckpoint: true,
    pause: async () => {},
  });
  assert.equal(result.verified, true);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", "read", "read", "read"]);
  assert.equal(files.size, 1);
});

test("explicit synthetic check proves exclusive-create semantics over MCP HTTP", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { exclusiveCreate: true });
  const result = await probeOpenViking({
    broker, nonce: "exclusive_http_123", verifyExclusiveCreate: true,
    pause: async () => {},
  });
  assert.equal(result.verified, true);
  assert.equal(result.exclusiveCreate, true);
  assert.equal(result.mode, "synthetic-exclusive-create");
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", "read", "write", "read"]);
  assert.equal(files.size, 1);
  assert.equal(files.get(result.uri), calls[2].args.content);
  assert.notEqual(calls[2].args.content, calls[4].args.content);
});

test("permissive create alias is rejected as unsafe before enabling checkpoint writes", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { permissiveCreate: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "permissive_http_123", verifyExclusiveCreate: true,
    pause: async () => {},
  }), /unsafe for checkpoint writes/);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", "read", "write"]);
  assert.equal(files.size, 1, "only the disposable QA URI may be modified");
  assert.equal(files.get(calls[2].args.uri), calls[4].args.content);
});

test("ambiguous collision error cannot prove exclusive-create semantics", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { collisionFailure: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "ambiguous_http_123", verifyExclusiveCreate: true,
    pause: async () => {},
  }), /unsafe for checkpoint writes/);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", "read", "write"]);
  assert.equal(files.get(calls[2].args.uri), calls[2].args.content);
});

test("real HTTP MCP rejects missing tools before mutation", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { missingWrite: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "missing_tools_123", createSyntheticCheckpoint: true,
  }), /does not advertise/);
  assert.deepEqual(calls.map(call => call.method), ["initialize", "tools/list"]);
  assert.equal(files.size, 0);
});

test("real MCP HTTP 202 is an uncertain write, never automatically replayed", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { uncertainWrite: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "uncertain_http_123", createSyntheticCheckpoint: true,
  }), /202|outcome unknown/);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write"]);
  assert.equal(files.size, 1, "the server may have persisted an uncertain write");
});

test("real MCP tool error is never mistaken for successful create or retried", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { toolWriteError: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "tool_error_http_123", createSyntheticCheckpoint: true,
  }), /MCP tool error; outcome unknown/);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write"]);
  assert.equal(files.size, 1, "a tool-error reply does not prove no write occurred");
});

test("real MCP SSE remote drift fails closed with no repeated writes", async t => {
  const { broker, calls, files } = await withFakeHttpOpenViking(t, { corruptRead: true });
  await assert.rejects(probeOpenViking({
    broker, nonce: "drift_http_123", createSyntheticCheckpoint: true,
    pause: async () => {},
  }), /exact readback unconfirmed/);
  assert.deepEqual(calls.map(call => call.name ?? call.method),
    ["initialize", "tools/list", "write", ...Array(5).fill("read")]);
  assert.equal(files.size, 1);
});
