import assert from "node:assert/strict";
import test from "node:test";
import {
  classifyReadFailure,
  parseReadRetryPolicy,
  retryReadOnly,
} from "./retry-policy.mjs";

const httpError = (status, headers) =>
  Object.assign(new Error("synthetic HTTP response"), { status, headers });

test("read-only 429 and 503 retries are bounded, ordered, and observable", async () => {
  const status = [429, 503, "ok"];
  let attempts = 0;
  const delays = [];
  const decisions = [];
  const result = await retryReadOnly(async () => {
    const next = status[attempts++];
    if (next !== "ok") throw httpError(next);
    return "models";
  }, {
    random: () => 0.5,
    sleep: async (ms) => { delays.push(ms); },
    onRetry: (decision) => decisions.push(decision),
  });
  assert.equal(result, "models");
  assert.equal(attempts, 3);
  assert.deepEqual(delays, [300, 600]);
  assert.deepEqual(decisions, [
    { category: "rate_limit", status: 429, attempt: 1, next_attempt: 2, delay_ms: 300 },
    { category: "http_transient", status: 503, attempt: 2, next_attempt: 3, delay_ms: 600 },
  ]);
});

test("429 honors Retry-After seconds and HTTP-date without retrying early", async () => {
  const delays = [];
  let calls = 0;
  const serverError = httpError(429, new Headers({ "retry-after": "2" }));
  const result = await retryReadOnly(async () => {
    if (++calls === 1) throw serverError;
    return true;
  }, { sleep: async (ms) => { delays.push(ms); } });
  assert.equal(result, true);
  assert.deepEqual(delays, [2000]);
  await assert.rejects(retryReadOnly(async () => {
    calls++;
    throw httpError(429, new Headers({ "retry-after": "120" }));
  }, { sleep: () => { throw new Error("must not sleep"); } }), (error) => error.status === 429);
  assert.equal(calls, 3);

  const dateHeader = new Date(1_700_000_002_000).toUTCString();
  const dateDelays = [];
  await retryReadOnly((() => {
    let attempt = 0;
    return async () => {
      if (attempt++ === 0) throw httpError(429, new Headers({ "retry-after": dateHeader }));
      return "ok";
    };
  })(), {
    now: () => 1_700_000_000_000,
    sleep: async (ms) => { dateDelays.push(ms); },
  });
  assert.deepEqual(dateDelays, [2000]);
});

test("auth, invalid requests, cancellation and arbitrary error strings are terminal", async () => {
  for (const error of [
    Object.assign(httpError(401), { retryable: true, cause: { code: "ECONNRESET" } }),
    httpError(403),
    httpError(400),
    Object.assign(new Error("timeout"), { code: "aborted" }),
    new Error("ECONNRESET in unrelated text"),
  ]) {
    let calls = 0;
    await assert.rejects(retryReadOnly(async () => {
      calls++;
      throw error;
    }, { sleep: () => { throw new Error("unreachable"); } }), (actual) => actual === error);
    assert.equal(calls, 1);
  }
});

test("transient read network failure eventually stops after exactly three calls", async () => {
  const error = Object.assign(new Error("offline network interruption"), {
    cause: { code: "UND_ERR_CONNECT_TIMEOUT" },
  });
  let calls = 0;
  await assert.rejects(retryReadOnly(async () => {
    calls++;
    throw error;
  }, {
    random: () => 0.5,
    sleep: async () => {},
  }), (actual) => actual === error);
  assert.equal(calls, 3);
  assert.deepEqual(classifyReadFailure(error), { category: "network_transient", status: null });
});

test("cooldown budget, attempt budget and invalid settings fail closed", async () => {
  const statusError = httpError(429, new Headers({ "retry-after": "4" }));
  let calls = 0;
  await assert.rejects(retryReadOnly(async () => {
    calls++;
    throw statusError;
  }, { sleep: () => { throw new Error("no premature retry"); } }), (error) => error === statusError);
  assert.equal(calls, 1);

  const invalid = { maxAttempts: 0, initialDelayMs: 0,
    maxDelayMs: 0, maxTotalDelayMs: 0, jitterRatio: 0 };
  await assert.rejects(retryReadOnly(async () => {
    throw new Error("should not execute");
  }, { policy: invalid }), TypeError);

  await assert.rejects(retryReadOnly(null), TypeError);
});

test("opt-in granular retry controls refuse disabled classes without any resend", async () => {
  const policy = parseReadRetryPolicy(JSON.stringify({
    maxAttempts: 5,
    categories: {
      rate_limit: { enabled: false },
      http_transient: { maxAttempts: 2 },
      network_transient: { maxAttempts: 1 },
    },
  }));
  assert.equal(policy.maxAttempts, 5);
  assert.equal(policy.categories.rate_limit.enabled, false);
  assert.equal(policy.categories.http_transient.maxAttempts, 2);
  for (const failure of [
    httpError(429),
    Object.assign(new Error("reset"), { code: "ECONNRESET" }),
  ]) {
    let calls = 0;
    await assert.rejects(retryReadOnly(async () => {
      calls++;
      throw failure;
    }, {
      policy,
      sleep: () => { throw new Error("no retry allowed"); },
    }), (error) => error === failure);
    assert.equal(calls, 1);
  }
  let serverCalls = 0;
  const delays = [];
  await assert.rejects(retryReadOnly(async () => {
    serverCalls++;
    throw httpError(503);
  }, {
    policy,
    random: () => 0.5,
    sleep: async (ms) => { delays.push(ms); },
  }), (error) => error.status === 503);
  assert.equal(serverCalls, 2);
  assert.deepEqual(delays, [300]);
});

test("retry configuration requires bounded known settings and rejects extra authority", () => {
  assert.equal(parseReadRetryPolicy().maxAttempts, 3);
  assert.equal(parseReadRetryPolicy("").maxAttempts, 3);
  for (const invalid of [
    "not json", "[]", "null", "\\",
    JSON.stringify({ maxAttempts: 100 }),
    JSON.stringify({ jitterRatio: 1 }),
    JSON.stringify({ maxTotalDelayMs: Infinity }),
    JSON.stringify({ initialDelayMs: -1 }),
    JSON.stringify({ maxDelayMs: 30_001 }),
    JSON.stringify({ categories: { rate_limit: { enabled: "yes" } } }),
    JSON.stringify({ categories: { rate_limit: { maxAttempts: 0 } } }),
    JSON.stringify({ categories: { unsupported: { enabled: true } } }),
    JSON.stringify({ enableInferenceResends: true }),
    " ".repeat(2049),
  ]) {
    assert.throws(() => parseReadRetryPolicy(invalid), TypeError);
  }
  assert.throws(() => parseReadRetryPolicy({ categories: [] }), TypeError);
  assert.throws(() => parseReadRetryPolicy({ categories: {
    rate_limit: { enabled: true, arbitrary: "field" },
  } }), TypeError);
});

test("per-failure controls do not enable inference retries or exceed total attempts", async () => {
  const policy = parseReadRetryPolicy({
    maxAttempts: 2,
    categories: { rate_limit: { maxAttempts: 5 } },
  });
  let attempts = 0;
  await assert.rejects(retryReadOnly(async () => {
    attempts++;
    throw httpError(429);
  }, { policy, sleep: async () => {} }), (error) => error.status === 429);
  assert.equal(attempts, 2);
});
