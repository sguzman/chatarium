// Each patched Responses request must belong to exactly one turn.
// Pure, offline-testable concurrency gate independent of authentication.
export class ResponsesLane {
  #active = null;

  acquire(requestId, kind) {
    if (kind !== "stream" && kind !== "probe") {
      throw new TypeError("Responses lane requires stream or probe kind.");
    }
    if (typeof requestId !== "string" || !requestId.trim() || requestId.length > 256) {
      const error = new Error("Responses operation requires a nonempty bounded request_id.");
      error.code = "invalid_response_request_id";
      error.retryable = false;
      throw error;
    }
    if (this.#active !== null) {
      const probeBusy = kind === "probe" || this.#active.kind === "probe";
      const error = new Error(
        probeBusy
          ? "Capability probes require an idle Responses bridge."
          : "Another response is already streaming. Wait for completion or cancel it before sending another."
      );
      error.code = probeBusy ? "probe_busy" : "response_busy";
      error.retryable = true;
      throw error;
    }
    const owner = { requestId, kind };
    this.#active = owner;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      if (this.#active !== owner) {
        throw new Error("Responses lane ownership changed before release.");
      }
      this.#active = null;
    };
  }
}
