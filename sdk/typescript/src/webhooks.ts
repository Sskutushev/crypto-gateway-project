import { createHmac, timingSafeEqual } from "node:crypto";

import { GatewaySdkError } from "./errors.js";

/** The replay window the gateway's integration guide asks for: five minutes. */
export const DEFAULT_TOLERANCE_SECONDS = 300;

/** The header the gateway signs every delivery with. */
export const SIGNATURE_HEADER = "gateway-signature";
/** The header carrying the event id, equal to the body's `id`. */
export const EVENT_ID_HEADER = "gateway-event-id";

const SECRET_PATTERN = /^[0-9a-fA-F]{64}$/;
const SIGNATURE_PATTERN = /^[0-9a-fA-F]{64}$/;
const TIMESTAMP_PATTERN = /^[0-9]{1,15}$/;

interface Envelope<Type extends string, ObjectKind extends string, Attributes> {
  /** Stable across delivery retries: deduplicate by it. */
  id: string;
  type: Type;
  /** Unix seconds. */
  created_at: number;
  data: {
    object: ObjectKind;
    id: string;
    attributes: Attributes;
  };
}

/** The intent is settled. Fulfil the order on this event only. */
export type PaymentIntentPaidEvent = Envelope<
  "payment_intent.paid",
  "payment_intent",
  { payment_intent_id: string; transfer_id: string; attempt_id: string }
>;

/** An operator honoured an underpayment; the remainder is still owed. Never fulfil on it. */
export type PaymentIntentPartiallyPaidEvent = Envelope<
  "payment_intent.partially_paid",
  "payment_intent",
  { payment_intent_id: string; transfer_id: string }
>;

/** The merchant cancelled an order that had no money on it. */
export type PaymentIntentCancelledEvent = Envelope<
  "payment_intent.cancelled",
  "payment_intent",
  { payment_intent_id: string; reason: string | null }
>;

/**
 * A payment exceeded the amount. Written together with `payment_intent.paid`;
 * `remainder_raw` is the excess in the token's smallest unit.
 */
export type OverpaidEvent = Envelope<
  "OVERPAID",
  "payment_intent",
  { payment_intent_id: string; transfer_id: string; remainder_raw: string }
>;

/** Sent by the operator to check delivery and verification. Not a payment. */
export type WebhookTestEvent = Envelope<"webhook.test", "webhook_endpoint", { endpoint_id: string; note: string }>;

/** Every event the gateway delivers to a merchant endpoint. */
export type WebhookEvent =
  | PaymentIntentPaidEvent
  | PaymentIntentPartiallyPaidEvent
  | PaymentIntentCancelledEvent
  | OverpaidEvent
  | WebhookTestEvent;

export type WebhookEventType = WebhookEvent["type"];

/** The fields each event's attributes must carry, and whether null is allowed. */
const EVENT_SHAPES: Record<WebhookEventType, { object: string; attributes: Record<string, "string" | "string|null"> }> =
  {
    "payment_intent.paid": {
      object: "payment_intent",
      attributes: { payment_intent_id: "string", transfer_id: "string", attempt_id: "string" },
    },
    "payment_intent.partially_paid": {
      object: "payment_intent",
      attributes: { payment_intent_id: "string", transfer_id: "string" },
    },
    "payment_intent.cancelled": {
      object: "payment_intent",
      attributes: { payment_intent_id: "string", reason: "string|null" },
    },
    OVERPAID: {
      object: "payment_intent",
      attributes: { payment_intent_id: "string", transfer_id: "string", remainder_raw: "string" },
    },
    "webhook.test": {
      object: "webhook_endpoint",
      attributes: { endpoint_id: "string", note: "string" },
    },
  };

export const WEBHOOK_EVENT_TYPES = Object.keys(EVENT_SHAPES) as WebhookEventType[];

export type WebhookVerificationFailure =
  | "missing_header"
  | "malformed_header"
  | "timestamp_outside_tolerance"
  | "signature_mismatch"
  | "invalid_payload";

/**
 * The delivery must be refused: answer it with a 4xx and do not act on it.
 * `reason` says why, for logs.
 */
export class WebhookVerificationError extends GatewaySdkError {
  readonly reason: WebhookVerificationFailure;

