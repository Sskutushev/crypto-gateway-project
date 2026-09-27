import { createHmac, timingSafeEqual } from "node:crypto";

/** Default replay window, matching the gateway's merchant integration guide. */
export const DEFAULT_TOLERANCE_SECONDS = 300;

const SECRET_PATTERN = /^[0-9a-fA-F]{64}$/;
const SIGNATURE_PATTERN = /^[0-9a-f]{64}$/;
const TIMESTAMP_PATTERN = /^[0-9]{1,15}$/;

export interface ParsedSignature {
  timestamp: number;
  /** Every `v1=` value on the line, in order. */
  signatures: string[];
}

export type VerifyResult =
  | { ok: true; timestamp: number }
  | { ok: false; reason: VerifyFailure };

export type VerifyFailure =
  | "missing_header"
  | "malformed_header"
  | "no_secret_configured"
  | "timestamp_outside_tolerance"
  | "signature_mismatch";

export interface VerifyOptions {
  /**
   * Hex secrets this receiver trusts, newest first. During a secret rotation
   * keep the previous secret here until the operator confirms the switch.
   */
  secrets: readonly string[];
  toleranceSeconds?: number;
  /** Injected for tests; defaults to the system clock. */
  nowSeconds?: number;
}

/**
 * Parses `t=<unix>,v1=<hex>[,v1=<hex>...]`.
 *
 * Unknown keys are ignored so a future signature scheme can be added beside
 * `v1` without breaking this receiver. Exactly one `t` is required.
 */
export function parseSignatureHeader(header: string): ParsedSignature | null {
  let timestamp: number | null = null;
  const signatures: string[] = [];
  for (const part of header.split(",")) {
    const separator = part.indexOf("=");
    if (separator <= 0) {
      return null;
    }
    const key = part.slice(0, separator).trim();
    const value = part.slice(separator + 1).trim();
    if (key === "t") {
      if (timestamp !== null || !TIMESTAMP_PATTERN.test(value)) {
        return null;
      }
      timestamp = Number(value);
    } else if (key === "v1") {
      if (!SIGNATURE_PATTERN.test(value)) {
        return null;
      }
      signatures.push(value);
    }
  }
  if (timestamp === null || signatures.length === 0) {
    return null;
  }
  return { timestamp, signatures };
}

/**
 * HMAC-SHA256 over `<t>.<raw body>`, keyed by the hex-decoded secret.
 * The key is the 32 decoded bytes, not the 64-character hex text.
 */
export function computeSignature(secretHex: string, timestamp: number, rawBody: Buffer): Buffer {
  if (!SECRET_PATTERN.test(secretHex)) {
    throw new Error("a webhook secret must be 64 hex characters");
  }
  return createHmac("sha256", Buffer.from(secretHex, "hex"))
    .update(`${timestamp}.`, "utf8")
    .update(rawBody)
    .digest();
}

/** Verifies the header against the exact bytes received, before any parsing. */
export function verifySignature(
  header: string | undefined,
  rawBody: Buffer,
  options: VerifyOptions,
): VerifyResult {
  if (header === undefined || header.length === 0) {
    return { ok: false, reason: "missing_header" };
  }
  if (options.secrets.length === 0) {
    return { ok: false, reason: "no_secret_configured" };
  }
  const parsed = parseSignatureHeader(header);
  if (parsed === null) {
    return { ok: false, reason: "malformed_header" };
  }
  const tolerance = options.toleranceSeconds ?? DEFAULT_TOLERANCE_SECONDS;
  const now = options.nowSeconds ?? Math.floor(Date.now() / 1000);
  if (Math.abs(now - parsed.timestamp) > tolerance) {
    return { ok: false, reason: "timestamp_outside_tolerance" };
  }

  let matched = false;
  for (const secret of options.secrets) {
    const expected = computeSignature(secret, parsed.timestamp, rawBody);
    for (const candidate of parsed.signatures) {
      // Both buffers are 32 bytes: the header pattern admits only 64 hex digits.
      if (timingSafeEqual(expected, Buffer.from(candidate, "hex"))) {
        matched = true;
      }
    }
  }
  return matched
    ? { ok: true, timestamp: parsed.timestamp }
    : { ok: false, reason: "signature_mismatch" };
}
