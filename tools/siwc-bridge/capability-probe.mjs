import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const bridgePath = resolve(here, "bridge.mjs");
const bootstrapPath = resolve(here, "bootstrap.mjs");

function usage() {
  return `Usage: node tools/siwc-bridge/capability-probe.mjs [options]\n\nOptions:\n  --model <slug>     Account-visible model slug. Defaults to the first listed model.\n  --probe <name>     Run one probe instead of all.\n  --json             Emit one JSON result object.\n  --help             Show this help.\n\nProbe names:\n  baseline, image_input, file_input, function_tools, additional_tools,\n  web_search, reasoning, verbosity, structured_output\n`;
}

function parseArgs(argv) {
  const options = { model: undefined, probe: "all", json: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help") {
      process.stdout.write(usage());
      process.exit(0);
    } else if (arg === "--json") {
      options.json = true;
    } else if (arg === "--model" || arg === "--probe") {
      const value = argv[index + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`${arg} requires a value.`);
      }
      index += 1;
      if (arg === "--model") options.model = value;
      else options.probe = value;
    } else {
      throw new Error(`Unknown option: ${arg}`);
    }
  }
  return options;
}

function run(command, args, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd: options.cwd,
      stdio: options.stdio ?? "inherit",
      shell: false,
    });
    child.once("error", rejectPromise);
    child.once("exit", (code) => {
      if (code === 0) resolvePromise();
      else rejectPromise(
        new Error(`${command} exited with status ${code ?? "unknown"}`),
      );
    });
  });
}

function probeDefinitions() {
  const tinyPng =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
  const tinyText =
    "data:text/plain;base64,Q2hhdGFyaXVtIGNhcGFiaWxpdHkgcHJvYmUuCg==";
  const functionTool = {
    type: "function",
    name: "echo_probe",
    description: "Return a supplied probe value unchanged.",
    parameters: {
      type: "object",
      properties: { value: { type: "string" } },
      required: ["value"],
      additionalProperties: false,
    },
  };

  return {
    baseline: {
      prompt: "Reply with exactly PROBE_OK.",
      patch: {},
    },
    image_input: {
      prompt: "Reply with exactly PROBE_OK.",
      patch: {
        input: [
          {
            role: "user",
            content: [
              {
                type: "input_text",
                text: "This is a capability probe. Reply with exactly IMAGE_OK.",
              },
              { type: "input_image", image_url: tinyPng },
            ],
          },
        ],
      },
    },
    file_input: {
      prompt: "Reply with exactly PROBE_OK.",
      patch: {
        input: [
          {
            role: "user",
            content: [
              {
                type: "input_text",
                text: "Read the attached capability probe file and reply with exactly FILE_OK.",
              },
              {
                type: "input_file",
                filename: "chatarium-probe.txt",
                file_data: tinyText,
              },
            ],
          },
        ],
      },
    },
    function_tools: {
      prompt: "Reply with exactly FUNCTION_TOOL_OK. Do not call any tool.",
      patch: {
        tools: [
          {
            type: "namespace",
            name: "chatarium_probe",
            description: "Tiny functions used only to probe tool admission.",
            tools: [functionTool],
          },
        ],
      },
    },
    additional_tools: {
      prompt: "Reply with exactly PROBE_OK.",
      patch: {
        input: [
          {
            type: "additional_tools",
            role: "developer",
            tools: [functionTool],
          },
          {
            role: "user",
            content: "Reply with exactly ADDITIONAL_TOOLS_OK. Do not call any tool.",
          },
        ],
      },
    },
    web_search: {
      prompt: "Reply with exactly WEB_SEARCH_OK. Do not search the web.",
      patch: { tools: [{ type: "web_search" }] },
    },
    reasoning: {
      prompt: "Reply with exactly REASONING_OK.",
      patch: { reasoning: { effort: "low" } },
    },
    verbosity: {
      prompt: "Reply with exactly VERBOSITY_OK.",
      patch: { text: { verbosity: "low" } },
    },
    structured_output: {
      prompt: 'Return JSON with one boolean field named "ok" set to true.',
      patch: {
        text: {
          format: {
            type: "json_schema",
            name: "chatarium_probe",
            schema: {
              type: "object",
              properties: { ok: { type: "boolean" } },
              required: ["ok"],
              additionalProperties: false,
            },
            strict: true,
          },
        },
      },
    },
  };
}

function classify(error) {
  if (error.code === "subscription_sharing_unsupported_capability") {
    return "unsupported_route";
  }
  if (error.code === "model_not_found") return "model_unavailable";
  if (
    error.code === "invalid_request" ||
    error.code === "invalid_request_error" ||
    error.status === 400 ||
    error.status === 422
  ) {
    return "rejected";
  }
  return "error";
}

