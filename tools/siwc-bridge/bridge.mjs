import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, resolve } from "node:path";

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
    process.stdout.write(
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
      ...(typeof error.requestId === "string"
        ? { request_id: error.requestId }
        : {}),
    };
  }
  return {
    code: "bridge_error",
    message: "The ChatGPT bridge operation failed.",
    retryable: false,
  };
}

function runSecretTool(args, input) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn("secret-tool", args, {
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
    });
    const stdout = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.once("error", (error) => rejectPromise(error));
    child.once("exit", (code) => {
      resolvePromise({
        code: code ?? 1,
        stdout: Buffer.concat(stdout).toString("utf8"),
      });
    });
    if (input !== undefined) child.stdin.end(input);
    else child.stdin.end();
  });
}

let cachedCredentialKey;

async function loadCredentialKey() {
  if (cachedCredentialKey) return cachedCredentialKey;
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

chatgpt.subscribe((session) => {
  emit({ type: "session", session });
});

async function handle(command) {
  const requestId =
    typeof command.request_id === "string" ? command.request_id : undefined;
  try {
    switch (command.type) {
      case "session": {
        const session = await chatgpt.getSession();
        emit({ type: "result", request_id: requestId, result: { session } });
        break;
      }
      case "sign_in": {
        // Fail before opening an OAuth flow if Chatarium cannot protect the
        // rotating credentials that a successful sign-in would create.
        await loadCredentialKey();
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
        const models = await chatgpt.listModels();
        emit({ type: "result", request_id: requestId, result: { models } });
        break;
      }
      case "disconnect": {
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
        const result = await chatgpt.streamResponse({
          model: command.model,
          input: command.input,
          ...(typeof command.instructions === "string"
            ? { instructions: command.instructions }
            : {}),
          onDelta(delta) {
            emit({
              type: "delta",
              request_id: requestId,
              delta,
            });
          },
        });
        emit({
          type: "result",
          request_id: requestId,
          result,
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
