// Sanitized, read-only evidence that a Responses streaming web-search call
// actually completed. Never emit or persist SSE bodies, queries, sources, or
// model text. This runs only for the explicit web-search capability probe.
const MAX_STREAM_BYTES = 8 * 1024 * 1024;
const MAX_PENDING_LINE = 128 * 1024;

export function webSearchCompletedEvent(event) {
  if (!event || typeof event !== "object") return false;
  if (event.type === "response.web_search_call.completed") return true;
  if (event.type === "response.output_item.done") {
    return event.item?.type === "web_search_call" &&
      event.item?.status === "completed";
  }
  if (event.type === "response.completed") {
    return event.response?.output?.some(
      (item) => item?.type === "web_search_call" &&
        item?.status === "completed",
    ) === true;
  }
  return false;
}

export async function observeWebSearchCompletion(response) {
  if (!response?.ok || !response?.body) return false;
  const reader = response.clone().body.getReader();
  const decoder = new TextDecoder("utf-8");
  let pending = "";
  let bytes = 0;
  let completed = false;
  function processLine(line) {
    const trimmed = line.endsWith("\r") ? line.slice(0, -1) : line;
    if (!trimmed.startsWith("data:")) return;
    const payload = trimmed.slice(5).trim();
    if (!payload.startsWith("{") || payload.length > MAX_PENDING_LINE) return;
    try {
      if (webSearchCompletedEvent(JSON.parse(payload))) completed = true;
    } catch {
      // Malformed stream evidence cannot authorize a web-search capability.
    }
  }
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > MAX_STREAM_BYTES) return false;
      pending += decoder.decode(value, { stream: true });
      if (pending.length > MAX_PENDING_LINE) return false;
      let boundary = pending.indexOf("\n");
      while (boundary !== -1) {
        processLine(pending.slice(0, boundary));
        pending = pending.slice(boundary + 1);
        boundary = pending.indexOf("\n");
      }
    }
    pending += decoder.decode();
    if (pending) processLine(pending);
    return completed;
  } catch {
    return false;
  } finally {
    try { await reader.cancel(); } catch { /* stream already closed */ }
    reader.releaseLock();
  }
}
