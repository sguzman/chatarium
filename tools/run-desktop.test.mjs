// Product-first regression: the local GUI launcher must not await a network
// download, credential provider, or pinned DevKit build before starting cargo.
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const launcher = resolve(here, "run-desktop.mjs");

test("local Chatarium starts without a credential helper or DevKit bootstrap", async (t) => {
  if (process.platform === "win32") {
    t.skip("disposable POSIX cargo shim used by Linux product alpha");
    return;
  }
  const root = await mkdtemp(join(tmpdir(), "chatarium-launch-offline-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const argsPath = join(root, "cargo-arguments");
  const cargo = join(root, "cargo");
  await writeFile(cargo, [
    "#!/bin/sh",
    'printf "%s\\n" "$@" > "$CHATARIUM_QA_LAUNCH_RECORD"',
    "",
  ].join("\n"), { mode: 0o700 });
  await chmod(cargo, 0o700);

  // PATH has no git, npm, pnpm, secret-tool, or network client. A successful
  // invocation proves the local GUI path does not synchronously need them.
  const launched = spawnSync(process.execPath, [launcher], {
    env: {
      ...process.env,
      PATH: root,
      CHATARIUM_QA_LAUNCH_RECORD: argsPath,
    },
    encoding: "utf8",
    timeout: 8_000,
  });
  assert.equal(launched.status, 0, launched.stderr || launched.error?.message);
  assert.match(launched.stdout, /Launching Chatarium/);
  assert.doesNotMatch(launched.stdout, /Preparing pinned Sign in with ChatGPT runtime/);
  assert.deepEqual((await readFile(argsPath, "utf8")).trim().split("\n"), [
    "run", "-p", "chatarium-desktop", "--bin", "chatarium-desktop",
  ]);
});
