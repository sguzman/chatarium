import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..");

function run(command, args, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? repoRoot,
      stdio: "inherit",
      shell: false,
    });
    child.once("error", rejectPromise);
    child.once("exit", (code, signal) => {
      if (code === 0) resolvePromise();
      else {
        rejectPromise(
          new Error(
            `${command} exited with ${signal ? `signal ${signal}` : `status ${code ?? "unknown"}`}`,
          ),
        );
      }
    });
  });
}

const [nodeMajor] = process.versions.node.split(".").map(Number);
if (nodeMajor < 22) {
  throw new Error(
    `Chatarium Sign in with ChatGPT requires Node.js 22 or newer; current runtime is ${process.version}.`,
  );
}

if (process.platform === "linux") {
  const secretTool = spawnSync("secret-tool", ["--help"], {
    stdio: "ignore",
    shell: false,
  });
  if (secretTool.error?.code === "ENOENT") {
    process.stderr.write(
      "warning: secret-tool was not found. Chatarium will still launch, but Sign in with ChatGPT will fail closed until the Linux Secret Service helper is installed.\n",
    );
  }
}

process.stdout.write("Preparing pinned Sign in with ChatGPT runtime...\n");
await run(process.execPath, [resolve(repoRoot, "tools/siwc-bridge/bootstrap.mjs")]);

process.stdout.write("Launching Chatarium...\n");
await run("cargo", ["run", "-p", "chatarium-desktop"]);
