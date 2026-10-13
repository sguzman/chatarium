// Source-observed, chronological SIWC inference activity.
// This records only events the bridge actually sees. It never invents
// reasoning summaries or upstream tool activity that the DevKit does not emit.
const STAGES = new Set([
  "started", "wire_attempted", "acknowledged", "first_text_delta",
  "text_delta", "completed", "failed",
]);
const TERMINAL = new Set(["completed", "failed"]);

export function createAgentActivityRecorder({
  requestId,
  emit,
  monotonicNow = () => performance.now(),
  utcNow = () => new Date().toISOString(),
}) {
  if (typeof requestId !== "string" || !requestId.trim() ||
      requestId.length > 256 || typeof emit !== "function") {
    throw new TypeError("agent activity requires a bounded request identity and sink");
  }
  const origin = monotonicNow();
  let sequence = 0;
  let terminal = false;
  let wireAttempted = false;
  let acknowledged = false;
  let textDeltaSeen = false;
  const events = [];
  function write(stage, extra = {}) {
    if (!STAGES.has(stage) || terminal) {
      throw new Error("invalid or terminal agent activity stage");
    }
    const elapsed = monotonicNow() - origin;
    const record = Object.freeze({
      type: "agent_activity",
      request_id: requestId,
      source: "siwc_bridge_observation",
      sequence: ++sequence,
      stage,
      observed_at_utc: utcNow(),
      elapsed_ms: Math.max(0, Number.isFinite(elapsed)
        ? Math.round(elapsed * 1000) / 1000
        : 0),
      ...extra,
    });
    emit(record);
    events.push(record);
    if (TERMINAL.has(stage)) terminal = true;
    return record;
  }
  return Object.freeze({
    started() { return write("started"); },
    transport(stage) {
      if (stage === "wire_attempted") {
        if (wireAttempted) throw new Error("duplicate wire attempt");
        wireAttempted = true;
        return write(stage);
      }
      if (stage === "acknowledged") {
        if (!wireAttempted || acknowledged) throw new Error("invalid acknowledgment");
        acknowledged = true;
        return write(stage);
      }
      throw new Error("invalid transport progress");
    },
    delta(text) {
      if (typeof text !== "string") throw new TypeError("text delta must be a string");
      if (!textDeltaSeen) {
        textDeltaSeen = true;
        return write("first_text_delta", { delta_chars: text.length });
      }
      return write("text_delta", { delta_chars: text.length });
    },
    completed() {
      return write("completed", {
        delivery_evidence: textDeltaSeen
          ? "assistant_delta_seen"
          : acknowledged
            ? "http_acknowledged"
            : wireAttempted
              ? "wire_unconfirmed"
              : "not_dispatched",
      });
    },
    failed(error) {
      // Error bodies may contain private prompt text or credentials. Only
      // transport evidence is recorded; the existing error channel remains
      // responsible for sanitized error reporting.
      return write("failed", {
        delivery_evidence: textDeltaSeen
          ? "assistant_delta_seen"
          : acknowledged
            ? "http_acknowledged"
            : wireAttempted
              ? "wire_unconfirmed"
              : "not_dispatched",
        cancelled: error?.code === "aborted" || error?.name === "AbortError",
      });
    },
    snapshot() { return events.slice(); },
  });
}
