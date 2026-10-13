import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";
import { writeSync } from "node:fs";
import { lstat } from "node:fs/promises";
import { homedir } from "node:os";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve, join } from "node:path";
import { ResponsesLane } from "./response-lane.mjs";
import { createAgentActivityRecorder } from "./agent-activity.mjs";
import { parseReadRetryPolicy, retryReadOnly } from "./retry-policy.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const devkitEntry = resolve(
  repoRoot,
  "vendor/openai-sign-in-with-chatgpt-devkit/packages/local/dist/index.js",
);

const credentialKeyPattern =
  /^(?:access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|cookie|cookies)$/i;

function hasCredentialField(value) {
  if (Array.isArray(value)) return value.some(hasCredentialField);
  if (!value || typeof value !== "object") return false;

  for (const [key, nested] of Object.entries(value)) {
    if (credentialKeyPattern.test(key)) return true;
    if (hasCredentialField(nested)) return true;
  }
  return false;
}

function emit(value) {
  if (hasCredentialField(value)) {
    writeSync(
      1,
      `${JSON.stringify({
        type: "fatal",
        error: {
          code: "credential_boundary_violation",
          message:
            "The Sign in with ChatGPT runtime attempted to expose credential-bearing state. Chatarium stopped the bridge.",
          retryable: false,
        },
      })}\n`,
    );
    process.exit(70);
  }
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function safeError(error) {
  if (error && typeof error === "object") {
    return {
      code: typeof error.code === "string" ? error.code : "bridge_error",
      message:
        typeof error.message === "string"
          ? error.message
          : "The ChatGPT bridge operation failed.",
      retryable: Boolean(error.retryable),
      ...(Number.isInteger(error.status) ? { status: error.status } : {}),
      ...(typeof error.param === "string" ? { param: error.param } : {}),
      ...(typeof error.requestId === "string"
        ? { request_id: error.requestId }
        : {}),
      ...(typeof error.responseShape === "string" &&
      error.responseShape.length <= 4096
        ? { response_shape: error.responseShape }
        : {}),
    };
  }
  return {
    code: "bridge_error",
    message: "The ChatGPT bridge operation failed.",
    retryable: false,
  };
}

// A hung Secret Service helper must not leave native sign-in pending forever.
const SECRET_TOOL_TIMEOUT_MS = 15_000;

function runSecretTool(args, input) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn("secret-tool", args, {
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const stdout = [];
    let inputFailed = false;
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, SECRET_TOOL_TIMEOUT_MS);
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    // A locked or unavailable keyring can exit before reading stdin. EPIPE
    // must become a failed helper result, not an uncaught sidecar crash.
    child.stdin.on("error", () => { inputFailed = true; });
    child.once("error", (error) => {
      clearTimeout(timer);
      rejectPromise(error);
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      resolvePromise({
        code: timedOut || inputFailed ? 1 : (code ?? 1),
        stdout: Buffer.concat(stdout).toString("utf8"),
        timedOut,
      });
    });
    if (input !== undefined) child.stdin.end(input);
    else child.stdin.end();
  });
}

let cachedCredentialKey;
let pendingCredentialKey;

// Sign-in and DevKit encryption callbacks can overlap. Without a shared
// initialization promise, simultaneous missing-key lookups can each generate
// and store a different key, orphaning credentials written by one caller.
async function loadCredentialKey() {
  if (cachedCredentialKey) return cachedCredentialKey;
  if (!pendingCredentialKey) {
    pendingCredentialKey = loadCredentialKeyUncached().finally(() => {
      pendingCredentialKey = undefined;
    });
  }
  return pendingCredentialKey;
}

