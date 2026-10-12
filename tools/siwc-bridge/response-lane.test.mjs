import test from "node:test";
import assert from "node:assert/strict";
import { ResponsesLane } from "./response-lane.mjs";

test("ordinary response refuses concurrent response without losing first owner", () => {
  const lane = new ResponsesLane();
  const releaseFirst = lane.acquire("turn-A", "stream");
  assert.throws(() => lane.acquire("turn-B", "stream"), {code: "response_busy", retryable: true});
  assert.throws(() => lane.acquire("turn-A", "stream"), {code: "response_busy"});
  releaseFirst();
  releaseFirst();
  lane.acquire("turn-B", "stream")();
});

test("response patches cannot overlap an active probe or vice versa", () => {
  const lane = new ResponsesLane();
  const releaseProbe = lane.acquire("probe-A", "probe");
  assert.throws(() => lane.acquire("turn-A", "stream"), {code: "probe_busy"});
  assert.throws(() => lane.acquire("probe-B", "probe"), {code: "probe_busy"});
  releaseProbe();
  const releaseStream = lane.acquire("turn-A", "stream");
  assert.throws(() => lane.acquire("probe-B", "probe"), {code: "probe_busy"});
  releaseStream();
  lane.acquire("probe-B", "probe")();
});

test("request IDs are required for safe cancellation and response attribution", () => {
  const lane = new ResponsesLane();
  for (const value of [undefined, null, "", "   ", "x".repeat(257), 17]) {
    assert.throws(() => lane.acquire(value, "stream"), {
      code: "invalid_response_request_id", retryable: false,
    });
  }
  lane.acquire("turn-C", "stream")();
  assert.throws(() => lane.acquire("turn-D", "unknown"), TypeError);
});

test("failed and cancelled operations release the lane for future turns", async () => {
  const lane = new ResponsesLane();
  async function simulateFailure() {
    const release = lane.acquire("turn-fails", "stream");
    try {
      assert.throws(() => lane.acquire("another-turn", "stream"), {code: "response_busy"});
      throw new Error("synthetic transport disconnection");
    } finally {
      release();
    }
  }
  await assert.rejects(simulateFailure(), /synthetic transport disconnection/);
  lane.acquire("turn-recovery", "stream")();
});
