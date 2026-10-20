import assert from "node:assert/strict";
import test from "node:test";
import { runOpenVikingQa } from "./run-openviking-qa.mjs";

const discovered = {
  ok: true,
  mode: "discovery-only",
  advertisedReadAndWrite: true,
  schemaCompatible: true,
  remoteMutation: false,
};

test("read-only OpenViking readiness launches the normal desktop", async () => {
  const operations = [];
  const report = [];
  const ready = await runOpenVikingQa({
    probe: async () => { operations.push("probe"); return discovered; },
    launch: async () => { operations.push("launch"); },
    output: { write: message => report.push(message) },
    autocapture: undefined,
  });
  assert.equal(ready, true);
  assert.deepEqual(operations, ["probe", "launch"]);
  assert.match(report.join(""), /discovery: PASS.*read-only/);
  assert.doesNotMatch(report.join(""), /automatic checkpoints are disabled/);
});

test("unavailable OpenViking does not prevent launching local Chatarium", async () => {
  const operations = [];
  const report = [];
  const ready = await runOpenVikingQa({
    probe: async () => { operations.push("probe"); throw new Error("PRIVATE_TRANSPORT_CONTENT"); },
    launch: async () => { operations.push("launch"); },
    output: { write: message => report.push(message) },
    autocapture: "0",
  });
  assert.equal(ready, false);
  assert.deepEqual(operations, ["probe", "launch"]);
  assert.match(report.join(""), /discovery: UNAVAILABLE/);
  assert.match(report.join(""), /automatic checkpoints are disabled/);
  assert.doesNotMatch(report.join(""), /PRIVATE_TRANSPORT_CONTENT/);
});

test("a successful synthetic create cannot be mistaken for read-only readiness", async () => {
  let launched = false;
  const messages = [];
  const ready = await runOpenVikingQa({
    probe: async () => ({
      ...discovered,
      mode: "synthetic-create-once",
      remoteMutation: true,
    }),
    launch: async () => { launched = true; },
    output: { write: message => messages.push(message) },
  });
  assert.equal(ready, false);
  assert.equal(launched, true);
  assert.match(messages.join(""), /discovery: UNAVAILABLE/);
});
