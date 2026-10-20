// One-command, read-only OpenViking readiness check followed by Chatarium.
// This launcher never creates a checkpoint: production writes still require
// the durable outbox and the app's own exclusive-create safety preflight.
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { LocalMcpBroker } from "./local-http-mcp/client.mjs";
import { probeOpenViking } from "./local-http-mcp/openviking-live-probe.mjs";

export async function runOpenVikingQa({
  probe = () => probeOpenViking({ broker: new LocalMcpBroker({ timeoutMs: 5000 }) }),
  launch = () => import("./run-desktop.mjs"),
  output = process.stdout,
  autocapture = process.env.CHATARIUM_OPENVIKING_AUTOCAPTURE,
} = {}) {
  let ready = false;
  try {
    const result = await probe();
    ready = result?.ok === true
      && result?.mode === "discovery-only"
      && result?.advertisedReadAndWrite === true
      && result?.schemaCompatible === true
      && result?.remoteMutation === false;
  } catch {
    // Do not print raw transport errors or server metadata, which might
    // contain account details, host configuration or authentication material.
  }
  output.write(ready
    ? "OpenViking MCP discovery: PASS (read-only; checkpoint writes not yet verified).\n"
    : "OpenViking MCP discovery: UNAVAILABLE. Chatarium still opens; checkpoint verification requires the local service.\n");
  if (autocapture === "0") {
    output.write("OpenViking automatic checkpoints are disabled by CHATARIUM_OPENVIKING_AUTOCAPTURE=0.\n");
  }
  output.write("Launching Chatarium. Its normal checkpoint safety preflight runs only when there is a queued memory.\n");
  await launch();
  return ready;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runOpenVikingQa().catch(() => {
    process.stderr.write("Chatarium launch failed. The read-only probe does not start or change services.\n");
    process.exitCode = 1;
  });
}
