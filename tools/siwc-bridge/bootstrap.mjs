import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { access, readFile, writeFile } from "node:fs/promises";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../..");
const devkitRoot = resolve(
  repoRoot,
  "vendor/openai-sign-in-with-chatgpt-devkit",
);
const localRoot = resolve(devkitRoot, "packages/local");
const expectedCommit = "f723814abdccec135b519c451fb6e1992ee5e933";
const bootstrapStamp = resolve(
  localRoot,
  "dist",
  ".chatarium-bootstrap-commit",
);

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

const submoduleStatus = await capture("git", [
  "submodule",
  "status",
  "--",
  "vendor/openai-sign-in-with-chatgpt-devkit",
]);
const statusMatch = submoduleStatus.match(/^([ +\-U])?([0-9a-f]{40})\s/);
const commit = statusMatch?.[2];
const prefix = statusMatch?.[1] ?? " ";
if (commit !== expectedCommit || prefix !== " ") {
  throw new Error(
    `Pinned Sign in with ChatGPT DevKit mismatch: expected checked-out ${expectedCommit}, got ${commit ?? "unknown"} (status ${JSON.stringify(prefix)})`,
  );
}

let alreadyBuilt = false;
try {
  const [stamp] = await Promise.all([
    readFile(bootstrapStamp, "utf8"),
    access(resolve(localRoot, "dist", "index.js")),
    access(resolve(devkitRoot, "node_modules", "jose", "package.json")),
    access(resolve(devkitRoot, "node_modules", "proper-lockfile", "package.json")),
  ]);
  alreadyBuilt = stamp.trim() === expectedCommit;
} catch {
  alreadyBuilt = false;
}

if (alreadyBuilt) {
  console.log("Chatarium Sign in with ChatGPT DevKit is already ready.");
  process.exit(0);
}

await runPackageTool(
  "npm",
  [
    "ci",
    "--workspace",
    "@siwc/local",
    "--include-workspace-root",
  ],
  { cwd: devkitRoot },
);

await runPackageTool(
  "npm",
  ["run", "build", "--workspace", "@siwc/local"],
  { cwd: devkitRoot },
);

await writeFile(bootstrapStamp, `${expectedCommit}\n`, "utf8");
console.log("Chatarium Sign in with ChatGPT DevKit is ready.");
