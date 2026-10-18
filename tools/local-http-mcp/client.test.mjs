import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { LOCAL_SERVERS, LocalMcpBroker, boundManifoldrResult, validateOpenVikingArguments } from "./client.mjs";

let server;
let port;
const seen = [];

function respond(res, body, extra = {}, status = 200) {
  res.writeHead(status, { "content-type": "application/json", ...extra });
  res.end(JSON.stringify(body));
}

before(async () => {
  server = http.createServer(async (req, res) => {
    const pieces = [];
    for await (const chunk of req) pieces.push(chunk);
    const message = pieces.length ? JSON.parse(Buffer.concat(pieces).toString()) : {};
    seen.push({ method: message.method, session: req.headers["mcp-session-id"] || null,
      id: message.id, tool: message.params?.name, args: message.params?.arguments });
    if (req.url === "/healthz") { res.end("ok"); return; }
    if (message.method === "initialize") {
      respond(res, { jsonrpc: "2.0", id: message.id, result: {
        protocolVersion: "2025-03-26",
        serverInfo: { name: "test-mcp", version: "1.0" }, capabilities: { tools: {} },
      } }, { "Mcp-Session-Id": `session-${seen.filter(item => item.method === "initialize").length}` });
      return;
    }
    if (message.method === "notifications/initialized") { res.writeHead(202); res.end(); return; }
    if (message.method === "tools/list") {
      respond(res, { jsonrpc: "2.0", id: message.id, result: { tools: [
        { name: "mathematica_create_session", inputSchema: { type: "object" } },
      ] } });
      return;
    }
    if (message.method === "tools/call") {
      if (message.params.name === "ack_without_result") {
        res.writeHead(202);
        res.end();
        return;
      }
      if (message.params.name === "null_rpc_result") {
        respond(res, { jsonrpc: "2.0", id: message.id, result: null });
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write(`: ready\n\ndata: ${JSON.stringify({ jsonrpc: "2.0", method: "notifications/message", params: {} })}\n\ndata: ${JSON.stringify({
        jsonrpc: "2.0", id: message.id, result: {
          content: [{ type: "text", text: "ok" }],
          structuredContent: { name: message.params.name, session: req.headers["mcp-session-id"] },
        },
      })}\n\n`);
      // The Mathematica server is allowed to keep an SSE stream alive after
      // sending its correlated result. Do not wait for the stream to end.
      if (message.params.name === "stream_stays_open") {
        const timer = setTimeout(() => res.end(), 1800);
        res.on("close", () => clearTimeout(timer));
      } else {
        res.end();
      }
      return;
    }
    respond(res, { error: "unknown" }, {}, 404);
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  port = server.address().port;
});

after(async () => { if (server) await new Promise(resolve => server.close(resolve)); });

function client() {
  const url = `http://127.0.0.1:${port}/mcp`;
  return new LocalMcpBroker({ routes: {
    mathematica: { url, health: `http://127.0.0.1:${port}/healthz` },
    manifoldr: { url, health: `http://127.0.0.1:${port}/healthz` },
    openviking: { url, health: `http://127.0.0.1:${port}/healthz` },
  } });
}

test("fixed production ports and loopback only", () => {
  assert.throws(() => new LocalMcpBroker({ routes: {
    evil: { url: "http://example.org/mcp" },
  } }), /loopback/);
  assert.throws(() => new LocalMcpBroker({ routes: {
    evil: { url: "http://127.0.0.1:8765/admin" },
  } }), /loopback/);
});

test("different conversations have distinct persistent transport sessions", async () => {
  const broker = client();
  const first = await broker.request({ conversation_id: "chat-a", provider: "mathematica",
    method: "call", name: "mathematica_create_session", arguments: {} });
  const followup = await broker.request({ conversation_id: "chat-a", provider: "mathematica",
    method: "call", name: "mathematica_execute_code", arguments: { code: "6*7" } });
  const second = await broker.request({ conversation_id: "chat-b", provider: "mathematica",
    method: "call", name: "mathematica_create_session", arguments: {} });
  assert.equal(first.structuredContent.session, followup.structuredContent.session);
  assert.notEqual(first.structuredContent.session, second.structuredContent.session);
  assert.equal(seen.filter(item => item.method === "initialize").length, 2);
});

test("tools/list and health check are supported without inference", async () => {
  const broker = client();
  const list = await broker.request({ conversation_id: "chat-x", provider: "manifoldr", method: "list" });
  assert.equal(list.tools[0].name, "mathematica_create_session");
  const health = await broker.health("manifoldr");
  assert.equal(health.reachable, true);
});

test("invalid tool arguments never reach the server", async () => {
  const broker = client();
  const before = seen.filter(item => item.method === "tools/call").length;
  await assert.rejects(
    broker.request({ conversation_id: "chat-guard", provider: "manifoldr", method: "call",
      name: "bad/name", arguments: {} }), /Invalid MCP/);
  assert.equal(seen.filter(item => item.method === "tools/call").length, before);
});

test("SSE returns a tool response before the HTTP stream closes", async () => {
  const broker = client();
  const start = Date.now();
  const reply = await broker.request({
    conversation_id: "chat-long-sse",
    provider: "mathematica",
    method: "call",
    name: "stream_stays_open",
    arguments: {},
  });
  assert.equal(reply.structuredContent.name, "stream_stays_open");
  assert.ok(Date.now() - start < 1400, "MCP tool waited for SSE connection EOF");
});

test("HTTP 202 without a correlated result is uncertain, never success or automatic replay", async () => {
  const broker = client();
  const before = seen.length;
  await assert.rejects(
    broker.request({
      conversation_id: "chat-math-202",
      provider: "mathematica",
      method: "call",
      name: "ack_without_result",
      arguments: {},
    }),
    /MCP HTTP 202.*outcome unknown.*not retried/,
  );
  const firstAttempt = seen.slice(before);
  assert.equal(firstAttempt.filter(item => item.method === "tools/call").length, 1);
  assert.equal(firstAttempt.filter(item => item.method === "initialize").length, 1);

  // A separately authorized next call may start a fresh transport session;
  // the ambiguous original call is never retried.
  const recovered = await broker.request({
    conversation_id: "chat-math-202",
    provider: "mathematica",
    method: "call",
    name: "mathematica_time",
    arguments: {},
  });
  const all = seen.slice(before);
  assert.equal(recovered.structuredContent.name, "mathematica_time");
  assert.equal(all.filter(item => item.method === "initialize").length, 2);
  assert.deepEqual(all.filter(item => item.method === "tools/call").map(item => item.tool),
    ["ack_without_result", "mathematica_time"]);
});

test("HTTP 200 with a null tools/call result cannot be recorded as a success", async () => {
  const broker = client();
  const before = seen.length;
  await assert.rejects(
    broker.request({
      conversation_id: "chat-math-null",
      provider: "mathematica",
      method: "call",
      name: "null_rpc_result",
      arguments: {},
    }),
    /MCP tools\/call returned no valid content array.*not retried/,
  );
  assert.deepEqual(
    seen.slice(before).filter(item => item.method === "tools/call").map(item => item.tool),
    ["null_rpc_result"],
  );
});

test("Manifoldr probability request preserves the exact array-valued ids argument", async () => {
  // Regression for the live one-market request that previously failed downstream.
  // This mock verifies that Chatarium never serializes ids into a scalar string.
  // Actual Manifold REST compatibility is tested by the independent service.
  const broker = client();
  const ids = ["market-actual-id"];
  const before = seen.length;
  const result = await broker.request({
    conversation_id: "probabilities-arg-regression",
    provider: "manifoldr",
    method: "call",
    name: "get_probabilities",
    arguments: { ids },
  });
  const calls = seen.slice(before).filter(item => item.method === "tools/call");
  assert.equal(calls.length, 1, "one model proposal must create only one MCP tool call");
  assert.equal(calls[0].tool, "get_probabilities");
  assert.deepEqual(calls[0].args, { ids: ["market-actual-id"] });
  assert.equal(result.structuredContent.name, "get_probabilities");
  assert.deepEqual(ids, ["market-actual-id"]);
});

test("oversized Manifoldr categories are an explicitly partial, bounded preview", () => {
  const categories = Array.from({ length: 300 }, (_, index) => ({
    id: `category-${index}`,
    name: `Category ${index}`,
    description: "topic description ".repeat(25),
  }));
  const original = {
    isError: false,
    content: [{ type: "text", text: JSON.stringify({ data: categories }) }],
    structuredContent: { data: categories },
  };
  const projected = boundManifoldrResult("list_categories", original);
  const meta = projected.structuredContent._chatarium_projection;
  assert.equal(meta.kind, "bounded-preview");
  assert.equal(meta.original_entries, categories.length);
  assert.ok(meta.included_entries > 0 && meta.included_entries < categories.length);
  assert.equal(meta.omitted_entries, categories.length - meta.included_entries);
  assert.equal(meta.complete, false);
  assert.deepEqual(projected.structuredContent.data, categories.slice(0, meta.included_entries));
  assert.ok(Buffer.byteLength(JSON.stringify({ ok: true, result: projected }, null, 2)) <= 40 * 1024);
  assert.equal(original.structuredContent.data.length, 300, "projection must not mutate source");
});

test("small Manifoldr results pass unchanged and oversized unknown results fail explicitly", () => {
  const small = { content: [{ type: "text", text: "ok" }], structuredContent: { data: [1] } };
  assert.equal(boundManifoldrResult("search_markets", small), small);
  const unstructured = { content: [{ type: "text", text: "x".repeat(100_000) }] };
  assert.throws(() => boundManifoldrResult("list_categories", unstructured), /cannot be safely projected/);
});

test("oversized single-market descriptions do not hide real IDs or imply completeness", () => {
  const raw = { isError: false, structuredContent: { data: {
    id: "real-market-id", question: "Will this test pass?", probability: 0.42,
    description: "large description ".repeat(15_000), url: "https://manifold.markets/example",
  } }, content: [{ type: "text", text: "large ".repeat(30_000) }] };
  const projected = boundManifoldrResult("get_market", raw);
  assert.equal(projected.structuredContent.data.id, "real-market-id");
  assert.equal(projected.structuredContent.data.probability, 0.42);
  assert.equal(projected.structuredContent._chatarium_projection.complete, false);
  assert.ok(projected.structuredContent._chatarium_projection.omitted_entries > 0);
  assert.equal(projected.structuredContent.data.description, undefined);
});

test("OpenViking endpoint is pinned to loopback port 1933", () => {
  assert.equal(LOCAL_SERVERS.openviking.url, "http://127.0.0.1:1933/mcp");
  assert.equal(LOCAL_SERVERS.openviking.health, "http://127.0.0.1:1933/health");
});

test("OpenViking scoped retrieval and create-only checkpoint reach the MCP server", async () => {
  const broker = client(), before = seen.length;
  const scope = "viking://~/chatarium/conversations/chat-a";
  const a = await broker.request({ conversation_id: "chat-a", provider: "openviking",
    method: "call", name: "find", arguments: { query: "milestone", target_uri: scope, limit: 3 } });
  assert.equal(a.structuredContent.name, "find");
  const b = await broker.request({ conversation_id: "chat-a", provider: "openviking",
    method: "call", name: "write", arguments: { uri: scope + "/checkpoints/turn-4.md",
      content: "Milestone done; source commit abc.", mode: "create" } });
  assert.equal(b.structuredContent.name, "write");
  const calls = seen.slice(before).filter(x => x.method === "tools/call");
  assert.deepEqual(calls.map(x => x.tool), ["find", "write"]);
  assert.equal(calls[1].args.mode, "create");
});

test("OpenViking invalid scopes, context-mode searches, and overwrites fail before tools/call", async () => {
  const failures = [
    ["find", { query: "anything" }],
    ["find", { query: "anything", target_uri: "viking://agent/skills" }],
    ["find", { query: "anything", target_uri: "viking://~/chatarium/conversations/chat-other" }],
    ["find", { query: "anything", target_uri: "viking://~/chatarium/conversations/chat-guard" }],
    ["search", { query: "anything", target_uri: "viking://~/chatarium", mode: "context" }],
    ["read", { uris: "viking://~/secrets/token" }],
    ["read", { uris: "viking://~/chatarium/conversations/chat-other/private.md" }],
    ["read", { uris: "viking://~/chatarium", limit: -1 }],
    ["read", { uris: "viking://~/chatarium/conversations/chat-guard/test.md" }],
    ["read", { uris: Array(4).fill("viking://~/chatarium") }],
    ["write", { uri: "viking://~/chatarium/conversations/chat-a/checkpoints/1.md",
      content: "overwrite", mode: "replace" }],
    ["write", { uri: "viking://~/chatarium/conversations/../checkpoints/1.md",
      content: "traversal", mode: "create" }],
    ["write", { uri: "viking://~/chatarium/conversations/chat-a/checkpoints/1.md",
      content: "x", mode: "create", acl: { acl_mode: "inherit" } }],
    ["write", { uri: "viking://~/chatarium/conversations/chat-a/checkpoints/1.md",
      content: "x".repeat(20000), mode: "create" }],
    ["forget", { uri: "viking://~/chatarium" }],
  ];
  const broker = client(), before = seen.filter(x => x.method === "tools/call").length;
  for (const [name, args] of failures) {
    assert.throws(() => validateOpenVikingArguments(name, args, "chat-guard"), /OpenViking|scoped/);
    await assert.rejects(broker.request({ conversation_id: "chat-guard",
      provider: "openviking", method: "call", name, arguments: args }), /OpenViking/);
  }
  assert.equal(seen.filter(x => x.method === "tools/call").length, before);
});

test("OpenViking API key is never sent to another provider", async () => {
  const headers = [];
  const url = "http://127.0.0.1:" + port + "/mcp";
  const health = "http://127.0.0.1:" + port + "/healthz";
  const broker = new LocalMcpBroker({ routes: {
    openviking: { url, health }, manifoldr: { url, health },
  }, openVikingApiKey: "example-secret-not-real",
     fetchImpl: async (url, init) => {
       headers.push({ url, key: init.headers?.["X-Api-Key"] ?? null });
       return fetch(url, init);
     } });
  await broker.request({ conversation_id: "c1", provider: "openviking",
    method: "call", name: "read", arguments: { uris: "viking://~/chatarium/conversations/c1/test.md", limit: 10 } });
  await broker.request({ conversation_id: "c1", provider: "manifoldr", method: "list" });
  assert.equal(headers.filter(h => h.key).length, 2);
  assert.ok(headers.filter(h => h.key).every(h => h.key === "example-secret-not-real"));
  assert.equal(headers[headers.length - 1].key, null);
});
