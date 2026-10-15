// Fixed-loopback MCP Streamable HTTP client for Chatarium.
// No credentials, redirects, remote endpoints, dynamic hosts, or implicit tool retries.
// Sessions are isolated by local conversation and provider.
export const LOCAL_SERVERS = Object.freeze({
  mathematica: Object.freeze({ url: "http://127.0.0.1:8765/mcp", health: "http://127.0.0.1:8765/healthz" }),
  manifoldr: Object.freeze({ url: "http://127.0.0.1:8877/mcp", health: "http://127.0.0.1:8877/healthz" }),
});
const MAX_REPLY = 4 * 1024 * 1024;
const MATHEMATICA_PROTOCOL = "2025-03-26";
const MANIFOLDR_PROTOCOL = "2025-11-25";

function validateRoute(url) {
  const parsed = new URL(url);
  if (parsed.protocol !== "http:" || parsed.hostname !== "127.0.0.1"
      || parsed.username || parsed.password || parsed.search || parsed.hash
      || parsed.pathname !== "/mcp") {
    throw new Error("Local MCP endpoints must be explicit loopback HTTP /mcp routes");
  }
}

function parseSse(body, id) {
  let event = [];
  for (const line of body.split(/\r?\n/)) {
    if (line === "") {
      if (!event.length) continue;
      const data = event.join("\n");
      event = [];
      const message = JSON.parse(data);
      if (message && message.jsonrpc === "2.0" && message.id === id) return message;
      continue;
    }
    if (line.startsWith("data:")) event.push(line.slice(5).replace(/^ /, ""));
  }
  throw new Error("MCP response SSE did not contain matching JSON-RPC request ID");
}

async function boundedBody(response) {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const parts = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_REPLY) throw new Error("MCP reply exceeds 4 MiB");
      parts.push(value);
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  return Buffer.concat(parts.map(part => Buffer.from(part))).toString("utf8");
}

export class LocalMcpBroker {
  constructor({ routes = LOCAL_SERVERS, fetchImpl = globalThis.fetch, timeoutMs = 120000 } = {}) {
    for (const { url } of Object.values(routes)) validateRoute(url);
    this.routes = routes;
    this.fetchImpl = fetchImpl;
    this.timeoutMs = timeoutMs;
    this.sessions = new Map();
    this.queues = new Map();
  }

