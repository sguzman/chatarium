// Bounded retry decisions for explicitly read-only SIWC operations.
// Never use this helper for Responses POST inference or tool execution:
// an attempted write may have been accepted even without an HTTP response.
const TRANSIENT_READ_STATUSES = new Set([408, 429, 500, 502, 503, 504]);
const TRANSIENT_READ_CODES = new Set([
  "ECONNRESET", "ECONNREFUSED", "EAI_AGAIN", "ETIMEDOUT",
  "UND_ERR_CONNECT_TIMEOUT", "UND_ERR_HEADERS_TIMEOUT", "UND_ERR_SOCKET",
  "fetch_failed", "network_error", "rate_limit_exceeded",
]);

export const DEFAULT_READ_RETRY_POLICY = Object.freeze({
  maxAttempts: 3,
  initialDelayMs: 300,
  maxDelayMs: 3_000,
  maxTotalDelayMs: 6_000,
  jitterRatio: 0.2,
});

export function classifyReadFailure(error) {
  const status = Number.isInteger(error?.status)
    ? error.status
    : Number.isInteger(error?.response?.status)
      ? error.response.status
      : null;
  // An explicit nontransient HTTP response overrides misleading retryable
  // flags or nested transport codes. Auth/configuration failures stay terminal.
  if (status !== null) {
    if (!TRANSIENT_READ_STATUSES.has(status)) return null;
    return { category: status === 429 ? "rate_limit" : "http_transient", status };
  }
  const code = typeof error?.code === "string"
    ? error.code
    : error?.cause?.code;
  if (TRANSIENT_READ_CODES.has(code)) {
    return { category: "network_transient", status: null };
  }
  return null;
}

function retryAfterMs(error, now) {
  let header;
  const headers = error?.headers ?? error?.response?.headers;
  if (headers && typeof headers.get === "function") {
    header = headers.get("retry-after");
  }
  const direct = error?.retryAfterMs;
  if (Number.isFinite(direct) && direct >= 0) return direct;
  if (typeof header !== "string" || !header.trim()) return null;
  const value = header.trim();
  if (/^\d+(?:\.\d+)?$/.test(value)) {
    const seconds = Number(value);
    return Number.isFinite(seconds) ? seconds * 1000 : null;
  }
  const deadline = Date.parse(value);
  return Number.isFinite(deadline) ? Math.max(0, deadline - now()) : null;
}

function delayForFailure(error, previousFailures, policy, random, now) {
  const serverDelay = retryAfterMs(error, now);
  if (serverDelay !== null) {
    // Never retry sooner than a server-requested cooldown just because our
    // short bounded retry budget is smaller.
    return serverDelay;
  }
  const raw = Math.min(
    policy.maxDelayMs,
    policy.initialDelayMs * 2 ** Math.min(previousFailures - 1, 20),
  );
  const sample = Math.min(1, Math.max(0, random()));
  return Math.round(raw * (1 + (sample * 2 - 1) * policy.jitterRatio));
}

export async function retryReadOnly(operation, {
  policy = DEFAULT_READ_RETRY_POLICY,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  random = Math.random,
  now = Date.now,
  onRetry = () => {},
} = {}) {
  if (typeof operation !== "function") throw new TypeError("operation must be a function");
  if (!Number.isInteger(policy.maxAttempts) || policy.maxAttempts < 1 ||
      !Number.isFinite(policy.initialDelayMs) || policy.initialDelayMs < 0 ||
      !Number.isFinite(policy.maxDelayMs) || policy.maxDelayMs < 0 ||
      !Number.isFinite(policy.maxTotalDelayMs) || policy.maxTotalDelayMs < 0 ||
      !Number.isFinite(policy.jitterRatio) ||
      policy.jitterRatio < 0 || policy.jitterRatio > 1) {
    throw new TypeError("invalid read-only retry policy");
  }
  let cumulativeDelay = 0;
  for (let attempt = 1; ; attempt++) {
    try {
      return await operation();
    } catch (error) {
      const failure = classifyReadFailure(error);
      if (!failure || attempt >= policy.maxAttempts) throw error;
      const delayMs = delayForFailure(error, attempt, policy, random, now);
      if (!Number.isFinite(delayMs) || delayMs < 0 ||
          delayMs > policy.maxDelayMs ||
          cumulativeDelay + delayMs > policy.maxTotalDelayMs) {
        throw error;
      }
      cumulativeDelay += delayMs;
      // Metadata only: never emit the original error object or request body.
      onRetry({
        category: failure.category,
        status: failure.status,
        attempt,
        next_attempt: attempt + 1,
        delay_ms: delayMs,
      });
      await sleep(delayMs);
    }
  }
}
