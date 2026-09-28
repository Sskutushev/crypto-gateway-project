export interface RetryOptions {
  /** Retries after the first attempt. Default 3; 0 disables retries. */
  maxRetries?: number;
  /** The first backoff ceiling in milliseconds. Default 500. */
  baseDelayMs?: number;
  /** The largest backoff ceiling in milliseconds. Default 8000. */
  maxDelayMs?: number;
  /**
   * The longest `Retry-After` the client will wait out, in milliseconds.
   * A longer one is not slept through: the error is thrown with its
   * `retryAfterSeconds` for the caller to schedule. Default 60000.
   */
  maxRetryAfterMs?: number;
}

export interface ResolvedRetryOptions {
  maxRetries: number;
  baseDelayMs: number;
  maxDelayMs: number;
  maxRetryAfterMs: number;
}

export const DEFAULT_RETRY: ResolvedRetryOptions = {
  maxRetries: 3,
  baseDelayMs: 500,
  maxDelayMs: 8000,
  maxRetryAfterMs: 60_000,
};

function nonNegativeInteger(name: string, value: number | undefined, fallback: number): number {
  if (value === undefined) {
    return fallback;
  }
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`retry.${name} must be a non-negative integer, got ${String(value)}`);
  }
  return value;
}

export function resolveRetryOptions(options: RetryOptions | false | undefined): ResolvedRetryOptions {
  if (options === false) {
    return { ...DEFAULT_RETRY, maxRetries: 0 };
  }
  return {
    maxRetries: nonNegativeInteger("maxRetries", options?.maxRetries, DEFAULT_RETRY.maxRetries),
    baseDelayMs: nonNegativeInteger("baseDelayMs", options?.baseDelayMs, DEFAULT_RETRY.baseDelayMs),
    maxDelayMs: nonNegativeInteger("maxDelayMs", options?.maxDelayMs, DEFAULT_RETRY.maxDelayMs),
    maxRetryAfterMs: nonNegativeInteger("maxRetryAfterMs", options?.maxRetryAfterMs, DEFAULT_RETRY.maxRetryAfterMs),
  };
}

/**
 * Whether a request may be sent more than once. Only a request the gateway
 * recognises as a repeat qualifies: a GET, or a write carrying its
 * idempotency key. Any other write could act twice.
 */
export function isIdempotentRequest(method: string, idempotencyKey: string | undefined): boolean {
  return method.toUpperCase() === "GET" || idempotencyKey !== undefined;
}

/** Statuses worth another attempt: the server may answer differently later. */
export function isRetryableStatus(status: number): boolean {
  return status === 408 || status === 429 || status >= 500;
}

/**
 * Capped exponential backoff with full jitter: a uniform delay between zero
 * and `min(maxDelayMs, baseDelayMs * 2^retry)`. Jitter spreads the retries of
 * many clients that failed at the same moment.
 */
export function backoffDelayMs(retry: number, options: ResolvedRetryOptions, random: () => number): number {
  const ceiling = Math.min(options.maxDelayMs, options.baseDelayMs * 2 ** retry);
  return Math.floor(random() * ceiling);
}

const DELTA_SECONDS = /^[0-9]+$/;
const IMF_FIXDATE = /^[A-Za-z]{3}, [0-9]{2} [A-Za-z]{3} [0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT$/;
const RFC_850_DATE = /^[A-Za-z]{6,9}, [0-9]{2}-[A-Za-z]{3}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT$/;
const ASCTIME_DATE = /^[A-Za-z]{3} [A-Za-z]{3} [ 0-9][0-9] [0-9]{2}:[0-9]{2}:[0-9]{2} [0-9]{4}$/;

/**
 * Parses a `Retry-After` value into whole seconds from `nowMs`.
 *
 * Both forms are read: delta-seconds (`"120"`) and an HTTP date. A date in the
 * past is zero. Anything else is `null`: an unreadable header is reported as
 * absent, never as "retry now".
 */
export function parseRetryAfter(value: string | null | undefined, nowMs: number = Date.now()): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    return null;
  }
  if (DELTA_SECONDS.test(trimmed)) {
    const seconds = Number(trimmed);
    return Number.isSafeInteger(seconds) ? seconds : null;
  }
  // Date.parse accepts far more than HTTP dates ("Sep 2026" is a date to it),
  // and a loose match would turn a garbled header into "retry now". Only the
  // three forms RFC 9110 names are read, all in GMT.
  let normalised: string;
  if (IMF_FIXDATE.test(trimmed) || RFC_850_DATE.test(trimmed)) {
    normalised = trimmed;
  } else if (ASCTIME_DATE.test(trimmed)) {
    normalised = `${trimmed} GMT`;
  } else {
    return null;
  }
  const at = Date.parse(normalised);
  if (Number.isNaN(at)) {
    return null;
  }
  return Math.max(0, Math.ceil((at - nowMs) / 1000));
}
