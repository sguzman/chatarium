// Fixed-loopback MCP Streamable HTTP client for Chatarium, including scoped OpenViking.
// No credentials, redirects, remote endpoints, dynamic hosts, or implicit tool retries.
// Sessions are isolated by local conversation and provider.
export const LOCAL_SERVERS = Object.freeze({
  mathematica: Object.freeze({ url: "http://127.0.0.1:8765/mcp", health: "http://127.0.0.1:8765/healthz" }),
  manifoldr: Object.freeze({ url: "http://127.0.0.1:8877/mcp", health: "http://127.0.0.1:8877/healthz" }),
  openviking: Object.freeze({ url: "http://127.0.0.1:1933/mcp", health: "http://127.0.0.1:1933/health" }),
});
const MAX_REPLY = 4 * 1024 * 1024;
const MATHEMATICA_PROTOCOL = "2025-03-26";
const MANIFOLDR_PROTOCOL = "2025-11-25";
const OPENVIKING_PROTOCOL = "2025-03-26";

// Scope is enforced in the transport, not inferred from model instructions.
// Do not expose other agents' memory, skills, resources or private user data.
const MEMORY_ROOT = "viking://~/chatarium/conversations/";
const MEMORY_TOOLS = new Set(["find", "search", "read", "write"]);
function scopedUri(uri, conversationId) {
  const prefix = MEMORY_ROOT + conversationId;
  if (typeof uri !== "string" || !uri.startsWith(prefix)) return false;
  const remainder = uri.slice(prefix.length);
  if (remainder !== "" && !remainder.startsWith("/")) return false;
  if (remainder.length > 512 || /[?#%\\]/.test(remainder)) return false;
  return remainder === "" || remainder.split("/").slice(1).every(part =>
    part.length > 0 && part !== "." && part !== ".." && /^[a-zA-Z0-9._-]+$/.test(part));
}
function exactKeys(args, required, optional = []) {
  return required.every(k => Object.hasOwn(args, k))
      && Object.keys(args).every(k => required.includes(k) || optional.includes(k));
}
function queryValid(q) { return typeof q === "string" && q.trim().length > 0 && q.length <= 512; }
function limitValid(n, max) { return Number.isSafeInteger(n) && n >= 1 && n <= max; }
export function validateOpenVikingArguments(name, args, conversationId) {
  if (typeof conversationId !== "string"
      || !/^[0-9A-Za-z_-]{1,128}$/.test(conversationId))
    throw new Error("Invalid Chatarium conversation identity for OpenViking");
  if (!MEMORY_TOOLS.has(name) || !args || typeof args !== "object" || Array.isArray(args))
    throw new Error("Unsupported or malformed OpenViking operation");
  if (name === "find" || name === "search") {
    const optional = name === "search" ? ["limit", "min_score", "mode"] : ["limit", "min_score"];
    if (!exactKeys(args, ["query", "target_uri", "limit"], optional) || !queryValid(args.query)
        || !scopedUri(args.target_uri, conversationId) || !limitValid(args.limit, 5)
        || (args.min_score !== undefined && (typeof args.min_score !== "number"
           || args.min_score < 0 || args.min_score > 1))
        || (name === "search" && args.mode !== undefined && args.mode !== "list"))
      throw new Error("OpenViking search must be scoped and bounded to Chatarium");
    return;
  }
  if (name === "read") {
    const uris = typeof args.uris === "string" ? [args.uris] : args.uris;
    if (!exactKeys(args, ["uris", "limit"], ["offset"]) || !Array.isArray(uris)
        || !limitValid(uris.length, 3) || !uris.every(uri => scopedUri(uri, conversationId))
        || (args.offset !== undefined && (!Number.isSafeInteger(args.offset)
          || args.offset < 0 || args.offset > 100000))
        || !limitValid(args.limit, 100))
      throw new Error("OpenViking read must be scoped to three bounded Chatarium paths");
    return;
  }
  // The installed v0.5.0 server's mode=create fails if the file exists.
  // The model cannot overwrite existing memories or set ACLs.
  if (!exactKeys(args, ["uri", "content", "mode"], ["wait"])
      || !scopedUri(args.uri, conversationId)
      || !/^viking:\/\/~\/chatarium\/conversations\/[a-zA-Z0-9_-]+\/checkpoints\/[a-zA-Z0-9._-]+\.md$/.test(args.uri)
      || args.mode !== "create" || typeof args.content !== "string"
      || args.content.trim().length === 0 || Buffer.byteLength(args.content) > 16384
      || (args.wait !== undefined && typeof args.wait !== "boolean"))
    throw new Error("OpenViking write only permits create-only scoped checkpoint files");
}

function validateRoute(url) {
  const parsed = new URL(url);
  if (parsed.protocol !== "http:" || parsed.hostname !== "127.0.0.1"
      || parsed.username || parsed.password || parsed.search || parsed.hash
      || parsed.pathname !== "/mcp") {
    throw new Error("Local MCP endpoints must be explicit loopback HTTP /mcp routes");
  }
}

async function readSseJsonRpc(response, id) {
  if (!response.body) throw new Error("MCP SSE response has no body");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  let dataLines = [];
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) throw new Error("MCP SSE ended before the matching JSON-RPC reply");
      size += value.byteLength;
      if (size > MAX_REPLY) throw new Error("MCP SSE reply exceeds 4 MiB");
      pending += decoder.decode(value, { stream: true });
      while (true) {
        const newline = pending.indexOf("\n");
        if (newline < 0) break;
        let line = pending.slice(0, newline);
        pending = pending.slice(newline + 1);
        if (line.endsWith("\r")) line = line.slice(0, -1);
        if (line === "") {
          if (!dataLines.length) continue;
          const data = dataLines.join("\n");
          dataLines = [];
          if (!data.trim()) continue;
          const message = JSON.parse(data);
          if (message?.jsonrpc === "2.0" && message.id === id) return message;
          continue;
        }
        if (line.startsWith("data:")) dataLines.push(line.slice(5).replace(/^ /, ""));
      }
    }
  } finally {
    // Stop reading after the matching response even if the SSE connection stays
    // open for notifications/keepalives. Never wait for stream EOF.
    await reader.cancel().catch(() => {});
  }
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

