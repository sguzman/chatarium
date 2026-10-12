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
  process.stderr.write(
    `warning: Node.js 22+ is needed for Sign in with ChatGPT (found ${process.version}). Local Chatarium will still open.\n`,
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

// Always open the local-first desktop. The native bridge prepares the pinned
// DevKit in a worker, so missing network/package tooling never blocks local
// conversations or turns a first-launch bootstrap problem into a dead app.
process.stdout.write("Launching Chatarium (ChatGPT sign-in prepares in the background)...\n");
await run("cargo", ["run", "-p", "chatarium-desktop", "--bin", "chatarium-desktop"]);
