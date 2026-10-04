import fs from "node:fs/promises";
import { chromium } from "playwright";
import { extensionDirectory, qaProfileDirectory, chromiumLaunchOptions } from "./config.mjs";

await fs.mkdir(qaProfileDirectory, { recursive: true });

let context;
try {
  context = await chromium.launchPersistentContext(qaProfileDirectory, chromiumLaunchOptions);
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  if (/singleton|lock|already running|user data directory/i.test(message)) {
    console.error(`Chatarium QA browser profile is already in use: ${qaProfileDirectory}`);
    console.error("Close the existing chatarium-qa-browser process before launching another one.");
  } else {
    console.error("Unable to launch Playwright-managed Chromium.");
    console.error(`Expected extension directory: ${extensionDirectory}`);
    console.error(message);
  }
  process.exitCode = 1;
  process.exit();
}

const page = context.pages()[0] ?? await context.newPage();
if (page.url() === "about:blank") {
  await page.goto("about:blank");
}

console.log("Chatarium QA Chromium is running.");
console.log(`user-data-dir: ${qaProfileDirectory}`);
console.log(`extension: ${extensionDirectory}`);
console.log("Close the Chromium window to end this launcher.");

await new Promise((resolve) => context.on("close", resolve));
