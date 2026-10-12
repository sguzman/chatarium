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
  const models = await until(
    (event) => event.type === "result" && event.request_id === "offline-models",
  );
  assert.equal(models.result.models[0].slug, "offline-alpha-model");

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
