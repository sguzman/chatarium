// Persistent local MCP HTTP bridge. One process is shared by Chatarium workers.
// JSON-lines stdin/out: no arbitrary URL, process launch, credentials or retries.
import readline from "node:readline";
import { LocalMcpBroker } from "./client.mjs";

const broker = new LocalMcpBroker();
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of input) {
  if (line.length > 512 * 1024) {
    process.stdout.write(JSON.stringify({ ok: false, error: "MCP input too large" }) + "\n");
    continue;
  }
  let message;
  try {
    message = JSON.parse(line);
    if (!message || typeof message !== "object" || Array.isArray(message)
        || typeof message.id !== "string" || message.id.length > 128) {
      throw new Error("Invalid MCP bridge request");
    }
  } catch (error) {
    process.stdout.write(JSON.stringify({ ok: false, error: String(error.message).slice(0, 500) }) + "\n");
    continue;
  }
  try {
    const result = message.method === "health"
      ? await broker.health(message.provider)
      : await broker.request(message);
    const output = JSON.stringify({ id: message.id, ok: true, result });
    if (Buffer.byteLength(output) > 4 * 1024 * 1024)
      throw new Error("MCP tool result too large");
    process.stdout.write(output + "\n");
  } catch (error) {
    process.stdout.write(JSON.stringify({
      id: message.id, ok: false, error: String(error?.message || error).slice(0, 1200),
      // No reattempt: an interrupted tools/call might already have executed.
      retry_safe: false,
    }) + "\n");
  }
}
