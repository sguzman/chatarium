// Test-only: exercise real Linux sidecar Secret Service protection without
// using the user's keyring, network, browser, or any real credentials.
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, writeFile, chmod, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createInterface } from "node:readline";

const here = dirname(fileURLToPath(import.meta.url));
const bridge = resolve(here, "../bridge.mjs");
const preload = pathToFileURL(resolve(here, "offline-preload.mjs")).href;

function attempt(path, requestId) {
  return new Promise((accept, reject) => {
    const child = spawn(process.execPath, ["--import", preload, bridge], {
      env: { ...process.env, PATH: path, OPENAI_API_KEY: "" },
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    let sent = false;
    let reply;
    let complete = false;
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += String(chunk).slice(0, 400); });
    const timeout = setTimeout(() => finish(new Error("Offline sign-in timeout")), 8000);
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
});
