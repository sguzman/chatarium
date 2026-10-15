import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { LocalMcpBroker } from "./client.mjs";

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
