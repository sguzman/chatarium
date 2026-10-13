// Bounded retry decisions for explicitly read-only SIWC operations.
// Never use this helper for Responses POST inference or tool execution:
// an attempted write may have been accepted even without an HTTP response.
const TRANSIENT_READ_STATUSES = new Set([408, 429, 500, 502, 503, 504]);
const TRANSIENT_READ_CODES = new Set([
  "ECONNRESET", "ECONNREFUSED", "EAI_AGAIN", "ETIMEDOUT",
  "UND_ERR_CONNECT_TIMEOUT", "UND_ERR_HEADERS_TIMEOUT", "UND_ERR_SOCKET",
  "fetch_failed", "network_error", "rate_limit_exceeded",
]);

const RETRY_CATEGORIES = Object.freeze([
  "rate_limit", "http_transient", "network_transient",
]);
const POLICY_FIELDS = Object.freeze([
  "maxAttempts", "initialDelayMs", "maxDelayMs",
  "maxTotalDelayMs", "jitterRatio", "categories",
]);

export const DEFAULT_READ_RETRY_POLICY = Object.freeze({
  maxAttempts: 3,
  initialDelayMs: 300,
  maxDelayMs: 3_000,
  maxTotalDelayMs: 6_000,
  jitterRatio: 0.2,
  categories: Object.freeze({
    rate_limit: Object.freeze({ enabled: true, maxAttempts: 3 }),
    http_transient: Object.freeze({ enabled: true, maxAttempts: 3 }),
    network_transient: Object.freeze({ enabled: true, maxAttempts: 3 }),
  }),
});

// Policy customization is opt-in and limited to a small read-only budget;
// no configuration can authorize retrying inference, tools, or other writes.
export function parseReadRetryPolicy(input) {
  if (input === undefined || input === null || input === "") {
    return DEFAULT_READ_RETRY_POLICY;
  }
  if (typeof input === "string") {
    if (input.length > 2048) throw new TypeError("read retry policy too large");
    try {
      input = JSON.parse(input);
    } catch {
      throw new TypeError("invalid read retry policy JSON");
    }
  }
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("read retry policy must be a JSON object");
  }
  for (const key of Object.keys(input)) {
    if (!POLICY_FIELDS.includes(key)) {
      throw new TypeError("unknown read retry policy field");
    }
  }
  const policy = {
    ...DEFAULT_READ_RETRY_POLICY,
    ...input,
  };
  if (!Number.isInteger(policy.maxAttempts) || policy.maxAttempts < 1 ||
      policy.maxAttempts > 5 ||
      !Number.isSafeInteger(policy.initialDelayMs) ||
      policy.initialDelayMs < 0 || policy.initialDelayMs > 30_000 ||
      !Number.isSafeInteger(policy.maxDelayMs) ||
      policy.maxDelayMs < 0 || policy.maxDelayMs > 30_000 ||
      !Number.isSafeInteger(policy.maxTotalDelayMs) ||
      policy.maxTotalDelayMs < 0 || policy.maxTotalDelayMs > 90_000 ||
      typeof policy.jitterRatio !== "number" ||
      !Number.isFinite(policy.jitterRatio) ||
      policy.jitterRatio < 0 || policy.jitterRatio > 0.5) {
    throw new TypeError("read retry policy exceeds bounded limits");
  }
  const customCategories = input.categories ?? {};
  if (!customCategories || typeof customCategories !== "object" ||
      Array.isArray(customCategories)) {
    throw new TypeError("invalid read retry categories");
  }
  for (const category of Object.keys(customCategories)) {
    if (!RETRY_CATEGORIES.includes(category)) {
      throw new TypeError("unknown read retry category");
    }
  }
  const categories = {};
  for (const category of RETRY_CATEGORIES) {
    const value = customCategories[category] ?? {};
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new TypeError("invalid read retry category policy");
    }
    for (const key of Object.keys(value)) {
      if (key !== "enabled" && key !== "maxAttempts") {
        throw new TypeError("unknown read retry category field");
      }
    }
    const settings = { ...DEFAULT_READ_RETRY_POLICY.categories[category], ...value };
    if (typeof settings.enabled !== "boolean" ||
        !Number.isInteger(settings.maxAttempts) ||
        settings.maxAttempts < 1 || settings.maxAttempts > 5) {
      throw new TypeError("invalid read retry category limit");
    }
    categories[category] = Object.freeze(settings);
  }
  return Object.freeze({ ...policy, categories: Object.freeze(categories) });
}

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
  // Validate before the first network operation, not after a failure.
  policy = parseReadRetryPolicy(policy);
  let cumulativeDelay = 0;
  const failuresByCategory = Object.create(null);
  for (let attempt = 1; ; attempt++) {
    try {
      return await operation();
    } catch (error) {
      const failure = classifyReadFailure(error);
      if (!failure || attempt >= policy.maxAttempts) throw error;
      const categoryPolicy = policy.categories[failure.category];
      const categoryFailures = (failuresByCategory[failure.category] ?? 0) + 1;
      failuresByCategory[failure.category] = categoryFailures;
      if (!categoryPolicy.enabled ||
          categoryFailures >= categoryPolicy.maxAttempts) throw error;
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
