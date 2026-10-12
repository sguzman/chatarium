// Test-only: exercise real Linux sidecar Secret Service protection without
// using the user's keyring, network, browser, or any real credentials.
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, readFile, mkdir, chmod, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createInterface } from "node:readline";

const here = dirname(fileURLToPath(import.meta.url));
const bridge = resolve(here, "../bridge.mjs");
const preload = pathToFileURL(resolve(here, "offline-preload.mjs")).href;

function attempt(path, requestId, timeoutMs = 8000) {
  return new Promise((accept, reject) => {
    const child = spawn(process.execPath, ["--import", preload, bridge], {
      env: { ...process.env, PATH: path, HOME: path, OPENAI_API_KEY: "" },
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let sent = false;
    let reply;
    let complete = false;
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += String(chunk).slice(0, 400); });
    const timeout = setTimeout(() => finish(new Error("Offline sign-in timeout")), timeoutMs);
    function finish(error) {
      if (complete) return;
      complete = true;
      clearTimeout(timeout);
      if (error) {
        child.kill();
        reject(error);
      } else {
        accept(reply);
      }
    }
    child.on("error", finish);
    child.on("exit", (code) => {
      if (code === 0 && reply) finish();
      else finish(new Error("Offline SIWC process exited without response: " + stderr));
    });
    lines.on("line", (line) => {
      if (complete) return;
      let frame;
      try { frame = JSON.parse(line); }
      catch { finish(new Error("Malformed SIWC output")); return; }
      assert.doesNotMatch(JSON.stringify(frame),
        /access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|cookie/i);
      if (frame.type === "ready" && !sent) {
        sent = true;
        child.stdin.write(JSON.stringify({ type: "sign_in", request_id: requestId }) + "\n");
      } else if (frame.request_id === requestId &&
                 (frame.type === "result" || frame.type === "error")) {
        reply = frame;
        child.stdin.end();
      }
    });
  });
}

test("SIWC sign-in fails closed without Secret Service and passes synthetic encrypted preflight", async (t) => {
  if (process.platform !== "linux") {
    t.skip("Linux Secret Service implementation only");
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "chatarium-siwc-offline-"));
  t.after(async () => { await rm(root, { recursive: true, force: true }); });
  // The child sees only this empty directory on PATH, never the host keyring.
  const denied = await attempt(root, "offline-denied");
  assert.equal(denied.type, "error");
  assert.equal(denied.error.code, "bridge_error");
  assert.match(denied.error.message, /secret-tool.*unavailable/i);

  // A disposable fake helper accepts the generated key over stdin, discards
  // it and writes nothing to disk. It cannot reach the real Secret Service.
  const helper = join(root, "secret-tool");
  const script = [
    "#!" + process.execPath,
    'if (process.argv[2] === "lookup") process.exit(1);',
    'else if (process.argv[2] === "store") {',
    '  process.stdin.resume();',
    '  process.stdin.on("end", () => process.exit(0));',
    '} else process.exit(2);',
    "",
  ].join("\n");
  await writeFile(helper, script, { mode: 0o700 });
  await chmod(helper, 0o700);
  const accepted = await attempt(root, "offline-synthetic");
  assert.equal(accepted.type, "result");
  assert.equal(accepted.result.session.status, "connected");
  assert.equal(accepted.result.session.sharing, true);
  assert.equal(accepted.result.session.profileId, "offline-fixture");

  // If the key disappears while encrypted auth state remains, never silently
  // generate and store a different key (which would orphan the state).
  const authDir = join(root, ".config", "chatarium");
  await mkdir(authDir, { recursive: true, mode: 0o700 });
  const protectedPath = join(authDir, "chatgpt-auth.json");
  const existing = '{"version":3,"provider":"synthetic","ciphertext":"protected"}\n';
  await writeFile(protectedPath, existing, { mode: 0o600 });
  const protectedLogin = await attempt(root, "offline-key-lost");
  assert.equal(protectedLogin.type, "error");
  assert.match(protectedLogin.error.message, /saved chatgpt sign-in data exists/i);
  assert.equal(await readFile(protectedPath, "utf8"), existing);
});


