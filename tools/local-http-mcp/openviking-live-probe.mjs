#!/usr/bin/env node
// Live-service verification through Chatarium's fixed-loopback MCP broker.
// Default behavior is metadata-only. Explicit create mode leaves a synthetic file.
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { LocalMcpBroker } from "./client.mjs";

export async function probeOpenViking({
  broker = new LocalMcpBroker(),
  createSyntheticCheckpoint = false,
  nonce = randomUUID().replaceAll("-", ""),
  pause = ms => new Promise(resolve => setTimeout(resolve, ms)),
} = {}) {
  if (typeof nonce !== "string" || !/^[A-Za-z0-9_-]{8,64}$/.test(nonce)) {
    throw new Error("Invalid synthetic probe identity");
  }
  const owner = "chatarium_openviking_qa_" + nonce;
  const listed = await broker.request({
    conversation_id: owner, provider: "openviking", method: "list",
  });
  const names = (Array.isArray(listed?.tools) ? listed.tools : [])
    .map(item => item?.name)
    .filter(name => typeof name === "string");
  if (!["read", "write"].every(name => names.includes(name))) {
    throw new Error("OpenViking does not advertise the expected read/write tools");
  }
  if (!createSyntheticCheckpoint) {
    return { ok: true, mode: "discovery-only", advertisedReadAndWrite: true, remoteMutation: false };
  }
  const turn = "probe_" + nonce;
  const uri = "viking://~/chatarium/conversations/" + owner + "/checkpoints/" + turn + ".md";
  const body = "# Chatarium memory checkpoint\n\nConversation: " + owner
    + "\nTurn: " + turn + "\nSource journal sequence: 1\n\n"
    + "Synthetic Chatarium live transport verification only. No user content.\n";
  // Create-only, no retries, no delete. This synthetic QA file persists.
  const writeResult = await broker.request({
    conversation_id: owner, provider: "openviking", method: "call",
    name: "write", arguments: { uri, content: body, mode: "create", wait: false },
  });
  // MCP transports can return a successful JSON-RPC envelope containing a
  // tool-level failure. The write may still have reached remote storage.
  // Never mistake that response (or a coincidentally matching old file) for
  // a confirmed create, and never try the same write again automatically.
  if (writeResult?.isError === true) {
    throw new Error("Synthetic create returned an MCP tool error; outcome unknown; NEVER auto-retry");
  }
  // wait:false may return before the created checkpoint is materialized.
  // Repeat only idempotent reads, never the potentially-committed write.
  const maxReadAttempts = 5;
  for (let attempt = 0; attempt < maxReadAttempts; attempt++) {
    let readback;
    try {
      readback = await broker.request({
        conversation_id: owner, provider: "openviking", method: "call",
        name: "read", arguments: { uris: uri, limit: 100 },
      });
    } catch {
      // A temporary read failure is safe to retry; do not log source data.
    }
    if (readback?.isError !== true
        && readback?.content?.length === 1
        && readback.content[0]?.type === "text"
        && readback.content[0]?.text === body) {
      return { ok: true, mode: "synthetic-create-once",
        verified: true, remoteMutation: true, uri };
    }
    if (attempt + 1 < maxReadAttempts) {
      await pause(250 * 2 ** attempt);
    }
  }
  throw new Error("Synthetic create attempted once; exact readback unconfirmed; NEVER auto-retry");

}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (!(args.length === 0 || (args.length === 1
        && ["--read-only", "--create-synthetic-checkpoint"].includes(args[0])))) {
    console.error("Usage: node tools/local-http-mcp/openviking-live-probe.mjs [--read-only | --create-synthetic-checkpoint]");
    process.exitCode = 2;
  } else {
    const createSyntheticCheckpoint = args[0] === "--create-synthetic-checkpoint";
    try {
      console.log(JSON.stringify(await probeOpenViking({ createSyntheticCheckpoint })));
    } catch {
      // Do not log sensitive remote responses, user content or credentials.
      // An ambiguous create is never retried automatically.
      console.error(JSON.stringify({ ok: false, mode: createSyntheticCheckpoint
        ? "synthetic-create-once" : "discovery-only",
        detail: "MCP discovery or exact readback failed; no automatic retry" }));
      process.exitCode = 1;
    }
  }
}
