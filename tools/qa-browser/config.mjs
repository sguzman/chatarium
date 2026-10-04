import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const toolDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(toolDirectory, "../..");

export const qaProfileDirectory = path.join(
  os.homedir(),
  ".local",
  "share",
  "chatarium-qa-browser",
);
export const extensionDirectory = path.join(repositoryRoot, "browser", "edge-bridge");

export const chromiumLaunchOptions = {
  headless: false,
  args: [
    `--disable-extensions-except=${extensionDirectory}`,
    `--load-extension=${extensionDirectory}`,
  ],
};

export const qaBrowserConfig = {
  browserName: "chromium",
  userDataDir: qaProfileDirectory,
  launchOptions: chromiumLaunchOptions,
};

export function describeConfig() {
  return {
    browserName: qaBrowserConfig.browserName,
    userDataDir: qaProfileDirectory,
    extensionDirectory,
    launchOptions: chromiumLaunchOptions,
  };
}
