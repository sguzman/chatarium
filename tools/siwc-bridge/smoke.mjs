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
let requestedProbeGuard = false;
let requestedStreamGuard = false;
let sessionStatus;
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
          JSON.stringify({ type: "session", request_id: "smoke-session" }) + "\n",
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
      sessionStatus = value.result.session.status;
      if (!requestedProbeGuard) {
        requestedProbeGuard = true;
        child.stdin.write(
          JSON.stringify({
            type: "probe_response",
            request_id: "smoke-probe-guard",
            model: "smoke-model",
            input: "probe",
            request_patch: { temperature: 0 },
          }) + "\n",
        );
      }
      return;
    }

    if (
      ready &&
      value?.type === "error" &&
      value?.request_id === "smoke-probe-guard"
    ) {
      if (value?.error?.code !== "invalid_probe_patch") {
        clearTimeout(timeout);
        lines.close();
        rejectPromise(
          new Error(
            `Capability probe guard returned unexpected error code ${value?.error?.code ?? "missing"}.`,
          ),
        );
        return;
      }
      if (!requestedStreamGuard) {
        requestedStreamGuard = true;
        child.stdin.write(
          JSON.stringify({
            type: "stream_response",
            request_id: "smoke-stream-guard",
            model: "smoke-model",
            input: "probe",
            request_patch: { temperature: 0 },
          }) + "\n",
        );
      }
      return;
    }

    if (
      ready &&
      value?.type === "result" &&
      value?.request_id === "smoke-probe-guard"
    ) {
      clearTimeout(timeout);
      lines.close();
      rejectPromise(
        new Error("Capability probe guard accepted a forbidden request field."),
      );
      return;
    }

    if (
      ready &&
      value?.type === "error" &&
      value?.request_id === "smoke-stream-guard"
    ) {
      if (value?.error?.code !== "invalid_stream_patch") {
        clearTimeout(timeout);
        lines.close();
        rejectPromise(
          new Error(
            `Normal inference guard returned unexpected error code ${value?.error?.code ?? "missing"}.`,
          ),
        );
        return;
      }
      clearTimeout(timeout);
      lines.close();
      resolvePromise({
        protocol: 1,
        sessionStatus,
        probeGuard: "invalid_probe_patch",
        streamGuard: "invalid_stream_patch",
      });
      return;
    }

    if (
      ready &&
      value?.type === "result" &&
      value?.request_id === "smoke-stream-guard"
    ) {
      clearTimeout(timeout);
      lines.close();
      rejectPromise(
        new Error("Normal inference guard accepted a forbidden request field."),
      );
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
  `Chatarium SIWC bridge ready: protocol ${result.protocol}; session ${result.sessionStatus}; probe guard ${result.probeGuard}; stream guard ${result.streamGuard}`,
);
