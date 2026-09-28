/**
 * Every error code the merchant and checkout routes answer with, taken from
 * the gateway's error table (`crates/gateway-http/src/error.rs`).
 */
export type GatewayErrorCode =
  | "authentication_failed"
  | "invalid_request"
  | "invalid_idempotency_key"
  | "payment_intent_not_found"
  | "idempotency_conflict"
  | "payment_intent_reference_conflict"
  | "payment_intent_not_cancellable"
  | "payment_intent_not_quotable"
  | "quote_unavailable"
  | "rail_stopped"
  | "checkout_not_found"
  | "storage_unavailable"
  | "internal_error";

export const GATEWAY_ERROR_CODES: readonly GatewayErrorCode[] = [
  "authentication_failed",
  "invalid_request",
  "invalid_idempotency_key",
  "payment_intent_not_found",
  "idempotency_conflict",
  "payment_intent_reference_conflict",
  "payment_intent_not_cancellable",
  "payment_intent_not_quotable",
  "quote_unavailable",
  "rail_stopped",
  "checkout_not_found",
  "storage_unavailable",
  "internal_error",
];

/** The base of every error this SDK throws, so one `instanceof` catches them all. */
export class GatewaySdkError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = new.target.name;
  }
}

export interface GatewayErrorInit {
  status: number;
  code: GatewayErrorCode | (string & {}) | null;
  message: string;
  retryAfterSeconds: number | null;
  requestId: string | null;
  body: string;
  method: string;
  path: string;
}

/**
 * The gateway answered with a non-2xx status.
 *
 * `code` is the API's error code when the body carried the error envelope. It
 * is `null` when it did not (a proxy's page, the server's request timeout),
 * and it is kept verbatim when it is a code this SDK version does not know.
 */
export class GatewayError extends GatewaySdkError {
  readonly status: number;
  readonly code: GatewayErrorCode | (string & {}) | null;
  /** Seconds to wait before retrying, from `Retry-After`; `null` when absent or unreadable. */
  readonly retryAfterSeconds: number | null;
  /** The `X-Request-Id` response header, when a proxy or the gateway sets one. */
  readonly requestId: string | null;
  /** The raw response body, for logs. */
  readonly body: string;
  readonly method: string;
  readonly path: string;

  constructor(init: GatewayErrorInit) {
    super(`${init.method} ${init.path} failed: ${init.status} ${init.code ?? "(no error code)"}: ${init.message}`);
    this.status = init.status;
    this.code = init.code;
    this.retryAfterSeconds = init.retryAfterSeconds;
    this.requestId = init.requestId;
    this.body = init.body;
    this.method = init.method;
    this.path = init.path;
  }

  /** Whether `code` is one of the codes this SDK version knows. */
  get isKnownCode(): boolean {
    return this.code !== null && (GATEWAY_ERROR_CODES as readonly string[]).includes(this.code);
  }
}

export type NetworkFailureKind = "network" | "timeout" | "aborted";

const NETWORK_VERB: Record<NetworkFailureKind, string> = {
  network: "failed",
  timeout: "timed out",
  aborted: "was aborted",
};

/**
 * No HTTP response arrived: the connection failed, the request timed out, or
 * the caller aborted it. For a write this means the outcome is unknown; retry
 * it with the same idempotency key to learn it.
 */
export class GatewayNetworkError extends GatewaySdkError {
  readonly kind: NetworkFailureKind;
  readonly method: string;
  readonly path: string;

  constructor(kind: NetworkFailureKind, method: string, path: string, message: string, cause?: unknown) {
    super(`${method} ${path} ${NETWORK_VERB[kind]}: ${message}`, { cause });
    this.kind = kind;
    this.method = method;
    this.path = path;
  }
}

/** The request did not finish within `timeoutMs`. */
export class GatewayTimeoutError extends GatewayNetworkError {
  readonly timeoutMs: number;

  constructor(method: string, path: string, timeoutMs: number) {
    super("timeout", method, path, `no response within ${timeoutMs} ms`);
    this.timeoutMs = timeoutMs;
  }
}

/** A 2xx response whose body is not the JSON the route promises. */
export class GatewayProtocolError extends GatewaySdkError {
  readonly status: number;
  readonly body: string;

  constructor(status: number, method: string, path: string, message: string, body: string) {
    super(`${method} ${path} returned ${status} with an unusable body: ${message}`);
    this.status = status;
    this.body = body;
  }
}
