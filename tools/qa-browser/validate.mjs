import fs from "node:fs/promises";
import { chromium } from "playwright";
import {
  chromiumLaunchOptions,
  describeConfig,
  extensionDirectory,
  qaProfileDirectory,
} from "./config.mjs";

await fs.mkdir(qaProfileDirectory, { recursive: true });

const browserExecutable = chromium.executablePath();
const context = await chromium.launchPersistentContext(
  qaProfileDirectory,
  chromiumLaunchOptions,
);

try {
  const page = context.pages()[0] ?? await context.newPage();
  await page.goto("https://example.com/", { waitUntil: "domcontentloaded" });

  const workers = context.serviceWorkers();
  const serviceWorker = workers.find((worker) => worker.url().startsWith(`chrome-extension://`));
  const observedWorker = serviceWorker ?? await context.waitForEvent("serviceworker", { timeout: 10_000 });
  const workerUrl = observedWorker.url();
  const extensionId = new URL(workerUrl).hostname;
  const manifest = await observedWorker.evaluate(async () => {
    const response = await fetch(chrome.runtime.getURL("manifest.json"));
    return response.json();
  });

  const result = {
    executable: browserExecutable,
    executableExists: (await fs.stat(browserExecutable)).isFile(),
    browserVersion: await context.browser()?.version(),
    config: describeConfig(),
    headed: chromiumLaunchOptions.headless === false,
    page: { url: page.url(), title: await page.title() },
    serviceWorker: { url: workerUrl, extensionId },
    bridge: { version: manifest.version, name: manifest.name },
  };

  console.log(JSON.stringify(result, null, 2));
} finally {
  await context.close();
}
