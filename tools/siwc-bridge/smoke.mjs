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

let ready = false;
let requestedSession = false;
const result = await new Promise((resolvePromise, rejectPromise) => {
  const timeout = setTimeout(() => {
    lines.close();
    rejectPromise(
      new Error("Sign in with ChatGPT bridge did not complete startup smoke."),
    );
  }, 10_000);

  child.once("error", (error) => {
    clearTimeout(timeout);
    lines.close();
    rejectPromise(error);
  });

  lines.on("line", (line) => {
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      clearTimeout(timeout);
      lines.close();
      rejectPromise(new Error("Sign in with ChatGPT bridge emitted invalid JSON."));
      return;
    }

    const serialized = JSON.stringify(value);
    if (
      /access[_-]?token|refresh[_-]?token|id[_-]?token|authorization/i.test(
        serialized,
      )
    ) {
      clearTimeout(timeout);
      lines.close();
      rejectPromise(
        new Error("Sign in with ChatGPT bridge exposed credential-shaped output."),
      );
      return;
    }

    // The official DevKit subscription immediately publishes its initial
    // disconnected session snapshot before Chatarium emits its own ready marker.
    if (value?.type === "session") return;

    if (value?.type === "fatal") {
      clearTimeout(timeout);
      lines.close();
      rejectPromise(
        new Error(value?.error?.message ?? "Sign in with ChatGPT bridge failed to start."),
      );
      return;
    }

    if (value?.type === "ready" && value?.protocol === 1) {
      ready = true;
      if (!requestedSession) {
        requestedSession = true;
        child.stdin.write(
          JSON.stringify({ type: "session", request_id: "smoke-session" }) + "\\n",
        );
      }
      return;
    }

    if (
      ready &&
      value?.type === "result" &&
      value?.request_id === "smoke-session" &&
      typeof value?.result?.session?.status === "string" &&
      typeof value?.result?.session?.sharing === "boolean"
    ) {
      clearTimeout(timeout);
      lines.close();
      resolvePromise({
        protocol: 1,
        sessionStatus: value.result.session.status,
      });
    }
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

console.log(
  `Chatarium SIWC bridge ready: protocol ${result.protocol}; session ${result.sessionStatus}`,
);
