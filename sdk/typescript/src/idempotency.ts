import { randomUUID } from "node:crypto";

/** 16 to 128 URL-safe characters, as the gateway validates them. */
const KEY_PATTERN = /^[A-Za-z0-9_-]{16,128}$/;
const PREFIX_PATTERN = /^[A-Za-z0-9_-]{1,91}$/;

/** Whether `key` is an idempotency key the gateway accepts. */
export function isValidIdempotencyKey(key: unknown): key is string {
  return typeof key === "string" && KEY_PATTERN.test(key);
}

/** Throws a `TypeError` unless `key` is an idempotency key the gateway accepts. */
export function assertIdempotencyKey(key: unknown): asserts key is string {
  if (!isValidIdempotencyKey(key)) {
    throw new TypeError(
      "an idempotency key must be 16 to 128 characters of A-Z a-z 0-9 _ -, got " +
        (typeof key === "string" ? `${key.length} characters` : typeof key),
    );
  }
}

/**
 * A fresh random idempotency key.
 *
 * Generate it once per logical operation and store it with your order before
 * the first attempt: a retry after a crash must reuse the same key, or it is a
 * second request rather than a retry.
 */
export function newIdempotencyKey(prefix?: string): string {
  if (prefix !== undefined && !PREFIX_PATTERN.test(prefix)) {
    throw new TypeError("an idempotency key prefix must be 1 to 91 characters of A-Z a-z 0-9 _ -");
  }
  const id = randomUUID();
  return prefix === undefined ? id : `${prefix}-${id}`;
}
