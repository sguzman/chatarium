import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const bridge = resolve(here, "bridge.mjs");

const child = spawn(process.execPath, [bridge], {
  stdio: ["pipe", "pipe", "inherit"],
  shell: false,
});

const lines = createInterface({
  input: child.stdout,
  crlfDelay: Infinity,
});

const result = await new Promise((resolvePromise, rejectPromise) => {
  const timeout = setTimeout(() => {
    rejectPromise(new Error("Sign in with ChatGPT bridge did not become ready."));
  }, 10_000);

  child.once("error", (error) => {
    clearTimeout(timeout);
    rejectPromise(error);
  });

  lines.once("line", (line) => {
    clearTimeout(timeout);
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      rejectPromise(new Error("Sign in with ChatGPT bridge emitted invalid JSON."));
      return;
    }
    if (value?.type !== "ready" || value?.protocol !== 1) {
      rejectPromise(
        new Error("Sign in with ChatGPT bridge did not emit the expected ready event."),
      );
      return;
    }
    resolvePromise(value);
  });
});

child.stdin.end();
await new Promise((resolvePromise) => {
  child.once("exit", resolvePromise);
  setTimeout(() => {
    child.kill();
    resolvePromise();
  }, 5_000).unref();
});

console.log(`Chatarium SIWC bridge ready: protocol ${result.protocol}`);