  constructor(reason: WebhookVerificationFailure, detail: string) {
    super(`webhook verification failed (${reason}): ${detail}`);
    this.reason = reason;
  }
}

export interface UnknownWebhookEnvelope {
  id: string;
  type: string;
  created_at: number;
  data: Record<string, unknown>;
}

/**
 * The signature verified, but the event type is one this SDK version does not
 * know. The delivery is genuine; `envelope` is its parsed body. Answer 2xx and
 * log it, or upgrade the SDK: refusing it would only make the gateway retry.
 */
export class UnknownWebhookEventError extends GatewaySdkError {
  readonly envelope: UnknownWebhookEnvelope;

  constructor(envelope: UnknownWebhookEnvelope) {
    super(`webhook event type ${JSON.stringify(envelope.type)} is not known to this SDK version`);
    this.envelope = envelope;
  }
}

export interface ParsedSignatureHeader {
  timestamp: number;
  /** Every `v1` value, in order. During a secret rotation there are two. */
  signatures: string[];
}

/**
 * Parses `t=<unix>,v1=<hex>[,v1=<hex>...]`, or returns `null`.
 *
 * Unknown keys are ignored so a future scheme can be added beside `v1`
 * without breaking receivers. Exactly one `t` and at least one `v1` are required.
 */
export function parseSignatureHeader(header: string): ParsedSignatureHeader | null {
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
      signatures.push(value.toLowerCase());
    }
  }
  if (timestamp === null || signatures.length === 0) {
    return null;
  }
  return { timestamp, signatures };
}

function assertSecret(secret: string): void {
  if (typeof secret !== "string" || !SECRET_PATTERN.test(secret)) {
    throw new TypeError("a webhook signing secret must be the 64 hex characters the operator handed over");
  }
}

function toBytes(payload: string | Uint8Array): Buffer {
  return typeof payload === "string"
    ? Buffer.from(payload, "utf8")
    : Buffer.from(payload.buffer, payload.byteOffset, payload.byteLength);
}

/**
 * `HMAC-SHA256(hex_decode(secret), "<t>." + body)`, as lowercase hex. The key
 * is the 32 decoded bytes, not the 64-character text.
 */
export function computeWebhookSignature(secret: string, timestamp: number, payload: string | Uint8Array): string {
  assertSecret(secret);
  return createHmac("sha256", Buffer.from(secret, "hex"))
    .update(`${timestamp}.`, "utf8")
    .update(toBytes(payload))
    .digest("hex");
}

/**
 * Builds a `Gateway-Signature` value the way the gateway does, one `v1` per
 * secret in order. For tests of your own receiver; production signatures only
 * ever come from the gateway.
 */
export function signWebhookPayload(options: {
  payload: string | Uint8Array;
  secrets: readonly string[];
  timestamp: number;
}): string {
  if (!Number.isSafeInteger(options.timestamp) || options.timestamp < 0) {
    throw new RangeError("the timestamp must be non-negative unix seconds");
  }
  if (options.secrets.length === 0) {
    throw new TypeError("at least one secret is required");
  }
  const parts = [`t=${options.timestamp}`];
  for (const secret of options.secrets) {
    parts.push(`v1=${computeWebhookSignature(secret, options.timestamp, options.payload)}`);
  }
  return parts.join(",");
}

