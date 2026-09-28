import {
  GatewayError,
  GatewayNetworkError,
  GatewayProtocolError,
  GatewayTimeoutError,
  type GatewayErrorCode,
} from "./errors.js";
import { assertIdempotencyKey } from "./idempotency.js";
import { isIntegerString } from "./money.js";
import {
  backoffDelayMs,
  isIdempotentRequest,
  isRetryableStatus,
  parseRetryAfter,
  resolveRetryOptions,
  type ResolvedRetryOptions,
  type RetryOptions,
} from "./retry.js";
import type {
  CancelPaymentIntentParams,
  CheckoutView,
  CreatePaymentIntentParams,
  CreateQuoteParams,
  IdempotentResult,
  IssuedQuote,
  PaymentIntent,
} from "./types.js";
import { SDK_VERSION } from "./version.js";

export type FetchLike = (input: string, init: RequestInit) => Promise<Response>;

export interface GatewayClientOptions {
  /** The gateway's origin, for example `https://pay.example.com`. A path prefix is kept. */
  baseUrl: string;
  /** The merchant API key, 32 to 256 characters. Sent as a bearer token. */
  apiKey: string;
  /** A `fetch` implementation. Defaults to the global one (Node 18+). */
  fetch?: FetchLike;
  /** Per attempt, including reading the body. Default 30000. */
  timeoutMs?: number;
  /** Retry policy for idempotent requests; `false` disables retries. */
  retry?: RetryOptions | false;
  /**
   * Allows a plain `http://` base URL on a host other than localhost. The API
   * key travels in every request; leave this off outside a private network.
   */
  allowInsecureHttp?: boolean;
  /** Sleeps between retries. Injected by tests; the default is a timer. */
  sleep?: (ms: number, signal?: AbortSignal) => Promise<void>;
  /** A source of uniform numbers in [0, 1) for retry jitter. Injected by tests. */
  random?: () => number;
}

export interface RequestOptions {
  /** Aborts the request and any retry wait. */
  signal?: AbortSignal;
  /** Overrides the client's per-attempt timeout for this call. */
  timeoutMs?: number;
}

export interface WriteOptions extends RequestOptions {
  /**
   * Required on every write: 16 to 128 characters of `A-Z a-z 0-9 _ -`.
   * Reuse the same key to retry the same operation; see `newIdempotencyKey`.
   */
  idempotencyKey: string;
}

const UUID = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const CHECKOUT_TOKEN = /^[0-9a-f]{64}$/;
const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);
const DEFAULT_TIMEOUT_MS = 30_000;
const MAX_BODY_IN_MESSAGE = 200;

interface RouteSpec<T> {
  method: "GET" | "POST";
  path: string;
  body?: unknown;
  idempotencyKey?: string;
  authenticated: boolean;
  options: RequestOptions | undefined;
  accept: (value: unknown) => value is T;
  expect: string;
}

