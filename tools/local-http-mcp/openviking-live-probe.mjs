#!/usr/bin/env node
// Live-service verification through Chatarium's fixed-loopback MCP broker.
// Default behavior is metadata-only. Explicit create mode leaves a synthetic file.
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { LocalMcpBroker } from "./client.mjs";

async function verifyReadback({ broker, owner, uri, expected, pause }) {
  // A write may be accepted before its contents are readable. Reads are
  // idempotent; never use a second write to settle an uncertain first write.
  const maxReadAttempts = 5;
  for (let attempt = 0; attempt < maxReadAttempts; attempt++) {
    let readback;
    try {
      readback = await broker.request({
        conversation_id: owner, provider: "openviking", method: "call",
        name: "read", arguments: { uris: uri, limit: 100 },
      });
    } catch {
      // A temporary read error must not reveal remote content or trigger writes.
    }
    if (readback?.isError !== true
        && readback?.content?.length === 1
        && readback.content[0]?.type === "text"
        && readback.content[0]?.text === expected) {
      return;
    }
    if (attempt + 1 < maxReadAttempts) await pause(250 * 2 ** attempt);
  }
  throw new Error("Synthetic create attempted once; exact readback unconfirmed; NEVER auto-retry");
}

function explicitAlreadyExists(detail) {
  // "409" alone also occurs in unrelated transport/status and validation
  // errors. Demand an actual AlreadyExists business code/message instead.
  return /\bALREADY_EXISTS\b|\bAlreadyExists\b|\bfile already exists\b/i.test(String(detail));
}

function explicitCollision(result) {
  // Never infer collision safety from a successful, ambiguous or generic
  // isError reply. The original bytes must also be reverified after rejection.
  return result?.isError === true && explicitAlreadyExists(
    JSON.stringify(result?.structuredContent?.error ?? result?.content ?? []));
}

function verifyToolSchema(tool, fields) {
  const schema = tool?.inputSchema;
  if (schema?.type !== "object" || !schema.properties
      || !fields.every(key => Object.hasOwn(schema.properties, key))) {
    throw new Error("OpenViking MCP tools/list lacks expected argument schemas");
  }
  return schema;
}

function validateAdvertisedContract(tools) {
  const read = tools.find(tool => tool?.name === "read");
  const write = tools.find(tool => tool?.name === "write");
  if (!read || !write) {
    throw new Error("OpenViking does not advertise the expected read/write tools");
  }
  verifyToolSchema(read, ["uris", "limit"]);
  const writeSchema = verifyToolSchema(write, ["uri", "content", "mode", "wait"]);
  const mode = writeSchema.properties.mode;
  if (mode?.type !== "string"
      || (Array.isArray(mode.enum) && !mode.enum.includes("create"))) {
    throw new Error("OpenViking write tool does not advertise mode=create");
  }
}

export async function probeOpenViking({
  broker = new LocalMcpBroker(),
  createSyntheticCheckpoint = false,
  verifyExclusiveCreate = false,
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
  validateAdvertisedContract(Array.isArray(listed?.tools) ? listed.tools : []);
  if (!createSyntheticCheckpoint && !verifyExclusiveCreate) {
    return { ok: true, mode: "discovery-only", advertisedReadAndWrite: true,
      schemaCompatible: true, exclusiveCreateVerified: false, remoteMutation: false };
  }
  const turn = "probe_" + nonce;
  const uri = "viking://~/chatarium/conversations/" + owner + "/checkpoints/" + turn + ".md";
  const body = "# Chatarium memory checkpoint\n\nConversation: " + owner
    + "\nTurn: " + turn + "\nSource journal sequence: 1\n\n"
    + "Synthetic Chatarium live transport verification only. No user content.\n";
  // Create request is sent once. This standard roundtrip does NOT prove that
  // "create" is exclusive on the installed OpenViking version.
  const writeResult = await broker.request({
    conversation_id: owner, provider: "openviking", method: "call",
    name: "write", arguments: { uri, content: body, mode: "create", wait: false },
  });
  if (writeResult?.isError === true) {
    throw new Error("Synthetic create returned an MCP tool error; outcome unknown; NEVER auto-retry");
  }
  await verifyReadback({ broker, owner, uri, expected: body, pause });
  if (!verifyExclusiveCreate) {
    return { ok: true, mode: "synthetic-create-once", verified: true,
      exclusiveCreateVerified: false, remoteMutation: true, uri };
  }

  // Explicit diagnostic ONLY. Attempt a second create on this disposable
  // synthetic QA file with a different body, never on personal content. A
  // compatible exclusive-create server rejects it; permissive newer servers
  // may overwrite the QA object. Never repeat this in production dispatch.
  const collisionBody = body.replace(
    "Synthetic Chatarium live transport verification only.",
    "Synthetic EXCLUSIVE CREATE collision test only.");
  let collisionRejected = false;
  try {
    const second = await broker.request({
      conversation_id: owner, provider: "openviking", method: "call",
      name: "write", arguments: { uri, content: collisionBody, mode: "create", wait: false },
    });
    collisionRejected = explicitCollision(second);
  } catch (error) {
    // A protocol-level 409/AlreadyExists may be surfaced as an exception.
    // Network failures or unknown errors cannot establish the contract.
    collisionRejected = explicitAlreadyExists(error);
  }
  if (!collisionRejected) {
    throw new Error("Installed OpenViking did not prove exclusive-create semantics; unsafe for checkpoint writes");
  }
  await verifyReadback({ broker, owner, uri, expected: body, pause });
  return { ok: true, mode: "synthetic-exclusive-create",
    verified: true, exclusiveCreate: true, exclusiveCreateVerified: true,
    remoteMutation: true, uri };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const modes = ["--read-only", "--create-synthetic-checkpoint", "--verify-exclusive-create"];
  if (!(args.length === 0 || (args.length === 1 && modes.includes(args[0])))) {
    console.error("Usage: node tools/local-http-mcp/openviking-live-probe.mjs [--read-only | --create-synthetic-checkpoint | --verify-exclusive-create]");
    process.exitCode = 2;
  } else {
    const createSyntheticCheckpoint = args[0] === "--create-synthetic-checkpoint";
    const verifyExclusiveCreate = args[0] === "--verify-exclusive-create";
    try {
      // Each RPC has its own upper bound. Rust also applies a 90-second
      // process deadline to automatically triggered production preflights.
      console.log(JSON.stringify(await probeOpenViking({
        broker: new LocalMcpBroker({ timeoutMs: 10000 }),
        createSyntheticCheckpoint, verifyExclusiveCreate,
      })));
    } catch {
      // Never log credentials, user memory, or raw remote responses.
      console.error(JSON.stringify({ ok: false,
        mode: verifyExclusiveCreate ? "synthetic-exclusive-create"
          : createSyntheticCheckpoint ? "synthetic-create-once" : "discovery-only",
        detail: "MCP discovery, exact readback, or exclusive-create verification failed; no automatic retry" }));
      process.exitCode = 1;
    }
  }
}