// A provider can return a legitimate response larger than the legacy 64 KiB
// Chatarium agent envelope, including duplicated text and structuredContent.
// Preserve the complete source entries we do include, explicitly disclose every
// omitted entry/field, and never turn an oversized response into a silent success.
export function boundManifoldrResult(operation, result) {
  const LIMIT = 40 * 1024; // pretty-printed bytes, with headroom for the agent envelope
  const size = value => Buffer.byteLength(JSON.stringify({ ok: true, result: value }, null, 2));
  if (size(result) <= LIMIT) return result;
  if (result?.isError || !result?.structuredContent ||
      !Object.hasOwn(result.structuredContent, "data")) {
    throw new Error("Manifoldr result exceeds the agent context limit and cannot be safely projected");
  }
  const original = result.structuredContent.data;
  if (original === null || typeof original !== "object") {
    throw new Error("Oversized Manifoldr result has no projectable structured data");
  }
  const array = Array.isArray(original);
  const preview = array ? [] : {};
  const originalKeys = array ? original.length : Object.keys(original).length;
  const meta = {
    kind: "bounded-preview",
    operation,
    original_entries: originalKeys,
    included_entries: 0,
    omitted_entries: originalKeys,
    complete: false,
    warning: "Chatarium projected an oversized Manifoldr result; missing entries are NOT negative results.",
  };
  const projected = {
    isError: false,
    content: [{ type: "text", text: "Chatarium bounded Manifoldr preview. Check structuredContent._chatarium_projection for omission counts." }],
    structuredContent: { data: preview, _chatarium_projection: meta },
  };
  const entries = array
    ? original.map((value, index) => [index, value])
    : Object.entries(original).sort(([a], [b]) => {
        // Keep stable market identifiers and values before long descriptive text.
        const priority = ["id", "question", "slug", "url", "probability", "outcomeType", "isResolved"];
        return (priority.indexOf(a) < 0 ? 100 : priority.indexOf(a)) -
          (priority.indexOf(b) < 0 ? 100 : priority.indexOf(b));
      });
  for (const [key, value] of entries) {
    if (array) preview.push(value);
    else preview[key] = value;
    meta.included_entries++;
    meta.omitted_entries--;
    meta.complete = meta.omitted_entries === 0;
    if (size(projected) > LIMIT) {
      if (array) preview.pop();
      else delete preview[key];
      meta.included_entries--;
      meta.omitted_entries++;
      meta.complete = false;
      // Arrays retain a true prefix. Objects can omit a huge field and still
      // keep subsequent small fields, always with an explicit incompleteness marker.
      if (array) break;
    }
  }
  if (size(projected) > LIMIT) {
    throw new Error("Manifoldr bounded preview still exceeds the agent context limit");
  }
  return projected;
}