  async _post(state, url, value) {
    const headers = {
      Accept: "application/json, text/event-stream",
      "Content-Type": "application/json",
      "MCP-Protocol-Version": state.protocol,
    };
    if (state.sessionId) headers["Mcp-Session-Id"] = state.sessionId;
    const response = await this.fetchImpl(url, {
      method: "POST",
      headers,
      body: JSON.stringify(value),
      redirect: "error",
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    const session = response.headers.get("Mcp-Session-Id");
    if (session) {
      if (session.length > 256 || /[\r\n]/.test(session)) throw new Error("Invalid MCP session header");
      state.sessionId = session;
    }
    if (!response.ok && response.status !== 202) {
      const description = (await boundedBody(response)).slice(0, 500);
      throw new Error(`MCP HTTP ${response.status}: ${description}`);
    }
    if (response.status === 202) return null;
    const media = (response.headers.get("content-type") || "").toLowerCase();
    const raw = await boundedBody(response);
    const message = media.includes("text/event-stream")
      ? parseSse(raw, value.id)
      : media.includes("application/json")
        ? JSON.parse(raw)
        : (() => { throw new Error("Unexpected MCP response content type"); })();
    if (!message || message.jsonrpc !== "2.0" || message.id !== value.id)
      throw new Error("MCP response request ID mismatch");
    if (message.error) throw new Error(`MCP JSON-RPC error: ${JSON.stringify(message.error).slice(0, 800)}`);
    if (!("result" in message)) throw new Error("MCP response missing result");
    return message.result;
  }

  async _request(state, url, method, params = {}) {
    const id = ++state.nextId;
    return await this._post(state, url, { jsonrpc: "2.0", id, method, params });
  }

  async _initialize(state, url, provider) {
    const result = await this._request(state, url, "initialize", {
      protocolVersion: state.protocol,
      capabilities: {},
      clientInfo: { name: "chatarium-local-mcp", version: "1.0.0" },
    });
    if (!result || !result.serverInfo) throw new Error("MCP server did not initialize");
    if (typeof result.protocolVersion === "string") state.protocol = result.protocolVersion;
    // Manifoldr v0.4.0 uses stateless JSON responses. Mathematica requires
    // a session-scoped notifications/initialized before kernel operations.
    if (provider !== "mathematica") {
      state.initialized = true;
      return;
    }
    if (!state.sessionId) throw new Error("Mathematica MCP did not establish a transport session");
    const headers = { Accept: "application/json, text/event-stream", "Content-Type": "application/json", "MCP-Protocol-Version": state.protocol };
    if (state.sessionId) headers["Mcp-Session-Id"] = state.sessionId;
    const reply = await this.fetchImpl(url, {
      method: "POST", headers, redirect: "error",
      signal: AbortSignal.timeout(15000),
      body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
    });
    if (!reply.ok && reply.status !== 202)
      throw new Error(`MCP initialized notification rejected: HTTP ${reply.status}`);
    await reply.body?.cancel().catch(() => {});
    state.initialized = true;
  }

  async _perform(conversationId, provider, method, args) {
    const config = this.routes[provider];
    if (!config) throw new Error("Unrecognized local MCP server");
    const key = `${conversationId}: ${provider}`;
    let state = this.sessions.get(key);
    if (!state) {
      state = { sessionId: null, nextId: 0,
        protocol: provider === "manifoldr" ? MANIFOLDR_PROTOCOL : MATHEMATICA_PROTOCOL,
        initialized: false };
      this.sessions.set(key, state);
    }
    try {
      if (!state.initialized) await this._initialize(state, config.url, provider);
      if (method === "list") return await this._request(state, config.url, "tools/list");
      if (method === "call") {
        if (typeof args.name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(args.name)
            || !args.arguments || typeof args.arguments !== "object" || Array.isArray(args.arguments))
          throw new Error("Invalid MCP tool name or JSON-object arguments");
        // One attempt only. Transport timeout may be an unknown-outcome mutation.
        return await this._request(state, config.url, "tools/call", {
          name: args.name, arguments: args.arguments,
        });
      }
      throw new Error("Unsupported MCP operation");
    } catch (error) {
      this.sessions.delete(key);
      throw error;
    }
  }

  async request({ conversation_id, provider, method, name, arguments: toolArgs = {} }) {
    if (typeof conversation_id !== "string" || conversation_id.length < 1 || conversation_id.length > 128
        || !/^[0-9A-Za-z_-]+$/.test(conversation_id)) throw new Error("Invalid local conversation identifier");
    if (!Object.hasOwn(this.routes, provider)) throw new Error("Unsupported local MCP provider");
    if (!["list", "call"].includes(method)) throw new Error("Unsupported local MCP method");
    const key = `${conversation_id}: ${provider}`;
    const before = this.queues.get(key) || Promise.resolve();
    const action = before.catch(() => {}).then(() =>
      this._perform(conversation_id, provider, method, { name, arguments: toolArgs }));
    this.queues.set(key, action);
    try { return await action; }
    finally { if (this.queues.get(key) === action) this.queues.delete(key); }
  }

  async health(provider) {
    if (!Object.hasOwn(this.routes, provider)) throw new Error("Unsupported local MCP provider");
    const result = await this.fetchImpl(this.routes[provider].health, {
      method: "GET", redirect: "error", signal: AbortSignal.timeout(3000),
    });
    return { reachable: result.ok, status: result.status };
  }
}