function startBridge() {
  const child = spawn(process.execPath, [bridgePath], {
    stdio: ["pipe", "pipe", "inherit"],
    shell: false,
  });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const pending = new Map();
  let nextRequest = 1;
  let readyResolve;
  let readyReject;
  const ready = new Promise((resolvePromise, rejectPromise) => {
    readyResolve = resolvePromise;
    readyReject = rejectPromise;
  });

  const failPending = (error) => {
    readyReject(error);
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  };

  child.once("error", failPending);
  child.once("exit", (code) => {
    if (code !== 0) {
      failPending(new Error(`Sign in with ChatGPT bridge exited with status ${code}.`));
    }
  });

  lines.on("line", (line) => {
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      failPending(new Error("Sign in with ChatGPT bridge emitted invalid JSON."));
      return;
    }
    if (value?.type === "fatal") {
      const error = Object.assign(
        new Error(value?.error?.message ?? "Sign in with ChatGPT bridge failed."),
        value?.error ?? {},
      );
      failPending(error);
      return;
    }
    if (value?.type === "ready") {
      readyResolve(value);
      return;
    }
    const requestId = value?.request_id;
    if (typeof requestId !== "string") return;
    const request = pending.get(requestId);
    if (!request) return;
    if (value?.type === "result") {
      pending.delete(requestId);
      request.resolve(value.result ?? {});
    } else if (value?.type === "error") {
      pending.delete(requestId);
      request.reject(
        Object.assign(
          new Error(value?.error?.message ?? "Bridge request failed."),
          value?.error ?? {},
        ),
      );
    }
  });

  return {
    ready,
    request(type, payload = {}) {
      const requestId = `capability-probe-${nextRequest++}`;
      return new Promise((resolvePromise, rejectPromise) => {
        const timeout = setTimeout(() => {
          pending.delete(requestId);
          child.stdin.write(
            `${JSON.stringify({
              type: "cancel_response",
              request_id: `cancel:${requestId}`,
              target_request_id: requestId,
            })}\n`,
          );
          rejectPromise(
            Object.assign(new Error("Capability probe request timed out."), {
              code: "probe_timeout",
              retryable: true,
            }),
          );
        }, 210_000);
        timeout.unref();
        pending.set(requestId, {
          resolve(value) {
            clearTimeout(timeout);
            resolvePromise(value);
          },
          reject(error) {
            clearTimeout(timeout);
            rejectPromise(error);
          },
        });
        child.stdin.write(
          `${JSON.stringify({ type, request_id: requestId, ...payload })}\n`,
        );
      });
    },
    async close() {
      lines.close();
      child.stdin.end();
      await new Promise((resolvePromise) => {
        let settled = false;
        const finish = () => {
          if (!settled) {
            settled = true;
            resolvePromise();
          }
        };
        child.once("exit", finish);
        setTimeout(() => {
          child.kill();
          finish();
        }, 5_000).unref();
      });
    },
  };
}

const options = parseArgs(process.argv.slice(2));
const definitions = probeDefinitions();
if (options.probe !== "all" && !Object.hasOwn(definitions, options.probe)) {
  throw new Error(
    `Unknown probe ${options.probe}. Choose one of: ${Object.keys(definitions).join(", ")}.`,
  );
}

await run(process.execPath, [bootstrapPath], {
  stdio: options.json ? ["ignore", "ignore", "inherit"] : "inherit",
});
const bridge = startBridge();
let report;
try {
  await bridge.ready;
  const sessionResult = await bridge.request("session");
  const session = sessionResult.session;
  if (session?.status !== "connected" || session?.sharing !== true) {
    throw new Error(
      "Capability probes require an existing connected ChatGPT profile with plan-usage sharing enabled.",
    );
  }

  const modelResult = await bridge.request("models");
  const models = Array.isArray(modelResult.models) ? modelResult.models : [];
  if (models.length === 0) throw new Error("ChatGPT returned no account-visible models.");
  const selectedModel = options.model
    ? models.find((model) => model.slug === options.model)
    : models[0];
  if (!selectedModel) {
    throw new Error(
      `Model ${options.model} is not in the current account-visible model catalog.`,
    );
  }

  const names =
    options.probe === "all"
      ? Object.keys(definitions)
      : options.probe === "baseline"
        ? ["baseline"]
        : ["baseline", options.probe];
  const results = [];
  let baselineFailed = false;
  for (const name of names) {
    if (baselineFailed && name !== "baseline") {
      results.push({
        name,
        status: "not_run",
        reason: "baseline_failed",
      });
      continue;
    }
    const definition = definitions[name];
    try {
      const result = await bridge.request("probe_response", {
        model: selectedModel.slug,
        input: definition.prompt,
        request_patch: definition.patch,
      });
      results.push({
        name,
        status: "supported",
        completed: true,
        text_received: typeof result?.text === "string" && result.text.length > 0,
      });
    } catch (error) {
      const entry = {
        name,
        status: classify(error),
        code: typeof error.code === "string" ? error.code : "bridge_error",
        message: error.message,
        ...(Number.isInteger(error.status) ? { status_code: error.status } : {}),
      };
      results.push(entry);
      if (name === "baseline") baselineFailed = true;
    }
  }

  report = {
    model: selectedModel.slug,
    display_name: selectedModel.displayName ?? selectedModel.slug,
    probes: results,
  };
} finally {
  await bridge.close();
}

if (options.json) {
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
} else {
  console.log(`Chatarium capability probe model: ${report.model}`);
  for (const probe of report.probes) {
    const detail = probe.code ? ` (${probe.code})` : "";
    console.log(`${probe.name}: ${probe.status}${detail}`);
  }
}
