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

function packageToolAvailable(command) {
  return new Promise((resolvePromise) => {
    const executable =
      process.platform === "win32" ? process.env.ComSpec ?? "cmd.exe" : command;
    const args =
      process.platform === "win32"
        ? ["/d", "/s", "/c", command, "--version"]
        : ["--version"];
    const child = spawn(executable, args, {
      cwd: repoRoot,
      stdio: "ignore",
      shell: false,
    });
    child.once("error", () => resolvePromise(false));
    child.once("exit", (code) => resolvePromise(code === 0));
  });
}

const fallbackNpmVersion = "11.6.2";

async function runPinnedNpm(args, options = {}) {
  const forcePnpm = process.env.CHATARIUM_FORCE_PNPM_NPM === "1";
  if (!forcePnpm && (await packageToolAvailable("npm"))) {
    return runPackageTool("npm", args, options);
  }
  if (await packageToolAvailable("pnpm")) {
    console.log(
      `npm is not installed; using pnpm to run pinned npm@${fallbackNpmVersion}.`,
    );
    return runPackageTool(
      "pnpm",
      ["dlx", `npm@${fallbackNpmVersion}`, ...args],
      options,
    );
  }
  throw new Error(
    "Chatarium needs npm or pnpm to prepare the pinned Sign in with ChatGPT runtime. Neither command is available.",
  );
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

async function submoduleStatus() {
  const output = await capture("git", [
    "submodule",
    "status",
    "--",
    "vendor/openai-sign-in-with-chatgpt-devkit",
  ]);
  const match = output.match(/^([ +\-U])?([0-9a-f]{40})\s/);
  return {
    prefix: match?.[1] ?? " ",
    commit: match?.[2] ?? null,
  };
}

async function trackedDevkitChanges() {
  try {
    return await capture(
      "git",
      ["status", "--porcelain", "--untracked-files=no"],
      { cwd: devkitRoot },
    );
  } catch {
    return "";
  }
}

let status = await submoduleStatus();
if (status.prefix !== "-" && status.commit && status.commit !== expectedCommit) {
  const trackedChanges = await trackedDevkitChanges();
  if (trackedChanges) {
    throw new Error(
      "Pinned Sign in with ChatGPT DevKit is on the wrong commit and has tracked local modifications; refusing to overwrite credential-owning source.",
    );
  }
}

if (status.prefix !== " " || status.commit !== expectedCommit) {
  console.log(
    "Synchronizing pinned Sign in with ChatGPT DevKit to the commit recorded by Chatarium...",
  );
  await run("git", [
    "submodule",
    "update",
    "--init",
    "--depth",
    "1",
    "vendor/openai-sign-in-with-chatgpt-devkit",
  ]);
  status = await submoduleStatus();
}

if (status.commit !== expectedCommit || status.prefix !== " ") {
  throw new Error(
    `Pinned Sign in with ChatGPT DevKit mismatch after synchronization: expected checked-out ${expectedCommit}, got ${status.commit ?? "unknown"} (status ${JSON.stringify(status.prefix)})`,
  );
}

const trackedChanges = await trackedDevkitChanges();
if (trackedChanges) {
  throw new Error(
    "Pinned Sign in with ChatGPT DevKit has tracked local modifications; refusing to execute modified credential-owning source.",
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

await runPinnedNpm(
  [
    "ci",
    "--workspace",
    "@siwc/local",
    "--include-workspace-root",
  ],
  { cwd: devkitRoot },
);

await runPinnedNpm(
  ["run", "build", "--workspace", "@siwc/local"],
  { cwd: devkitRoot },
);

await writeFile(bootstrapStamp, `${expectedCommit}\n`, "utf8");
console.log("Chatarium Sign in with ChatGPT DevKit is ready.");