async function loadCredentialKeyUncached() {
  if (process.platform !== "linux") {
    throw new Error(
      "This Chatarium alpha currently requires the Linux Secret Service credential backend.",
    );
  }

  let lookup;
  try {
    lookup = await runSecretTool([
      "lookup",
      "application",
      "chatarium",
      "purpose",
      "siwc-credential-key",
    ]);
  } catch {
    throw new Error(
      "The Linux Secret Service helper 'secret-tool' is unavailable. Chatarium will not persist OAuth credentials without an OS-backed secret store.",
    );
  }

  if (lookup.timedOut) {
    throw new Error(
      "The Linux Secret Service lookup timed out. Check that the keyring is unlocked and its service is responding. Saved credentials were not modified.",
    );
  }

  if (lookup.code === 0 && lookup.stdout.trim()) {
    const key = Buffer.from(lookup.stdout.trim(), "base64");
    if (key.length !== 32) {
      throw new Error(
        "The saved Chatarium credential-encryption key is invalid. Refusing to overwrite it.",
      );
    }
    cachedCredentialKey = key;
    return key;
  }

  // The pinned DevKit stores encrypted connection state here by default.
  // If Secret Service lookup fails while that state still exists, issuing a
  // replacement key could orphan the original encrypted credentials. Never
  // mutate the OS keyring in that ambiguous recovery situation.
  const existingAuth = join(homedir(), ".config", "chatarium", "chatgpt-auth.json");
  let savedAuthExists = false;
  try {
    await lstat(existingAuth);
    savedAuthExists = true;
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw new Error(
        "Could not verify saved ChatGPT credential state. No replacement encryption key was created.",
      );
    }
  }
  // ENOENT is the only state in which creating an initial key is allowed.
  if (savedAuthExists) {
    throw new Error(
      "Saved ChatGPT sign-in data exists, but its Secret Service encryption key could not be retrieved. Restore the original keyring access; saved data was preserved.",
    );
  }

  const key = randomBytes(32);
  const stored = await runSecretTool(
    [
      "store",
      "--label=Chatarium Sign in with ChatGPT",
      "application",
      "chatarium",
      "purpose",
      "siwc-credential-key",
    ],
    key.toString("base64"),
  );
  if (stored.timedOut) {
    throw new Error(
      "The Linux Secret Service store operation timed out. Chatarium will not continue sign-in without a protected credential key.",
    );
  }
  if (stored.code !== 0) {
    throw new Error(
      "Chatarium could not store its OAuth encryption key in the Linux Secret Service.",
    );
  }
  cachedCredentialKey = key;
  return key;
}

const credentialEncryption = {
  id: "chatarium-linux-secret-service-aes256gcm-v1",
  async isAvailable() {
    try {
      await loadCredentialKey();
      return true;
    } catch {
      return false;
    }
  },
  async encrypt(plaintext) {
    const key = await loadCredentialKey();
    const nonce = randomBytes(12);
    const cipher = createCipheriv("aes-256-gcm", key, nonce);
    const ciphertext = Buffer.concat([
      cipher.update(plaintext, "utf8"),
      cipher.final(),
    ]);
    const tag = cipher.getAuthTag();
    return Buffer.concat([Buffer.from([1]), nonce, tag, ciphertext]);
  },
  async decrypt(encoded) {
    const bytes = Buffer.from(encoded);
    if (bytes.length < 30 || bytes[0] !== 1) {
      throw new Error("Unsupported Chatarium credential ciphertext.");
    }
    const key = await loadCredentialKey();
    const nonce = bytes.subarray(1, 13);
    const tag = bytes.subarray(13, 29);
    const ciphertext = bytes.subarray(29);
    const decipher = createDecipheriv("aes-256-gcm", key, nonce);
    decipher.setAuthTag(tag);
    return Buffer.concat([
      decipher.update(ciphertext),
      decipher.final(),
    ]).toString("utf8");
  },
};

let local;
try {
  local = await import(pathToFileURL(devkitEntry).href);
} catch {
  emit({
    type: "fatal",
    error: {
      code: "siwc_devkit_not_built",
      message:
        "The pinned OpenAI Sign in with ChatGPT DevKit is not built. Run: node tools/siwc-bridge/bootstrap.mjs",
      retryable: false,
    },
  });
  process.exit(2);
}

const chatgpt = local.createChatGPT({
  appName: "Chatarium",
  appId: "chatarium",
  redirectPort: 0,
  credentialEncryption,
});

const RESPONSES_URL = "https://api.openai.com/v1/responses";
const RESPONSE_PROBE_PATCH_FIELDS = new Set([
  "input",
  "reasoning",
  "text",
  "tools",
]);

function bridgeError(code, message) {
  const error = new Error(message);
  error.code = code;
  error.retryable = false;
  return error;
}

function validateResponseProbePatch(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw bridgeError(
      "invalid_probe_patch",
      "Capability probe request_patch must be a JSON object.",
    );
  }
  const patch = {};
  for (const [key, nested] of Object.entries(value)) {
    if (!RESPONSE_PROBE_PATCH_FIELDS.has(key)) {
      throw bridgeError(
        "invalid_probe_patch",
        `Capability probe cannot patch Responses field ${key}.`,
      );
    }
    patch[key] = nested;
  }
  const encoded = JSON.stringify(patch);
  if (encoded.length > 4 * 1024 * 1024) {
    throw bridgeError(
      "invalid_probe_patch",
      "Capability probe request_patch is too large.",
    );
  }
  return patch;
}

