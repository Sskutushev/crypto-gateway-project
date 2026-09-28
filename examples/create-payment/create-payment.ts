// Create a payment intent, quote it, show the payer what to send, and poll.
//
// Run on Node 22.6+:  node --experimental-strip-types create-payment.ts
// Environment: GATEWAY_URL, MERCHANT_KEY, ASSET_ID, ASSET_DECIMALS,
//              ORDER_REF, AMOUNT_MINOR, CURRENCY

import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";

interface PaymentIntent {
  id: string;
  status: "requires_quote" | "awaiting_payment" | "partially_paid" | "risk_hold" | "paid" | "expired" | "cancelled";
  reference: string;
  amount: { currency: string; minor_units: string };
}

interface IssuedQuote {
  id: string;
  payment_intent_id: string;
  collector_address: string;
  amount_raw: string;
  expires_at: string;
  late_payment_until: string;
}

interface ApiError {
  error: { code: string; message: string };
}

class GatewayError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(`${status} ${code}: ${message}`);
    this.status = status;
    this.code = code;
  }
}

/**
 * Converts an integer amount of the token's smallest unit into a decimal
 * string without floating point. "49990000" with 6 decimals is "49.99".
 * Trailing zeros of the fraction are dropped; nothing is rounded.
 */
export function formatUnits(amountRaw: string, decimals: number): string {
  if (!/^[0-9]+$/.test(amountRaw)) {
    throw new Error(`amount_raw must be an integer string, got ${JSON.stringify(amountRaw)}`);
  }
  if (!Number.isInteger(decimals) || decimals < 0 || decimals > 77) {
    throw new Error(`decimals must be an integer between 0 and 77, got ${decimals}`);
  }
  const digits = amountRaw.replace(/^0+(?=\d)/, "");
  if (decimals === 0) {
    return digits;
  }
  const padded = digits.padStart(decimals + 1, "0");
  const whole = padded.slice(0, padded.length - decimals);
  const fraction = padded.slice(padded.length - decimals).replace(/0+$/, "");
  return fraction.length === 0 ? whole : `${whole}.${fraction}`;
}

/**
 * An Idempotency-Key derived from something stable, so a retry after a crash
 * reuses it. 16 to 128 characters of [A-Za-z0-9_-].
 */
export function idempotencyKey(purpose: string, stableId: string): string {
  const digest = createHash("sha256").update(stableId, "utf8").digest("hex").slice(0, 40);
  return `${purpose}-${digest}`;
}

function required(name: string): string {
  const value = process.env[name];
  if (value === undefined || value.length === 0) {
    throw new Error(`${name} is required`);
  }
  return value;
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * Sends one request, retrying only what is safe to retry: a network failure,
 * 429 and 5xx. A write is retried with the same Idempotency-Key and the same
 * body, so the gateway returns the first result instead of acting twice.
 */
async function call<T>(
  baseUrl: string,
  apiKey: string,
  method: "GET" | "POST",
  path: string,
  options: { body?: unknown; idempotencyKey?: string; attempts?: number } = {},
): Promise<{ status: number; replayed: boolean; data: T }> {
  const attempts = options.attempts ?? 5;
  const payload = options.body === undefined ? undefined : JSON.stringify(options.body);
  let lastError: unknown = null;

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const headers: Record<string, string> = { authorization: `Bearer ${apiKey}` };
    if (payload !== undefined) {
      headers["content-type"] = "application/json";
    }
    if (options.idempotencyKey !== undefined) {
      headers["idempotency-key"] = options.idempotencyKey;
    }
    let response: Response;
    try {
      response = await fetch(`${baseUrl}${path}`, { method, headers, body: payload });
    } catch (error) {
      lastError = error;
      await sleep(1000 * 2 ** (attempt - 1));
      continue;
    }
    const text = await response.text();
    if (response.ok) {
      return {
        status: response.status,
        replayed: response.headers.get("idempotent-replayed") === "true",
        data: JSON.parse(text) as T,
      };
    }
    let code = "unknown";
    let message = text;
    try {
      const parsed = JSON.parse(text) as ApiError;
      code = parsed.error.code;
      message = parsed.error.message;
    } catch {
      // Not the gateway's error envelope (for example a proxy page); the raw text is kept in message.
    }
    lastError = new GatewayError(response.status, code, message);
    if (response.status === 429 || response.status >= 500) {
      await sleep(1000 * 2 ** (attempt - 1));
      continue;
    }
    // 4xx is a decision, not an outage. 409 idempotency_conflict means the
    // same key was sent with a different body: a bug in the caller.
    throw lastError;
  }
  throw lastError;
}

async function main(): Promise<void> {
  const baseUrl = required("GATEWAY_URL").replace(/\/+$/, "");
  const merchantKey = required("MERCHANT_KEY");
  const assetId = required("ASSET_ID");
  // The quote does not carry the token's decimals. USDT TRC20 has 6; take the
  // value from the operator's asset configuration, never from a guess.
  const decimals = Number(required("ASSET_DECIMALS"));
  const orderRef = required("ORDER_REF");
  const amountMinor = required("AMOUNT_MINOR");
  const currency = required("CURRENCY");

  // 1. The obligation. The reference is unique per merchant; the key is
  //    derived from it, so re-running this script for the same order returns
  //    the same intent (200, Idempotent-Replayed: true) instead of a conflict.
  const created = await call<PaymentIntent>(baseUrl, merchantKey, "POST", "/v1/payment-intents", {
    idempotencyKey: idempotencyKey("intent", orderRef),
    body: { amount_minor: amountMinor, currency, reference: orderRef },
  });
  const intent = created.data;
  console.log(`intent ${intent.id} status=${intent.status} replayed=${created.replayed}`);

  // 2. The quote. There is no route to read a quote back, so the key is
  //    derived from the intent: replaying it is how the quote is fetched again.
  const quoted = await call<IssuedQuote>(baseUrl, merchantKey, "POST", `/v1/payment-intents/${intent.id}/quotes`, {
    idempotencyKey: idempotencyKey("quote", intent.id),
    body: { asset_id: assetId },
  });
  const quote = quoted.data;

  // 3. What the payer sees: the exact amount, not rounded, and the address.
  console.log("Send exactly");
  console.log(`  ${formatUnits(quote.amount_raw, decimals)} (amount_raw ${quote.amount_raw})`);
  console.log(`  to ${quote.collector_address}`);
  console.log(`  before ${quote.expires_at}`);
  console.log("Any other amount is not matched automatically.");

  // 4. Poll. The signed webhook is the primary signal; polling is a fallback.
  const lateUntil = Date.parse(quote.late_payment_until);
  for (;;) {
    const current = await call<PaymentIntent>(baseUrl, merchantKey, "GET", `/v1/payment-intents/${intent.id}`);
    const status = current.data.status;
    console.log(`status=${status}`);
    if (status === "paid") {
      console.log("paid: fulfil the order (once, keyed by the intent id)");
      return;
    }
    if (status === "cancelled") {
      return;
    }
    // Expired but inside late_payment_until: a payment of the exact amount is
    // put in front of an operator, who may still honour it. Keep polling.
    if (status === "expired" && Date.now() > lateUntil) {
      // After late_payment_until nothing more is matched to this quote. Money
      // that still arrives is queued for the operator, not credited here.
      console.log("expired: create a new intent with a new reference to try again");
      return;
    }
    await sleep(10_000);
  }
}

const invokedDirectly = process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invokedDirectly) {
  main().catch((error: unknown) => {
    console.error(String(error));
    process.exitCode = 1;
  });
}
