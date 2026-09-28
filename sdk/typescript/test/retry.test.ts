import assert from "node:assert/strict";
import { describe, test } from "node:test";

import { DEFAULT_RETRY, backoffDelayMs, isIdempotentRequest, isRetryableStatus, parseRetryAfter } from "../src/retry.js";

describe("the retry policy", () => {
  test("only a GET or a write with an idempotency key may be repeated", () => {
    assert.equal(isIdempotentRequest("GET", undefined), true);
    assert.equal(isIdempotentRequest("get", undefined), true);
    assert.equal(isIdempotentRequest("POST", "order-1042-attempt-1"), true);
    assert.equal(isIdempotentRequest("POST", undefined), false);
    assert.equal(isIdempotentRequest("PUT", undefined), false);
  });

  test("retryable statuses are 408, 429 and 5xx", () => {
    for (const status of [408, 429, 500, 502, 503, 504]) {
      assert.equal(isRetryableStatus(status), true, String(status));
    }
    for (const status of [400, 401, 403, 404, 409, 422]) {
      assert.equal(isRetryableStatus(status), false, String(status));
    }
  });

  test("backoff is full jitter under a doubling, capped ceiling", () => {
    const options = { ...DEFAULT_RETRY, baseDelayMs: 100, maxDelayMs: 1000 };
    assert.equal(backoffDelayMs(0, options, () => 0), 0);
    assert.equal(backoffDelayMs(0, options, () => 0.5), 50);
    assert.equal(backoffDelayMs(3, options, () => 0.5), 400);
    assert.equal(backoffDelayMs(10, options, () => 0.5), 500);
    assert.equal(backoffDelayMs(60, options, () => 0.9999), 999);
  });
});

describe("parseRetryAfter", () => {
  const now = Date.parse("2026-09-28T10:00:00Z");

  test("delta-seconds", () => {
    assert.equal(parseRetryAfter("120", now), 120);
    assert.equal(parseRetryAfter("0", now), 0);
    assert.equal(parseRetryAfter(" 3 ", now), 3);
  });

  test("an HTTP date, rounded up, and a past date as zero", () => {
    assert.equal(parseRetryAfter("Mon, 28 Sep 2026 10:00:30 GMT", now), 30);
    assert.equal(parseRetryAfter("Mon, 28 Sep 2026 09:00:00 GMT", now), 0);
    assert.equal(parseRetryAfter("Monday, 28-Sep-26 10:01:00 GMT", now), 60);
    assert.equal(parseRetryAfter("Mon Sep 28 10:00:05 2026", now), 5);
  });

  test("absent or unreadable is null, never zero", () => {
    for (const value of [null, undefined, "", "  ", "-1", "1.5", "soon", "Sep 2026", "2026-09-28T10:00:30Z", "Mon, 28 Sep 2026 10:00:30", "99999999999999999999"]) {
      assert.equal(parseRetryAfter(value, now), null, String(value));
    }
  });
});