function validateStreamResponsePatch(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw bridgeError(
      "invalid_stream_patch",
      "Normal inference request_patch must be a JSON object.",
    );
  }

  const patch = {};
  for (const [key, nested] of Object.entries(value)) {
    if (key === "reasoning") {
      if (
        !nested ||
        typeof nested !== "object" ||
        Array.isArray(nested) ||
        Object.keys(nested).length !== 1 ||
        nested.effort !== "low"
      ) {
        throw bridgeError(
          "invalid_stream_patch",
          'Normal inference currently permits only reasoning={"effort":"low"}.',
        );
      }
      patch.reasoning = { effort: "low" };
      continue;
    }

    if (key === "text") {
      if (
        !nested ||
        typeof nested !== "object" ||
        Array.isArray(nested) ||
        Object.keys(nested).length !== 1 ||
        nested.verbosity !== "low"
      ) {
        throw bridgeError(
          "invalid_stream_patch",
          'Normal inference currently permits only text={"verbosity":"low"}.',
        );
      }
      patch.text = { verbosity: "low" };
      continue;
    }

    if (key === "tools") {
      if (
        !Array.isArray(nested) ||
        nested.length !== 1 ||
        !nested[0] ||
        typeof nested[0] !== "object" ||
        Array.isArray(nested[0]) ||
        Object.keys(nested[0]).length !== 1 ||
        nested[0].type !== "web_search"
      ) {
        throw bridgeError(
          "invalid_stream_patch",
          'Normal inference currently permits only tools=[{"type":"web_search"}].',
        );
      }
      patch.tools = [{ type: "web_search" }];
      continue;
    }

    throw bridgeError(
      "invalid_stream_patch",
      `Normal inference cannot patch Responses field ${key}.`,
    );
  }

  const encoded = JSON.stringify(patch);
  if (encoded.length > 64 * 1024) {
    throw bridgeError(
      "invalid_stream_patch",
      "Normal inference request_patch is too large.",
    );
  }
  return patch;
}

async function runWithStreamResponsePatch(patch, operation, onTransportProgress = () => {}) {
  // Even without optional tuning, intercept the request to enforce\n  // store:false and stream:true on every ordinary inference turn.

  const originalFetch = globalThis.fetch;
  let patched = false;
  const streamFetch = async (input, init = {}) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.href
          : input?.url;
    const method = String(
      init?.method ?? (input instanceof Request ? input.method : "GET"),
    ).toUpperCase();

    if (url !== RESPONSES_URL || method !== "POST") {
      return originalFetch(input, init);
    }
    if (patched) {
      throw bridgeError(
        "stream_request_count",
        "Normal inference attempted more than one Responses request while applying a request patch.",
      );
    }
    if (typeof init?.body !== "string") {
      throw bridgeError(
        "stream_request_shape",
        "Normal inference expected a JSON Responses request body.",
      );
    }

    let body;
    try {
      body = JSON.parse(init.body);
    } catch {
      throw bridgeError(
        "stream_request_shape",
        "Normal inference could not parse the Responses request body.",
      );
    }
    if (!body || typeof body !== "object" || Array.isArray(body)) {
      throw bridgeError(
        "stream_request_shape",
        "Normal inference received an unexpected Responses request body.",
      );
    }

    patched = true;
    // Keep the interceptor until the entire DevKit operation settles.
    // Otherwise a DevKit-internal retry could escape both the store:false
    // override and the at-most-one Responses POST invariant. The Responses
    // lane separately prevents concurrent inference/probe ownership.
    // Local transport attempt is not evidence that the server received it.
    onTransportProgress("wire_attempted");
    const response = await originalFetch(input, {
      ...init,
      body: JSON.stringify({
        ...body,
        ...patch,
        model: body.model,
        input: body.input,
        instructions: body.instructions,
        store: false,
        stream: true,
      }),
    });
    // An HTTP success is the earliest proof that the server accepted this
    // request. No response at all must stay marked as delivery-unknown.
    if (response.ok) {
      onTransportProgress("acknowledged");
    } else if (response.status >= 400 && response.status <= 599) {
      onTransportProgress("http_rejected", response.status);
    }
    return response;
  };

  globalThis.fetch = streamFetch;
  try {
    const result = await operation();
    if (!patched) {
      throw bridgeError(
        "stream_patch_not_applied",
        "Normal inference did not observe the expected Responses request.",
      );
    }
    return result;
  } finally {
    if (globalThis.fetch === streamFetch) globalThis.fetch = originalFetch;
  }
}