interface Success<T> {
  data: T;
  status: number;
  headers: Headers;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isPaymentIntent(value: unknown): value is PaymentIntent {
  return (
    isRecord(value) &&
    typeof value["id"] === "string" &&
    typeof value["status"] === "string" &&
    isRecord(value["amount"]) &&
    isIntegerString(value["amount"]["minor_units"])
  );
}

function isIssuedQuote(value: unknown): value is IssuedQuote {
  return (
    isRecord(value) &&
    typeof value["id"] === "string" &&
    typeof value["collector_address"] === "string" &&
    typeof value["checkout_token"] === "string" &&
    isIntegerString(value["amount_raw"]) &&
    isRecord(value["asset"]) &&
    Number.isInteger(value["asset"]["decimals"])
  );
}

function isCheckoutView(value: unknown): value is CheckoutView {
  return (
    isRecord(value) &&
    typeof value["status"] === "string" &&
    typeof value["collector_address"] === "string" &&
    isIntegerString(value["amount_raw"])
  );
}

function assertUuid(name: string, value: unknown): asserts value is string {
  if (typeof value !== "string" || !UUID.test(value)) {
    throw new TypeError(`${name} must be a UUID, got ${JSON.stringify(value)}`);
  }
}

function assertCheckoutToken(value: unknown): asserts value is string {
  if (typeof value !== "string" || !CHECKOUT_TOKEN.test(value)) {
    throw new TypeError("a checkout token is 64 lowercase hex characters");
  }
}

function assertTimeout(value: number): number {
  if (!Number.isInteger(value) || value <= 0) {
    throw new RangeError(`timeoutMs must be a positive integer, got ${String(value)}`);
  }
  return value;
}

function defaultSleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted === true) {
      reject(signal.reason);
      return;
    }
    const onAbort = () => {
      clearTimeout(timer);
      reject(signal?.reason);
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function readErrorEnvelope(text: string): { code: string; message: string } | null {
  try {
    const parsed: unknown = JSON.parse(text);
    if (isRecord(parsed) && isRecord(parsed["error"])) {
      const { code, message } = parsed["error"];
      if (typeof code === "string" && typeof message === "string") {
        return { code, message };
      }
    }
  } catch {
    // A body that is not JSON (a proxy page, an empty 408) has no envelope;
    // the caller reports it with a null code and the raw text.
  }
  return null;
}

/**
 * A client for the gateway's merchant API.
 *
 * Every method either resolves with what the gateway returned or throws: a
 * `GatewayError` for an HTTP error, a `GatewayNetworkError` when no response
 * arrived, a `GatewayProtocolError` for a 2xx body that is not what the route
 * promises. Nothing resolves to an empty or default value.
 */
export class GatewayClient {
  readonly baseUrl: string;
  readonly #apiKey: string;
  readonly #fetch: FetchLike;
  readonly #timeoutMs: number;
  readonly #retry: ResolvedRetryOptions;
  readonly #sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  readonly #random: () => number;

  constructor(options: GatewayClientOptions) {
    let url: URL;
    try {
      url = new URL(options.baseUrl);
    } catch {
      throw new TypeError(`baseUrl is not a URL: ${JSON.stringify(options.baseUrl)}`);
    }
    if (url.protocol !== "https:" && url.protocol !== "http:") {
      throw new TypeError("baseUrl must be http or https");
    }
    if (url.protocol === "http:" && !LOCAL_HOSTS.has(url.hostname) && options.allowInsecureHttp !== true) {
      throw new TypeError("baseUrl must be https: the API key is sent with every request (set allowInsecureHttp to override)");
    }
    if (url.search !== "" || url.hash !== "" || url.username !== "" || url.password !== "") {
      throw new TypeError("baseUrl must not carry credentials, a query or a fragment");
    }
    if (typeof options.apiKey !== "string" || options.apiKey.length < 32 || options.apiKey.length > 256 || /\s/.test(options.apiKey)) {
      throw new TypeError("apiKey must be the merchant key: 32 to 256 characters without whitespace");
    }
    const fetchImpl = options.fetch ?? (globalThis.fetch as FetchLike | undefined);
    if (fetchImpl === undefined) {
      throw new TypeError("no fetch implementation: use Node 18 or later, or pass options.fetch");
    }
    this.baseUrl = `${url.origin}${url.pathname.replace(/\/+$/, "")}`;
    this.#apiKey = options.apiKey;
    this.#fetch = fetchImpl;
    this.#timeoutMs = assertTimeout(options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
    this.#retry = resolveRetryOptions(options.retry);
    this.#sleep = options.sleep ?? defaultSleep;
    this.#random = options.random ?? Math.random;
  }

  /** `POST /v1/payment-intents`: the obligation, in fiat minor units. */
  async createPaymentIntent(
    params: CreatePaymentIntentParams,
    options: WriteOptions,
  ): Promise<IdempotentResult<PaymentIntent>> {
    if (!isIntegerString(params.amount_minor)) {
      throw new TypeError(
        `amount_minor must be an integer string of minor units (never a number), got ${JSON.stringify(params.amount_minor)}`,
      );
    }
    if (typeof params.currency !== "string" || typeof params.reference !== "string" || params.reference.length === 0) {
      throw new TypeError("currency and a non-empty reference are required");
    }
    const body: Record<string, unknown> = {
      amount_minor: params.amount_minor,
      currency: params.currency,
      reference: params.reference,
    };
    if (params.description !== undefined) {
      body["description"] = params.description;
    }
    if (params.metadata !== undefined) {
      body["metadata"] = params.metadata;
    }
    return this.#write({
      method: "POST",
      path: "/v1/payment-intents",
      body,
      options,
      accept: isPaymentIntent,
      expect: "a payment intent",
    });
  }

  /** `GET /v1/payment-intents/{intent_id}`. Another merchant's intent is a 404. */
  async getPaymentIntent(intentId: string, options?: RequestOptions): Promise<PaymentIntent> {
    assertUuid("intentId", intentId);
    const response = await this.#request({
      method: "GET",
      path: `/v1/payment-intents/${intentId}`,
      authenticated: true,
      options,
      accept: isPaymentIntent,
      expect: "a payment intent",
    });
    return response.data;
  }

  /**
   * `POST /v1/payment-intents/{intent_id}/quotes`: an exact token amount at
   * one collector address, for a bounded time.
   */
  async createQuote(
    intentId: string,
    params: CreateQuoteParams,
    options: WriteOptions,
  ): Promise<IdempotentResult<IssuedQuote>> {
    assertUuid("intentId", intentId);
    assertUuid("asset_id", params.asset_id);
    return this.#write({
      method: "POST",
      path: `/v1/payment-intents/${intentId}/quotes`,
      body: { asset_id: params.asset_id },
      options,
      accept: isIssuedQuote,
      expect: "a quote",
    });
  }

  /**
   * `POST /v1/payment-intents/{intent_id}/cancel`: closes an order that has
   * no money on it and queues a `payment_intent.cancelled` webhook.
   */
  async cancelPaymentIntent(
    intentId: string,
    params: CancelPaymentIntentParams,
    options: WriteOptions,
  ): Promise<IdempotentResult<PaymentIntent>> {
    assertUuid("intentId", intentId);
    const body: Record<string, unknown> = {};
    if (params.reason !== undefined) {
      body["reason"] = params.reason;
    }
    return this.#write({
      method: "POST",
      path: `/v1/payment-intents/${intentId}/cancel`,
      body,
      options,
      accept: isPaymentIntent,
      expect: "a payment intent",
    });
  }

  /**
   * `GET /v1/checkout/{checkout_token}`: the buyer-facing state of one
   * attempt. Public: the API key is not sent.
   */
  async getCheckoutView(checkoutToken: string, options?: RequestOptions): Promise<CheckoutView> {
    assertCheckoutToken(checkoutToken);
    const response = await this.#request({
      method: "GET",
      path: `/v1/checkout/${checkoutToken}`,
      authenticated: false,
      options,
      accept: isCheckoutView,
      expect: "a checkout view",
    });
    return response.data;
  }

  /** The hosted payment page for a quote, or for its `checkout_token`. */
  checkoutUrl(quoteOrToken: Pick<IssuedQuote, "checkout_token"> | string): string {
    const token = typeof quoteOrToken === "string" ? quoteOrToken : quoteOrToken.checkout_token;
    assertCheckoutToken(token);
    return `${this.baseUrl}/checkout/${token}`;
  }

  /** The QR code of the payment address, as an SVG URL. */
  checkoutQrUrl(quoteOrToken: Pick<IssuedQuote, "checkout_token"> | string): string {
    const token = typeof quoteOrToken === "string" ? quoteOrToken : quoteOrToken.checkout_token;
    assertCheckoutToken(token);
    return `${this.baseUrl}/v1/checkout/${token}/qr.svg`;
  }

  async #write<T>(
    spec: Omit<RouteSpec<T>, "authenticated" | "idempotencyKey" | "options"> & { options: WriteOptions },
  ): Promise<IdempotentResult<T>> {
    if (spec.options === undefined || spec.options === null) {
      throw new TypeError("options.idempotencyKey is required on every write");
    }
    assertIdempotencyKey(spec.options.idempotencyKey);
    const response = await this.#request({
      ...spec,
      authenticated: true,
      idempotencyKey: spec.options.idempotencyKey,
    });
    const replayed = response.headers.get("idempotent-replayed");
    return {
      data: response.data,
      replayed: replayed === "true" ? true : replayed === "false" ? false : null,
      status: response.status,
    };
  }

  async #request<T>(spec: RouteSpec<T>): Promise<Success<T>> {
    const maxRetries = isIdempotentRequest(spec.method, spec.idempotencyKey) ? this.#retry.maxRetries : 0;
    const signal = spec.options?.signal;
    for (let attempt = 0; ; attempt += 1) {
      let delayMs: number;
      try {
        return await this.#attempt(spec);
      } catch (error) {
        const canRetry = attempt < maxRetries && signal?.aborted !== true;
        if (error instanceof GatewayNetworkError) {
          if (!canRetry || error.kind === "aborted") {
            throw error;
          }
          delayMs = backoffDelayMs(attempt, this.#retry, this.#random);
        } else if (error instanceof GatewayError) {
          if (!canRetry || !isRetryableStatus(error.status)) {
            throw error;
          }
          if (error.retryAfterSeconds !== null) {
            delayMs = error.retryAfterSeconds * 1000;
            if (delayMs > this.#retry.maxRetryAfterMs) {
              throw error;
            }
          } else {
            delayMs = backoffDelayMs(attempt, this.#retry, this.#random);
          }
        } else {
          throw error;
        }
      }
      try {
        await this.#sleep(delayMs, signal);
      } catch (cause) {
        throw new GatewayNetworkError("aborted", spec.method, spec.path, "aborted while waiting to retry", cause);
      }
    }
  }

  async #attempt<T>(spec: RouteSpec<T>): Promise<Success<T>> {
    const timeoutMs = assertTimeout(spec.options?.timeoutMs ?? this.#timeoutMs);
    const userSignal = spec.options?.signal;
    if (userSignal?.aborted === true) {
      throw new GatewayNetworkError("aborted", spec.method, spec.path, "the signal was already aborted", userSignal.reason);
    }
    const controller = new AbortController();
    const timeout = { fired: false };
    const timer = setTimeout(() => {
      timeout.fired = true;
      controller.abort();
    }, timeoutMs);
    const onAbort = () => controller.abort(userSignal?.reason);
    userSignal?.addEventListener("abort", onAbort, { once: true });

    const headers: Record<string, string> = {
      accept: "application/json",
      "user-agent": `crypto-gateway-sdk-typescript/${SDK_VERSION}`,
    };
    if (spec.authenticated) {
      headers["authorization"] = `Bearer ${this.#apiKey}`;
    }
    if (spec.idempotencyKey !== undefined) {
      headers["idempotency-key"] = spec.idempotencyKey;
    }
    const init: RequestInit = { method: spec.method, headers, signal: controller.signal, redirect: "error" };
    if (spec.body !== undefined) {
      headers["content-type"] = "application/json";
      init.body = JSON.stringify(spec.body);
    }

    let response: Response;
    let text: string;
    try {
      response = await this.#fetch(`${this.baseUrl}${spec.path}`, init);
      text = await response.text();
    } catch (cause) {
      if (timeout.fired) {
        throw new GatewayTimeoutError(spec.method, spec.path, timeoutMs);
      }
      if (controller.signal.aborted) {
        throw new GatewayNetworkError("aborted", spec.method, spec.path, "aborted by the caller", cause);
      }
      throw new GatewayNetworkError("network", spec.method, spec.path, String(cause), cause);
    } finally {
      clearTimeout(timer);
      userSignal?.removeEventListener("abort", onAbort);
    }

    if (response.status < 200 || response.status > 299) {
      const envelope = readErrorEnvelope(text);
      throw new GatewayError({
        status: response.status,
        code: (envelope?.code ?? null) as GatewayErrorCode | null,
        message: envelope?.message ?? `HTTP ${response.status}: ${text.slice(0, MAX_BODY_IN_MESSAGE)}`,
        retryAfterSeconds: parseRetryAfter(response.headers.get("retry-after")),
        requestId: response.headers.get("x-request-id"),
        body: text,
        method: spec.method,
        path: spec.path,
      });
    }

    let data: unknown;
    try {
      data = JSON.parse(text);
    } catch (error) {
      throw new GatewayProtocolError(response.status, spec.method, spec.path, `not JSON: ${String(error)}`, text);
    }
    if (!spec.accept(data)) {
      throw new GatewayProtocolError(response.status, spec.method, spec.path, `expected ${spec.expect}`, text);
    }
    return { data, status: response.status, headers: response.headers };
  }
}