test("stalled Secret Service lookup fails closed instead of hanging sign-in", async (t) => {
  if (process.platform !== "linux") {
    t.skip("Linux Secret Service implementation only");
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "chatarium-siwc-key-timeout-"));
  t.after(async () => { await rm(root, { recursive: true, force: true }); });
  // A fake helper that never responds must be terminated; it cannot access
  // the host keyring. A later store would be a credential-safety regression.
  const helper = join(root, "secret-tool");
  await writeFile(helper, [
    "#!" + process.execPath,
    'if (process.argv[2] === "lookup") {',
    '  setInterval(() => {}, 1000);',
    '} else if (process.argv[2] === "store") {',
    '  require("node:fs").writeFileSync(process.env.HOME + "/unexpected-store", "yes");',
    '  process.exit(0);',
    '} else process.exit(2);',
    "",
  ].join("\n"), { mode: 0o700 });
  await chmod(helper, 0o700);
  const reply = await attempt(root, "offline-key-timeout", 21_000);
  assert.equal(reply.type, "error");
  assert.equal(reply.error.code, "bridge_error");
  assert.match(reply.error.message, /secret service lookup timed out/i);
  await assert.rejects(readFile(join(root, "unexpected-store"), "utf8"), {
    code: "ENOENT",
  });
});

test("overlapping Linux sign-ins initialize the same credential key once", async (t) => {
  if (process.platform !== "linux") {
    t.skip("Linux Secret Service implementation only");
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "chatarium-siwc-key-race-"));
  t.after(async () => { await rm(root, { recursive: true, force: true }); });

  // The keyring helper is synthetic. Delayed unsuccessful lookups force both
  // sign-ins to overlap before either can create a key; no host data is used.
  const helper = join(root, "secret-tool");
  await writeFile(helper, [
    "#!" + process.execPath,
    'const fs = require("node:fs");',
    'if (process.argv[2] === "lookup") {',
    '  setTimeout(() => process.exit(1), 80);',
    '} else if (process.argv[2] === "store") {',
    '  process.stdin.resume();',
    '  process.stdin.on("end", () => {',
    '    fs.appendFileSync(process.env.HOME + "/stores.log", "stored\\n");',
    '    process.exit(0);',
    '  });',
    '} else process.exit(2);',
    "",
  ].join("\n"), { mode: 0o700 });
  await chmod(helper, 0o700);

  const requestIds = ["parallel-first", "parallel-second"];
  const replies = await new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(process.execPath, ["--import", preload, bridge], {
      env: { ...process.env, HOME: root, PATH: root, OPENAI_API_KEY: "" },
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    const responses = new Map();
    let stderr = "";
    let started = false;
    let finished = false;
    const timeout = setTimeout(() => finish(new Error("Concurrent offline sign-in timed out")), 10000);
    function finish(error) {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      lines.close();
      if (error) {
        child.kill();
        rejectPromise(error);
      } else {
        resolvePromise(responses);
      }
    }
    child.stderr.on("data", (chunk) => { stderr += String(chunk).slice(0, 400); });
    child.on("error", finish);
    child.on("exit", (code) => {
      if (code === 0 && responses.size === requestIds.length) finish();
      else finish(new Error("Concurrent offline sign-in bridge exited: " + stderr));
    });
    lines.on("line", (line) => {
      if (finished) return;
      try {
        const frame = JSON.parse(line);
        assert.doesNotMatch(JSON.stringify(frame),
          /"(?:access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|cookie|cookies)"\s*:/i);
        if (frame.type === "ready" && !started) {
          started = true;
          for (const request_id of requestIds) {
            child.stdin.write(JSON.stringify({ type: "sign_in", request_id }) + "\n");
          }
        }
        if (requestIds.includes(frame.request_id) &&
            (frame.type === "result" || frame.type === "error")) {
          responses.set(frame.request_id, frame);
          if (responses.size === requestIds.length) child.stdin.end();
        }
      } catch (error) {
        finish(error);
      }
    });
  });
  // Only the newest preflight may start OAuth. The old request is
  // cancelled, without disturbing the one shared keyring write.
  assert.equal(replies.get(requestIds[0]).type, "error");
  assert.equal(replies.get(requestIds[0]).error.code, "cancelled");
  const latest = replies.get(requestIds[1]);
  assert.equal(latest.type, "result", JSON.stringify(latest.error ?? {}));
  assert.equal(latest.result.session.status, "connected");
  const stores = await readFile(join(root, "stores.log"), "utf8");
  assert.deepEqual(stores.trim().split("\n"), ["stored"],
    "Overlapping requests must not generate multiple credential keys");
});