async function runWithResponseProbePatch(patch, operation) {
  const originalFetch = globalThis.fetch;
  let patched = false;
  const probeFetch = async (input, init = {}) => {
    const url =
      typeof input === "string"
        ? input
        : input instanceof URL
          ? input.href
          : input?.url;
    const method = String(
      init?.method ?? (input instanceof Request ? input.method : "GET"),
    ).toUpperCase();
    if (url !== RESPONSES_URL || method !== "POST") {
      return originalFetch(input, init);
    }
    if (patched) {
      throw bridgeError(
        "probe_request_count",
        "Capability probe attempted more than one Responses request.",
      );
    }
    if (typeof init?.body !== "string") {
      throw bridgeError(
        "probe_request_shape",
        "Capability probe expected a JSON Responses request body.",
      );
    }
    let body;
    try {
      body = JSON.parse(init.body);
    } catch {
      throw bridgeError(
        "probe_request_shape",
        "Capability probe could not parse the Responses request body.",
      );
    }
    if (!body || typeof body !== "object" || Array.isArray(body)) {
      throw bridgeError(
        "probe_request_shape",
        "Capability probe received an unexpected Responses request body.",
      );
    }
    patched = true;
    return originalFetch(input, {
      ...init,
      body: JSON.stringify({
        ...body,
        ...patch,
        model: body.model,
        store: false,
        stream: true,
      }),
    });
  };

  globalThis.fetch = probeFetch;
  try {
    const result = await operation();
    if (!patched) {
      throw bridgeError(
        "probe_not_applied",
        "Capability probe did not observe the expected Responses request.",
      );
    }
    return result;
  } finally {
    if (globalThis.fetch === probeFetch) globalThis.fetch = originalFetch;
  }
}


chatgpt.subscribe((session) => {
  emit({ type: "session", session });
});

