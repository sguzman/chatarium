import { test } from "node:test";
import assert from "node:assert/strict";
import { probeOpenViking } from "./openviking-live-probe.mjs";

function syntheticBroker({ advertised = true, corruptRead = false, rejectWrite = false } = {}) {
  const calls = [];
  let stored = null;
  return {
    calls,
    async request(request) {
      calls.push(request);
      if (request.method === "list") {
        return { tools: advertised ? [{ name: "read" }, { name: "write" }] : [{ name: "read" }] };
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
        return { content: [{ type: "text", text: "created" }] };
      }
      if (request.name === "read") {
        assert.equal(request.arguments.uris, stored?.uri);
        assert.equal(request.arguments.limit, 100);
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
    advertisedReadAndWrite: true, remoteMutation: false });
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
    }));
    assert.deepEqual(broker.calls.map(x => x.name || x.method),
      options.rejectWrite ? ["list", "write"] : ["list", "write", "read"]);
  }
});
test("bad identity fails before contacting transport", async () => {
  const broker = syntheticBroker();
  await assert.rejects(probeOpenViking({
    broker, nonce: "../escape", createSyntheticCheckpoint: true,
  }), /Invalid/);
  assert.equal(broker.calls.length, 0);
});