test("cancellation during Linux keyring preflight prevents delayed OAuth launch", async (t) => {
  if (process.platform !== "linux") {
    t.skip("Linux Secret Service implementation only");
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "chatarium-siwc-cancel-preflight-"));
  t.after(async () => { await rm(root, { recursive: true, force: true }); });

  // Delay a disposable helper's key lookup to leave sign-in in preflight.
  // The fake DevKit must never be allowed to complete signIn after cancellation.
  const helper = join(root, "secret-tool");
  await writeFile(helper, [
    "#!" + process.execPath,
    'if (process.argv[2] === "lookup") {',
    '  setTimeout(() => process.exit(1), 150);',
    '} else if (process.argv[2] === "store") {',
    '  process.stdin.resume();',
    '  process.stdin.on("end", () => process.exit(0));',
    '} else process.exit(2);',
    "",
  ].join("\n"), { mode: 0o700 });
  await chmod(helper, 0o700);

  const requestIds = ["cancelled-before-oauth", "cancel-operation"];
  const replies = await new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(process.execPath, ["--import", preload, bridge], {
      env: { ...process.env, HOME: root, PATH: root, OPENAI_API_KEY: "" },
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    const responses = new Map();
    let stderr = "";
    let sent = false;
    let finished = false;
    const timeout = setTimeout(() => finish(new Error("Cancelled sign-in timed out")), 10000);
    function finish(error) {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      lines.close();
      if (error) {
        child.kill();
        rejectPromise(error);
      } else {
        resolvePromise(responses);
      }
    }
    child.stderr.on("data", (chunk) => { stderr += String(chunk).slice(0, 400); });
    child.on("error", finish);
    child.on("exit", (code) => {
      if (code === 0 && responses.size === requestIds.length) finish();
      else finish(new Error("Cancelled SIWC sidecar exited: " + stderr));
    });
    lines.on("line", (line) => {
      if (finished) return;
      try {
        const frame = JSON.parse(line);
        // Check credential-bearing JSON keys, not safe error prose.
        assert.doesNotMatch(JSON.stringify(frame),
          /"(?:access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|cookie|cookies)"\s*:/i);
        if (frame.type === "ready" && !sent) {
          sent = true;
          child.stdin.write(JSON.stringify({
            type: "sign_in", request_id: requestIds[0],
          }) + "\n");
          child.stdin.write(JSON.stringify({
            type: "cancel_sign_in", request_id: requestIds[1],
          }) + "\n");
        }
        if (requestIds.includes(frame.request_id) &&
            (frame.type === "result" || frame.type === "error")) {
          responses.set(frame.request_id, frame);
          if (responses.size === requestIds.length) child.stdin.end();
        }
      } catch (error) {
        finish(error);
      }
    });
  });

  assert.equal(replies.get(requestIds[1]).type, "result",
    "The cancellation command must be acknowledged");
  const cancelled = replies.get(requestIds[0]);
  assert.equal(cancelled.type, "error", "Delayed OAuth must never start");
  assert.equal(cancelled.error.code, "cancelled");
  assert.match(cancelled.error.message, /cancelled before authorization/i);
});
