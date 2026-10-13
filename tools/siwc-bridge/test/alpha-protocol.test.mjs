import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import test from "node:test";

const here = dirname(fileURLToPath(import.meta.url));
const bridge = resolve(here, "../bridge.mjs");
const preload = pathToFileURL(resolve(here, "offline-preload.mjs")).href;

// End-to-end NDJSON contract against the real sidecar implementation, with
// only the upstream DevKit swapped out. Nothing can authenticate or go online.
test("offline alpha: session, models, streamed turn, cancellation and recovery", async (t) => {
  const child = spawn(process.execPath, ["--import", preload, bridge], {
    stdio: ["pipe", "pipe", "pipe"],
    shell: false,
    env: {
      ...process.env,
      OPENAI_API_KEY: "",
      OPENAI_ORG_ID: "",
      OPENAI_PROJECT_ID: "",
      // The fake DevKit returns a synthetic HTTP 429 on the first model read.
      CHATARIUM_QA_MODEL_RETRY_ONCE: "1",
    },
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk.slice(0, 1024); });
  const lineReader = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const seen = [];
  let changed = () => {};
  let exited = false;
  child.on("exit", () => { exited = true; changed(); });
  child.on("error", () => { exited = true; changed(); });

  lineReader.on("line", (line) => {
    // Every returned command frame must be valid JSON and credential-free.
    const value = JSON.parse(line);
    assert.doesNotMatch(
      JSON.stringify(value),
      /access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|cookie/i,
    );
    seen.push(value);
    changed();
  });
  t.after(async () => {
    lineReader.close();
    child.stdin.end();
    if (!exited) child.kill();
  });

  async function until(predicate, after = 0) {
    const deadline = Date.now() + 7000;
    while (Date.now() < deadline) {
      const matched = seen.slice(after).find(predicate);
      if (matched) return matched;
      if (exited) throw new Error("SIWC bridge exited early: " + stderr);
      await new Promise((resolvePromise) => {
        const timer = setTimeout(() => {
          changed = () => {};
          resolvePromise();
        }, 40);
        changed = () => { clearTimeout(timer); resolvePromise(); };
      });
    }
    throw new Error("Timed out waiting for offline SIWC event; observed types: " +
      seen.map((event) => event.type + ":" + (event.request_id ?? "")).join(", "));
  }
  function send(type, request_id, more = {}) {
    child.stdin.write(JSON.stringify({ type, request_id, ...more }) + "\n");
  }
  const ready = await until((event) => event.type === "ready" && event.protocol === 1);
  assert.ok(ready);
  send("session", "offline-session");
  const session = await until(
    (event) => event.type === "result" && event.request_id === "offline-session",
  );
  assert.equal(session.result.session.status, "connected");
  assert.equal(session.result.session.sharing, true);

  send("models", "offline-models");
  const readRetry = await until(
    (event) => event.type === "read_retry_scheduled" &&
      event.request_id === "offline-models",
  );
  assert.equal(readRetry.operation, "models");
  assert.equal(readRetry.category, "rate_limit");
  assert.equal(readRetry.status, 429);
  assert.equal(readRetry.attempt, 1);
  assert.equal(readRetry.next_attempt, 2);
  assert.ok(readRetry.delay_ms >= 0 && readRetry.delay_ms <= 3000);
  const models = await until(
    (event) => event.type === "result" && event.request_id === "offline-models",
  );
  assert.equal(models.result.models[0].slug, "offline-alpha-model");
  assert.equal(
    seen.filter((event) => event.type === "read_retry_scheduled" &&
      event.request_id === "offline-models").length,
    1,
  );

  // Exercise real production request_patch interception and two delta frames.
  send("stream_response", "offline-patched", {
    model: "offline-alpha-model",
    input: [{ role: "user", content: "synthetic-only" }],
    request_patch: { reasoning: { effort: "low" } },
  });
  const completed = await until(
    (event) => event.type === "result" && event.request_id === "offline-patched",
  );
  const patched = JSON.parse(completed.result.text.slice("OFFLINE:".length));
  assert.deepEqual(patched.reasoning, { effort: "low" });
  assert.equal(patched.model, "offline-alpha-model");
  assert.deepEqual(patched.input, [{ role: "user", content: "synthetic-only" }]);
  assert.equal(patched.store, false);
  assert.equal(patched.stream, true);
  assert.equal(
    seen.filter((event) => event.type === "delta" &&
      event.request_id === "offline-patched").length,
    2,
  );
  // Activity event provenance is a real, ordered companion stream, not a
  // fabricated reasoning or tool transcript.
  const activity = seen.filter((event) =>
    event.type === "agent_activity" &&
    event.request_id === "offline-patched",
  );
  assert.deepEqual(activity.map((event) => event.stage), [
    "started", "wire_attempted", "acknowledged",
    "first_text_delta", "text_delta", "completed",
  ]);
  assert.deepEqual(activity.map((event) => event.sequence), [1, 2, 3, 4, 5, 6]);
  assert.ok(activity.every((event) =>
    event.source === "siwc_bridge_observation" &&
    typeof event.elapsed_ms === "number" &&
    event.elapsed_ms >= 0 &&
    /^\d{4}-\d\d-\d\dT/.test(event.observed_at_utc),
  ));
  assert.equal(activity.at(-1).delivery_evidence, "assistant_delta_seen");
  assert.ok(!JSON.stringify(activity).includes("reasoning_summary"));

  // The first stream remains in flight after its opening delta.
  const initial = seen.length;
  send("stream_response", "offline-hold", {
    model: "offline-alpha-model",
    input: "hold-for-cancel",
    request_patch: { text: { verbosity: "low" } },
  });
  await until((event) => event.type === "delta" &&
    event.request_id === "offline-hold", initial);
  const during = seen.length;
  send("stream_response", "offline-overlap", {
    model: "offline-alpha-model", input: "must-not-send",
    request_patch: {},
  });
  send("probe_response", "offline-overlap-probe", {
    model: "offline-alpha-model", input: "must-not-probe",
    request_patch: {},
  });
  const busy = await until((event) => event.type === "error" &&
    event.request_id === "offline-overlap", during);
  assert.equal(busy.error.code, "response_busy");
  const probeBusy = await until((event) => event.type === "error" &&
    event.request_id === "offline-overlap-probe", during);
  assert.equal(probeBusy.error.code, "probe_busy");

  send("cancel_response", "offline-cancel", { target_request_id: "offline-hold" });
  const cancelled = await until((event) => event.type === "result" &&
    event.request_id === "offline-cancel");
  assert.equal(cancelled.result.cancelled, true);
  const aborted = await until((event) => event.type === "error" &&
    event.request_id === "offline-hold");
  assert.equal(aborted.error.code, "aborted");
  const cancelledActivity = seen.filter((event) =>
    event.type === "agent_activity" &&
    event.request_id === "offline-hold",
  );
  assert.equal(cancelledActivity.at(-1).stage, "failed");
  assert.equal(cancelledActivity.at(-1).cancelled, true);
  assert.equal(cancelledActivity.at(-1).delivery_evidence, "assistant_delta_seen");

  // No stale patch from the cancelled stream may affect the recovered turn.
  send("stream_response", "offline-recovered", {
    model: "offline-alpha-model", input: "recovered", request_patch: {},
  });
  const recovered = await until((event) => event.type === "result" &&
    event.request_id === "offline-recovered");
  const normal = JSON.parse(recovered.result.text.slice("OFFLINE:".length));
  assert.equal(normal.reasoning, null);
  assert.equal(normal.text, null);
  assert.equal(normal.store, false);
  assert.equal(normal.stream, true);

  // A normal request with no optional patch must still enforce Chatarium's
  // privacy and streaming invariants, even if the DevKit supplies opposites.
  send("stream_response", "offline-enforced-defaults", {
    model: "offline-alpha-model",
    input: "verify-store-override",
    request_patch: {},
  });
  const enforced = await until(
    (event) => event.type === "result" &&
      event.request_id === "offline-enforced-defaults",
  );
  const enforcedWire = JSON.parse(enforced.result.text.slice("OFFLINE:".length));
  assert.equal(enforcedWire.store, false);
  assert.equal(enforcedWire.stream, true);

  // Unlike read-only model discovery, an inference POST with unknown
  // delivery MUST NOT automatically retry after a network disconnect.
  const beforeAmbiguous = seen.length;
  send("stream_response", "offline-ambiguous", {
    model: "offline-alpha-model",
    input: "simulate-ambiguous-network-loss",
    request_patch: {},
  });
  const attempted = await until(
    (event) => event.type === "transport_progress" &&
      event.request_id === "offline-ambiguous" &&
      event.stage === "wire_attempted",
    beforeAmbiguous,
  );
  assert.ok(attempted);
  const ambiguousError = await until(
    (event) => event.type === "error" &&
      event.request_id === "offline-ambiguous",
    beforeAmbiguous,
  );
  assert.equal(ambiguousError.error.code, "ECONNRESET");
  const ambiguousActivity = seen.filter((event) =>
    event.type === "agent_activity" &&
    event.request_id === "offline-ambiguous",
  );
  assert.deepEqual(ambiguousActivity.map((event) => event.stage), [
    "started", "wire_attempted", "failed",
  ]);
  assert.equal(ambiguousActivity.at(-1).delivery_evidence, "wire_unconfirmed");
  assert.equal(
    seen.filter((event) => event.type === "transport_progress" &&
      event.request_id === "offline-ambiguous" &&
      event.stage === "wire_attempted").length,
    1,
  );
  assert.equal(
    seen.filter((event) => event.type === "read_retry_scheduled" &&
      event.request_id === "offline-ambiguous").length,
    0,
  );

  // A different subsequent user turn remains possible after that uncertainty.
  send("stream_response", "offline-after-uncertain", {
    model: "offline-alpha-model",
    input: "new-independent-turn",
    request_patch: {},
  });
  const afterUncertain = await until(
    (event) => event.type === "result" &&
      event.request_id === "offline-after-uncertain",
  );
  assert.ok(afterUncertain.result.text.includes("new-independent-turn"));

  send("disconnect", "offline-disconnect");
  await until((event) => event.type === "result" &&
    event.request_id === "offline-disconnect");
  const afterDisconnect = seen.length;
  send("stream_response", "offline-disconnected", {
    model: "offline-alpha-model", input: "should-fail", request_patch: {},
  });
  const closed = await until((event) => event.type === "error" &&
    event.request_id === "offline-disconnected", afterDisconnect);
  assert.equal(closed.error.code, "not_connected");

  // Shutdown through the normal stdin EOF path.
  child.stdin.end();
  await new Promise((resolvePromise, rejectPromise) => {
    if (exited) return resolvePromise();
    const timeout = setTimeout(() => rejectPromise(new Error(
      "Bridge did not exit after stdin closed",
    )), 5000);
    child.once("exit", (code) => {
      clearTimeout(timeout);
      assert.equal(code, 0);
      resolvePromise();
    });
  });
});
