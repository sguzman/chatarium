import assert from "node:assert/strict";
import test from "node:test";
import { createAgentActivityRecorder } from "./agent-activity.mjs";

function fixture() {
  const events = [];
  let elapsed = 0;
  const activity = createAgentActivityRecorder({
    requestId: "turn-1",
    emit: (event) => events.push(event),
    monotonicNow: () => elapsed,
    utcNow: () => "2026-10-13T18:00:00.000Z",
  });
  return { events, activity, tick: (ms) => { elapsed += ms; } };
}

test("live activity events preserve sequence and per-event source timing", () => {
  const { activity, events, tick } = fixture();
  activity.started();
  tick(2);
  activity.transport("wire_attempted");
  tick(11);
  activity.transport("acknowledged");
  tick(17);
  activity.delta("ab");
  tick(6);
  activity.delta("c");
  tick(20);
  activity.completed();
  assert.deepEqual(events.map((event) => event.stage), [
    "started", "wire_attempted", "acknowledged", "first_text_delta",
    "text_delta", "completed",
  ]);
  assert.deepEqual(events.map((event) => event.sequence), [1, 2, 3, 4, 5, 6]);
  assert.deepEqual(events.map((event) => event.elapsed_ms), [0, 2, 13, 30, 36, 56]);
  assert.equal(events[3].delta_chars, 2);
  assert.equal(events[4].delta_chars, 1);
  assert.equal(events[5].delivery_evidence, "assistant_delta_seen");
  assert.ok(events.every((event) =>
    event.source === "siwc_bridge_observation" &&
    event.request_id === "turn-1" &&
    event.observed_at_utc === "2026-10-13T18:00:00.000Z",
  ));
  assert.ok(!JSON.stringify(events).includes("reasoning_summary"));
  assert.throws(() => activity.delta("late"), /terminal/);
});

test("loss before acknowledgment does not claim a response or allow second completion", () => {
  const { activity, events } = fixture();
  activity.started();
  activity.transport("wire_attempted");
  activity.failed(new Error("contains-private-message-that-must-not-appear"));
  assert.equal(events.at(-1).delivery_evidence, "wire_unconfirmed");
  assert.equal(events.at(-1).cancelled, false);
  assert.doesNotMatch(JSON.stringify(events), /contains-private-message/);
  assert.throws(() => activity.completed(), /terminal/);
});

test("cancelled before wire and failed after partial streaming remain distinct", () => {
  const first = fixture();
  first.activity.started();
  first.activity.failed(Object.assign(new Error("cancelled"), { code: "aborted" }));
  assert.equal(first.events.at(-1).delivery_evidence, "not_dispatched");
  assert.equal(first.events.at(-1).cancelled, true);
  const second = fixture();
  second.activity.started();
  second.activity.transport("wire_attempted");
  second.activity.transport("acknowledged");
  second.activity.delta("partial");
  second.activity.failed(Object.assign(new Error("broken"), { code: "ECONNRESET" }));
  assert.equal(second.events.at(-1).delivery_evidence, "assistant_delta_seen");
  assert.equal(second.events.at(-1).cancelled, false);
});

test("duplicate and invented transport events are refused", () => {
  const { activity } = fixture();
  activity.started();
  assert.throws(() => activity.transport("acknowledged"), /invalid acknowledgment/);
  activity.transport("wire_attempted");
  assert.throws(() => activity.transport("wire_attempted"), /duplicate/);
  assert.throws(() => activity.transport("reasoning_summary"), /invalid transport/);
  activity.transport("acknowledged");
  activity.completed();
  assert.throws(() => activity.failed(new Error("too late")), /terminal/);
  assert.throws(
    () => createAgentActivityRecorder({ requestId: "", emit: () => {} }),
    TypeError,
  );
});

test("observed HTTP 429 is a confirmed rejection, not a lost response", () => {
  const { activity, events } = fixture();
  activity.started();
  activity.transport("wire_attempted");
  activity.rejected(429);
  activity.failed(Object.assign(new Error("rate limited"), { status: 429 }));
  assert.deepEqual(events.map((event) => event.stage), [
    "started", "wire_attempted", "http_rejected", "failed",
  ]);
  assert.equal(events[2].http_status, 429);
  assert.equal(events[3].http_status, 429);
  assert.equal(events[3].delivery_evidence, "http_rejected");
  assert.throws(() => activity.completed(), /terminal/);
  const other = fixture();
  other.activity.started();
  assert.throws(() => other.activity.rejected(429), /invalid HTTP rejection/);
  other.activity.transport("wire_attempted");
  assert.throws(() => other.activity.rejected(200), /invalid HTTP rejection/);
  other.activity.rejected(503);
  assert.throws(() => other.activity.delta("invalid"), /rejected HTTP response/);
});
