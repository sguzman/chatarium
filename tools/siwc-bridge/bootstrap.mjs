import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { access } from "node:fs/promises";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const devkitRoot = resolve(
  repoRoot,
  "vendor/openai-sign-in-with-chatgpt-devkit",
);
const localRoot = resolve(devkitRoot, "packages/local");
const expectedCommit = "f723814abdccec135b519c451fb6e1992ee5e933";

function run(command, args, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? repoRoot,
      stdio: "inherit",
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

function runPackageTool(command, args, options = {}) {
  if (process.platform === "win32") {
    return run(process.env.ComSpec ?? "cmd.exe", [
      "/d",
      "/s",
      "/c",
      command,
      ...args,
    ], options);
  }
  return run(command, args, options);
}

function capture(command, args, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? repoRoot,
      stdio: ["ignore", "pipe", "inherit"],
      shell: false,
    });
    const stdout = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.once("error", rejectPromise);
    child.once("exit", (code) => {
      if (code === 0) {
        resolvePromise(Buffer.concat(stdout).toString("utf8").trim());
      } else {
        rejectPromise(
          new Error(`${command} exited with status ${code ?? "unknown"}`),
        );
      }
    });
  });
}

const [major] = process.versions.node.split(".").map(Number);
if (major < 22) {
  throw new Error("Chatarium Sign in with ChatGPT requires Node.js 22 or newer.");
}

try {
  await access(resolve(devkitRoot, "packages/local/package.json"));
} catch {
  await run("git", [
    "submodule",
    "update",
    "--init",
    "--depth",
    "1",
    "vendor/openai-sign-in-with-chatgpt-devkit",
  ]);
}

const commit = await capture("git", ["rev-parse", "HEAD"], {
  cwd: devkitRoot,
});
if (commit !== expectedCommit) {
  throw new Error(
    `Pinned Sign in with ChatGPT DevKit mismatch: expected ${expectedCommit}, got ${commit}`,
  );
}

await runPackageTool(
  "npm",
  [
    "install",
    "--no-save",
    "--no-package-lock",
    "--include=dev",
    "--workspaces=false",
    "typescript@7.0.2",
    "@types/node@24.0.0",
  ],
  { cwd: localRoot },
);

await run(
  process.execPath,
  [
    resolve(localRoot, "node_modules/typescript/bin/tsc"),
    "-p",
    "tsconfig.json",
  ],
  { cwd: localRoot },
);

console.log("Chatarium Sign in with ChatGPT DevKit is ready.");
