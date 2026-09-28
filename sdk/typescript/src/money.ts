/**
 * Exact conversions between integer unit strings and decimal display strings.
 *
 * Nothing here touches a JavaScript number: USDT has 6 decimals, other tokens
 * 18, and a raw amount passes 2^53 long before it is large in value.
 */

const INTEGER = /^[0-9]+$/;
const DECIMAL = /^([0-9]+)(?:\.([0-9]+))?$/;

/** The gateway accepts asset decimals from 0 to 77, the width of a uint256. */
export const MAX_DECIMALS = 77;

/** Whether `value` is a non-negative integer written as decimal digits only. */
export function isIntegerString(value: unknown): value is string {
  return typeof value === "string" && INTEGER.test(value);
}

function assertDecimals(decimals: number): void {
  if (!Number.isInteger(decimals) || decimals < 0 || decimals > MAX_DECIMALS) {
    throw new RangeError(`decimals must be an integer from 0 to ${MAX_DECIMALS}, got ${String(decimals)}`);
  }
}

/**
 * Formats an amount in the token's smallest unit as whole tokens, exactly.
 *
 * `formatTokenAmount("4999000", 6)` is `"4.999"`. Trailing zeros of the
 * fraction are dropped and nothing is rounded, so this matches the gateway's
 * own `amount` field. Throws on anything but an integer string.
 */
export function formatTokenAmount(raw: string, decimals: number): string {
  if (!isIntegerString(raw)) {
    throw new TypeError(`an amount must be an integer string, got ${JSON.stringify(raw)}`);
  }
  assertDecimals(decimals);
  const value = BigInt(raw);
  if (decimals === 0) {
    return value.toString();
  }
  const scale = 10n ** BigInt(decimals);
  const whole = value / scale;
  const fraction = (value % scale).toString().padStart(decimals, "0").replace(/0+$/, "");
  return fraction.length === 0 ? whole.toString() : `${whole.toString()}.${fraction}`;
}

/**
 * Parses a decimal amount into integer minor units, exactly.
 *
 * `parseMinorUnits("49.99", 2)` is `"4999"`; `parseMinorUnits("4.999", 6)` is
 * `"4999000"`. More fractional digits than `decimals` allow is an error, never
 * a rounding. Signs, exponents, separators and surrounding space are refused.
 */
export function parseMinorUnits(amount: string, decimals: number): string {
  assertDecimals(decimals);
  const match = typeof amount === "string" ? DECIMAL.exec(amount) : null;
  if (match === null) {
    throw new TypeError(`an amount must be a plain decimal like "49.99", got ${JSON.stringify(amount)}`);
  }
  const whole = match[1] ?? "";
  const fraction = match[2] ?? "";
  if (fraction.length > decimals) {
    throw new RangeError(
      `${JSON.stringify(amount)} has ${fraction.length} fractional digits; this unit allows ${decimals}`,
    );
  }
  const units = BigInt(whole) * 10n ** BigInt(decimals) + BigInt(fraction.padEnd(decimals, "0") || "0");
  return units.toString();
}