export interface VerifyWebhookOptions {
  /** The exact bytes received, before any JSON parsing or re-serialisation. */
  payload: string | Uint8Array;
  /** The `Gateway-Signature` header value. */
  header: string | readonly string[] | null | undefined;
  /**
   * Every signing secret this receiver currently trusts. During a rotation,
   * list the new and the previous secret; remove the old one after the
   * transition period.
   */
  secrets: readonly string[];
  /** Allowed clock distance in seconds, either direction. Default 300. */
  toleranceSeconds?: number;
  /** The receiver's clock; injected for tests. */
  now?: Date;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseEnvelope(bytes: Buffer): WebhookEvent {
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (error) {
    throw new WebhookVerificationError("invalid_payload", `the body is not UTF-8: ${String(error)}`);
  }
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch (error) {
    throw new WebhookVerificationError("invalid_payload", `the body is not JSON: ${String(error)}`);
  }
  if (!isRecord(body)) {
    throw new WebhookVerificationError("invalid_payload", "the body is not a JSON object");
  }
  const { id, type, created_at: createdAt, data } = body;
  if (
    typeof id !== "string" ||
    typeof type !== "string" ||
    typeof createdAt !== "number" ||
    !Number.isInteger(createdAt) ||
    !isRecord(data)
  ) {
    throw new WebhookVerificationError("invalid_payload", "the body is not a gateway event envelope");
  }
  const shape = Object.prototype.hasOwnProperty.call(EVENT_SHAPES, type)
    ? EVENT_SHAPES[type as WebhookEventType]
    : undefined;
  if (shape === undefined) {
    throw new UnknownWebhookEventError({ id, type, created_at: createdAt, data });
  }
  const attributes = data["attributes"];
  if (data["object"] !== shape.object || typeof data["id"] !== "string" || !isRecord(attributes)) {
    throw new WebhookVerificationError("invalid_payload", `the ${type} event has an unexpected data object`);
  }
  for (const [field, kind] of Object.entries(shape.attributes)) {
    const value = attributes[field];
    const ok = typeof value === "string" || (kind === "string|null" && value === null);
    if (!ok) {
      throw new WebhookVerificationError("invalid_payload", `the ${type} event is missing attributes.${field}`);
    }
  }
  return body as unknown as WebhookEvent;
}

/**
 * Verifies a delivery and returns its typed event.
 *
 * The signature is checked over the raw bytes first, in constant time, against
 * every `v1` value and every secret; only then is the body parsed. Throws
 * `WebhookVerificationError` for a delivery to refuse, and
 * `UnknownWebhookEventError` for a genuine event of a type this SDK does not know.
 * A misconfigured receiver (no secrets, a secret that is not 64 hex
 * characters) is a `TypeError`, not a verdict on the delivery.
 */
export function verifyWebhook(options: VerifyWebhookOptions): WebhookEvent {
  if (options.secrets.length === 0) {
    throw new TypeError("at least one webhook signing secret is required");
  }
  const keys = options.secrets.map((secret) => {
    assertSecret(secret);
    return Buffer.from(secret, "hex");
  });
  const tolerance = options.toleranceSeconds ?? DEFAULT_TOLERANCE_SECONDS;
  if (!Number.isFinite(tolerance) || tolerance < 0) {
    throw new RangeError("toleranceSeconds must be a non-negative number");
  }

  let header: string | undefined;
  if (typeof options.header === "string") {
    header = options.header;
  } else if (Array.isArray(options.header)) {
    if (options.header.length > 1) {
      throw new WebhookVerificationError("malformed_header", "the signature header was sent more than once");
    }
    header = options.header[0];
  }
  if (header === undefined || header.trim().length === 0) {
    throw new WebhookVerificationError("missing_header", "no Gateway-Signature header");
  }
  const parsed = parseSignatureHeader(header);
  if (parsed === null) {
    throw new WebhookVerificationError("malformed_header", "expected t=<unix seconds>,v1=<64 hex>[,v1=<64 hex>]");
  }

  const nowSeconds = Math.floor((options.now ?? new Date()).getTime() / 1000);
  if (Math.abs(nowSeconds - parsed.timestamp) > tolerance) {
    throw new WebhookVerificationError(
      "timestamp_outside_tolerance",
      `signed at ${parsed.timestamp}, now ${nowSeconds}, tolerance ${tolerance}s`,
    );
  }

  const bytes = toBytes(options.payload);
  const candidates = parsed.signatures.map((signature) => Buffer.from(signature, "hex"));
  // Every pair is compared without an early exit, so the time taken does not
  // reveal which secret or which v1 value matched.
  let matched = false;
  for (const key of keys) {
    const expected = createHmac("sha256", key).update(`${parsed.timestamp}.`, "utf8").update(bytes).digest();
    for (const candidate of candidates) {
      if (timingSafeEqual(expected, candidate)) {
        matched = true;
      }
    }
  }
  if (!matched) {
    throw new WebhookVerificationError("signature_mismatch", "no v1 signature matches any configured secret");
  }
  return parseEnvelope(bytes);
}