async function handle(command) {
  const requestId =
    command && typeof command === "object" && !Array.isArray(command) &&
    typeof command.request_id === "string"
      ? command.request_id
      : undefined;
  try {
    // JSON primitives and arrays are valid JSON but not bridge commands.
    // Keep all validation inside the handler's error boundary: otherwise a
    // malformed stdin line creates an unhandled rejection and kills the sidecar.
    if (!command || typeof command !== "object" || Array.isArray(command) ||
        typeof command.type !== "string" || !command.type) {
      throw bridgeError(
        "invalid_bridge_command",
        "Chatarium sent a non-object or missing-type bridge command.",
      );
    }
    switch (command.type) {
      case "session": {
        const session = await chatgpt.getSession();
        emit({ type: "result", request_id: requestId, result: { session } });
        break;
      }
      case "sign_in": {
        // Invalidate an earlier sign-in still waiting on credential preflight.
        // Cancellation during that await must prevent OAuth from starting.
        const generation = ++signInGeneration;
        // Fail before opening an OAuth flow if Chatarium cannot protect the
        // rotating credentials that a successful sign-in would create.
        await loadCredentialKey();
        if (generation !== signInGeneration) {
          throw bridgeError(
            "cancelled",
            "ChatGPT sign-in was cancelled before authorization.",
          );
        }
        const session = await chatgpt.signIn({
          ...(command.new_profile ? { newProfile: true } : {}),
          ...(typeof command.profile_id === "string"
            ? { profileId: command.profile_id }
            : {}),
          ...(command.reconsent ? { reconsent: true } : {}),
        });
        emit({ type: "result", request_id: requestId, result: { session } });
        break;
      }
      case "cancel_sign_in": {
        signInGeneration += 1;
        chatgpt.cancelSignIn();
        emit({ type: "result", request_id: requestId, result: {} });
        break;
      }
      case "profiles": {
        const profiles = await chatgpt.listProfiles();
        emit({ type: "result", request_id: requestId, result: { profiles } });
        break;
      }
      case "select_profile": {
        if (typeof command.profile_id !== "string") {
          throw new Error("select_profile requires profile_id");
        }
        const session = await chatgpt.selectProfile(command.profile_id);
        emit({ type: "result", request_id: requestId, result: { session } });
        break;
      }
      case "models": {
        // Model discovery is a read-only operation, unlike stream_response.
        // A retry cannot duplicate a user-authored inference turn.
        const models = await retryReadOnly(
          () => chatgpt.listModels(),
          {
            // Optional deployment/operator knob; strictly bounded to read-only
            // discovery. Invalid settings fail before dispatching a request.
            policy: parseReadRetryPolicy(
              command.read_retry_policy ?? process.env.CHATARIUM_READ_RETRY_POLICY_JSON,
            ),
            onRetry: (retry) => emit({
              type: "read_retry_scheduled",
              request_id: requestId,
              operation: "models",
              stage: "model_discovery",
              ...retry,
            }),
          },
        );
        emit({ type: "result", request_id: requestId, result: { models } });
        break;
      }
      case "disconnect": {
        // Disconnect must also prevent an unstarted preflight from launching OAuth.
        signInGeneration += 1;
        await chatgpt.disconnect();
        emit({ type: "result", request_id: requestId, result: {} });
        break;
      }
      case "stream_response": {
        if (typeof command.model !== "string" || !command.model.trim()) {
          throw new Error("stream_response requires model");
        }
        if (
          typeof command.input !== "string" &&
          !Array.isArray(command.input)
        ) {
          throw new Error("stream_response requires input");
        }
        const patch = validateStreamResponsePatch(command.request_patch ?? {});
        // The shared one-shot fetch patch must never intercept a different turn.
        const releaseLane = responsesLane.acquire(requestId, "stream");
        const controller = new AbortController();
        if (requestId) responseControllers.set(requestId, controller);
        const activity = createAgentActivityRecorder({
          requestId,
          emit,
        });
        activity.started();
        try {
          const operation = () =>
            chatgpt.streamResponse({
              model: command.model,
              input: command.input,
              ...(typeof command.instructions === "string"
                ? { instructions: command.instructions }
                : {}),
              signal: controller.signal,
              onDelta(delta) {
                activity.delta(delta);
                emit({
                  type: "delta",
                  request_id: requestId,
                  delta,
                });
              },
            });
          const result = await runWithStreamResponsePatch(patch, operation, (stage, status) => {
            if (stage === "http_rejected") {
              // A concrete HTTP rejection is not an ambiguous lost reply.
              activity.rejected(status);
            } else {
              activity.transport(stage);
              emit({ type: "transport_progress", request_id: requestId, stage });
            }
          });
          activity.completed();
          emit({
            type: "result",
            request_id: requestId,
            result,
          });
        } catch (error) {
          activity.failed(error);
          throw error;
        } finally {
          if (requestId && responseControllers.get(requestId) === controller) {
            responseControllers.delete(requestId);
          }
          releaseLane();
        }
        break;
      }
      case "probe_response": {
        if (typeof command.model !== "string" || !command.model.trim()) {
          throw new Error("probe_response requires model");
        }
        if (
          typeof command.input !== "string" &&
          !Array.isArray(command.input)
        ) {
          throw new Error("probe_response requires input");
        }
        const patch = validateResponseProbePatch(command.request_patch ?? {});
        const releaseLane = responsesLane.acquire(requestId, "probe");
        const controller = new AbortController();
        if (requestId) responseControllers.set(requestId, controller);
        try {
          const result = await runWithResponseProbePatch(patch, () =>
            chatgpt.streamResponse({
              model: command.model,
              input: command.input,
              ...(typeof command.instructions === "string"
                ? { instructions: command.instructions }
                : {}),
              signal: controller.signal,
              onDelta(delta) {
                emit({
                  type: "delta",
                  request_id: requestId,
                  delta,
                });
              },
            }),
          );
          emit({
            type: "result",
            request_id: requestId,
            result,
          });
        } finally {
          if (requestId && responseControllers.get(requestId) === controller) {
            responseControllers.delete(requestId);
          }
          releaseLane();
        }
        break;
      }
      case "cancel_response": {
        if (typeof command.target_request_id !== "string") {
          throw new Error("cancel_response requires target_request_id");
        }
        const controller = responseControllers.get(command.target_request_id);
        if (controller) controller.abort();
        emit({
          type: "result",
          request_id: requestId,
          result: { cancelled: Boolean(controller) },
        });
        break;
      }
      default:
        throw new Error("Unknown Chatarium SIWC command.");
    }
  } catch (error) {
    emit({
      type: "error",
      request_id: requestId,
      error: safeError(error),
    });
  }
}

const input = createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});

const inFlight = new Set();
let signInGeneration = 0;
const responseControllers = new Map();
const responsesLane = new ResponsesLane();

input.on("line", (line) => {
  let command;
  try {
    command = JSON.parse(line);
  } catch {
    emit({
      type: "error",
      error: {
        code: "invalid_bridge_command",
        message: "Chatarium sent invalid bridge JSON.",
        retryable: false,
      },
    });
    return;
  }

  const operation = handle(command).finally(() => {
    inFlight.delete(operation);
  });
  inFlight.add(operation);
});

input.once("close", async () => {
  try {
    await Promise.allSettled([...inFlight]);
  } finally {
    process.exit(0);
  }
});

emit({ type: "ready", protocol: 1 });