export class LocalMcpBroker {
  constructor({ routes = LOCAL_SERVERS, fetchImpl = globalThis.fetch, timeoutMs = 120000,
                openVikingApiKey = process.env.CHATARIUM_OPENVIKING_API_KEY || null } = {}) {
    for (const { url } of Object.values(routes)) validateRoute(url);
    if (openVikingApiKey !== null && (typeof openVikingApiKey !== "string"
        || !openVikingApiKey.trim() || /[\r\n]/.test(openVikingApiKey)))
      throw new Error("Invalid OpenViking API key");
    this.openVikingApiKey = openVikingApiKey;
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
    if (state.provider === "openviking" && this.openVikingApiKey)
      headers["X-Api-Key"] = this.openVikingApiKey;
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
    if (response.status === 202) {
      await response.body?.cancel().catch(() => {});
      // 202 acknowledges receipt, not completion. In particular, a tools/call
      // may already have executed remotely. Never journal a null result as
      // success and never replay this request to guess the outcome.
      throw new Error("MCP HTTP 202 returned no correlated JSON-RPC result; outcome unknown; not retried");
    }
    const media = (response.headers.get("content-type") || "").toLowerCase();
    let message;
    if (media.includes("text/event-stream")) {
      message = await readSseJsonRpc(response, value.id);
    } else if (media.includes("application/json")) {
      message = JSON.parse(await boundedBody(response));
    } else {
      throw new Error("Unexpected MCP response content type");
    }
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
        protocol: provider === "manifoldr" ? MANIFOLDR_PROTOCOL
          : provider === "openviking" ? OPENVIKING_PROTOCOL : MATHEMATICA_PROTOCOL,
        provider, initialized: false };
      this.sessions.set(key, state);
    }
    try {
      if (!state.initialized) await this._initialize(state, config.url, provider);
      if (method === "list") return await this._request(state, config.url, "tools/list");
      if (method === "call") {
        if (typeof args.name !== "string" || !/^[A-Za-z0-9_.-]{1,128}$/.test(args.name)
            || !args.arguments || typeof args.arguments !== "object" || Array.isArray(args.arguments))
          throw new Error("Invalid MCP tool name or JSON-object arguments");
        if (provider === "openviking") validateOpenVikingArguments(args.name, args.arguments, conversationId);
        // One attempt only. Transport timeout may be an unknown-outcome mutation.
        const result = await this._request(state, config.url, "tools/call", {
          name: args.name, arguments: args.arguments,
        });
        if (!result || typeof result !== "object" || Array.isArray(result)
            || !Array.isArray(result.content)) {
          throw new Error("MCP tools/call returned no valid content array; outcome unknown; not retried");
        }
        if (provider === "openviking"
            && Buffer.byteLength(JSON.stringify(result)) > 32 * 1024)
          throw new Error("OpenViking MCP result exceeds bounded agent context; narrow your query");
        return provider === "manifoldr" ? boundManifoldrResult(args.name, result) : result;
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
      headers: provider === "openviking" && this.openVikingApiKey
        ? { "X-Api-Key": this.openVikingApiKey } : {},
    });
    return { reachable: result.ok, status: result.status };
  }
}
