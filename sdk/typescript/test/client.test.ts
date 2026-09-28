import assert from "node:assert/strict";
import { describe, test } from "node:test";

import {
  GatewayClient,
  GatewayError,
  GatewayNetworkError,
  GatewayProtocolError,
  GatewayTimeoutError,
  newIdempotencyKey,
  type GatewayClientOptions,
} from "../src/index.js";

const BASE = "https://pay.example.com";
const API_KEY = `gw_${"a".repeat(64)}`;
const KEY = "order-1042-attempt-1";
const INTENT_ID = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
const ASSET_ID = "0190a1b2-c3d4-7e5f-8a9b-000000000001";
const TOKEN = "ab".repeat(32);

const INTENT = {
  id: INTENT_ID,
  merchant_id: "0190a1b2-c3d4-7e5f-8a9b-000000000002",
  amount: { currency: "USD", minor_units: "4999" },
  status: "requires_quote",
  reference: "order-1042",
  description: null,
  metadata: {},
  created_at: "2026-09-28T10:00:00Z",
  updated_at: "2026-09-28T10:00:00Z",
};

const QUOTE = {
  id: "0190a1b2-c3d4-7e5f-8a9b-000000000003",
  attempt_id: "0190a1b2-c3d4-7e5f-8a9b-000000000004",
  payment_intent_id: INTENT_ID,
  asset_id: ASSET_ID,
  collector_address_id: "0190a1b2-c3d4-7e5f-8a9b-000000000005",
  collector_address: "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
  price_snapshot_id: "0190a1b2-c3d4-7e5f-8a9b-000000000006",
  quote_policy_id: "0190a1b2-c3d4-7e5f-8a9b-000000000007",
  rail_health_snapshot_id: "0190a1b2-c3d4-7e5f-8a9b-000000000008",
  fiat_amount: { currency: "USD", minor_units: "4999" },
  amount_raw: "49990017",
  rate_numerator: "1000000",
  rate_denominator: "100",
  price_sources: [],
  price_observed_at: "2026-09-28T10:00:00Z",
  policy_version: "v1",
  rail_health_observed_at: "2026-09-28T10:00:00Z",
  created_at: "2026-09-28T10:00:00Z",
  expires_at: "2026-09-28T10:15:00Z",
  late_payment_until: "2026-09-29T10:00:00Z",
  asset: {
    chain: "tron",
    network: "nile",
    chain_environment: "testnet",
    symbol: "USDT",
    decimals: 6,
    contract_address: "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
  },
  amount: "49.990017",
  checkout_token: TOKEN,
};

const CHECKOUT = {
  status: "waiting",
  merchant_name: "Shop",
  description: null,
  fiat_amount: { currency: "USD", minor_units: "4999" },
  asset: QUOTE.asset,
  collector_address: QUOTE.collector_address,
  amount_raw: "49990017",
  amount: "49.990017",
  received: "0",
  expires_at: QUOTE.expires_at,
  late_payment_until: QUOTE.late_payment_until,
  transaction_hash: null,
  explorer_url: null,
};

interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: string | undefined;
  signal: AbortSignal | undefined;
}

type Reply = Response | Error | ((call: Call) => Promise<Response>);

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });
}

function apiError(status: number, code: string, message = "refused", headers: Record<string, string> = {}): Response {
  return json(status, { error: { code, message } }, headers);
}

function harness(replies: Reply[], options: Partial<GatewayClientOptions> = {}) {
  const calls: Call[] = [];
  const sleeps: number[] = [];
  const fetch = async (url: string, init: RequestInit): Promise<Response> => {
    const call: Call = {
      url,
      method: init.method ?? "GET",
      headers: { ...(init.headers as Record<string, string>) },
      body: init.body as string | undefined,
      signal: init.signal ?? undefined,
    };
    calls.push(call);
    const reply = replies.shift();
    if (reply === undefined) {
      throw new Error(`unexpected request ${call.method} ${url}`);
    }
    if (reply instanceof Error) {
      throw reply;
    }
    if (typeof reply === "function") {
      return reply(call);
    }
    return reply;
  };
  const client = new GatewayClient({
    baseUrl: BASE,
    apiKey: API_KEY,
    fetch,
    sleep: async (ms) => {
      sleeps.push(ms);
    },
    random: () => 0.5,
    ...options,
  });
  return { client, calls, sleeps, remaining: replies };
}

describe("request building", () => {
  test("createPaymentIntent posts the exact body with key, bearer and JSON headers", async () => {
    const { client, calls } = harness([json(201, INTENT, { "idempotent-replayed": "false" })]);
    const result = await client.createPaymentIntent(
      { amount_minor: "4999", currency: "USD", reference: "order-1042", metadata: { cart: 7 } },
      { idempotencyKey: KEY },
    );
    assert.deepEqual(result, { data: INTENT, replayed: false, status: 201 });
    assert.equal(calls.length, 1);
    const call = calls[0]!;
    assert.equal(call.url, `${BASE}/v1/payment-intents`);
    assert.equal(call.method, "POST");
    assert.equal(call.headers["authorization"], `Bearer ${API_KEY}`);
    assert.equal(call.headers["idempotency-key"], KEY);
    assert.equal(call.headers["content-type"], "application/json");
    assert.equal(call.headers["accept"], "application/json");
    assert.match(call.headers["user-agent"] ?? "", /^crypto-gateway-sdk-typescript\/\d+\.\d+\.\d+$/);
    assert.equal(call.body, '{"amount_minor":"4999","currency":"USD","reference":"order-1042","metadata":{"cart":7}}');
  });

  test("a replay reports replayed, and a missing header is reported as unknown, not false", async () => {
    const { client } = harness([json(200, INTENT, { "idempotent-replayed": "true" }), json(201, INTENT)]);
    const params = { amount_minor: "4999", currency: "USD", reference: "order-1042" };
    assert.equal((await client.createPaymentIntent(params, { idempotencyKey: KEY })).replayed, true);
    assert.equal((await client.createPaymentIntent(params, { idempotencyKey: KEY })).replayed, null);
  });

  test("getPaymentIntent is a bare authenticated GET", async () => {
    const { client, calls } = harness([json(200, INTENT)]);
    assert.deepEqual(await client.getPaymentIntent(INTENT_ID), INTENT);
    const call = calls[0]!;
    assert.equal(call.url, `${BASE}/v1/payment-intents/${INTENT_ID}`);
    assert.equal(call.method, "GET");
    assert.equal(call.body, undefined);
    assert.equal(call.headers["idempotency-key"], undefined);
    assert.equal(call.headers["content-type"], undefined);
    assert.equal(call.headers["authorization"], `Bearer ${API_KEY}`);
  });

  test("createQuote posts only the asset id to the intent's quotes", async () => {
    const { client, calls } = harness([json(201, QUOTE, { "idempotent-replayed": "false" })]);
    const result = await client.createQuote(INTENT_ID, { asset_id: ASSET_ID }, { idempotencyKey: KEY });
    assert.equal(result.data.amount_raw, "49990017");
    assert.equal(calls[0]!.url, `${BASE}/v1/payment-intents/${INTENT_ID}/quotes`);
    assert.equal(calls[0]!.body, JSON.stringify({ asset_id: ASSET_ID }));
  });

  test("cancelPaymentIntent sends an empty object, or the reason", async () => {
    const cancelled = { ...INTENT, status: "cancelled" };
    const { client, calls } = harness([json(200, cancelled), json(200, cancelled)]);
    await client.cancelPaymentIntent(INTENT_ID, {}, { idempotencyKey: KEY });
    await client.cancelPaymentIntent(INTENT_ID, { reason: "customer asked" }, { idempotencyKey: `${KEY}-2` });
    assert.equal(calls[0]!.url, `${BASE}/v1/payment-intents/${INTENT_ID}/cancel`);
    assert.equal(calls[0]!.body, "{}");
    assert.equal(calls[1]!.body, '{"reason":"customer asked"}');
  });

  test("getCheckoutView is public: the API key is never sent", async () => {
    const { client, calls } = harness([json(200, CHECKOUT)]);
    assert.deepEqual(await client.getCheckoutView(TOKEN), CHECKOUT);
    assert.equal(calls[0]!.url, `${BASE}/v1/checkout/${TOKEN}`);
    assert.equal(calls[0]!.headers["authorization"], undefined);
  });

  test("checkout URLs keep a path prefix and drop a trailing slash", () => {
    const { client } = harness([], { baseUrl: "https://example.com/gateway/" });
    assert.equal(client.checkoutUrl(TOKEN), `https://example.com/gateway/checkout/${TOKEN}`);
    assert.equal(client.checkoutUrl({ checkout_token: TOKEN }), `https://example.com/gateway/checkout/${TOKEN}`);
    assert.equal(client.checkoutQrUrl(TOKEN), `https://example.com/gateway/v1/checkout/${TOKEN}/qr.svg`);
    assert.throws(() => client.checkoutUrl("../admin"), TypeError);
  });
});

describe("arguments are checked before anything is sent", () => {
  const params = { amount_minor: "4999", currency: "USD", reference: "order-1042" };

  test("the idempotency key is required and must be 16 to 128 URL-safe characters", async () => {
    const { client, calls } = harness([]);
    for (const idempotencyKey of ["x".repeat(15), "x".repeat(129), "has space in it!!", "a/b/c/d/e/f/g/h/i", ""]) {
      await assert.rejects(client.createPaymentIntent(params, { idempotencyKey }), TypeError, idempotencyKey);
    }
    await assert.rejects(
      client.createPaymentIntent(params, undefined as unknown as { idempotencyKey: string }),
      TypeError,
    );
    await assert.rejects(client.createQuote(INTENT_ID, { asset_id: ASSET_ID }, {} as { idempotencyKey: string }), TypeError);
    assert.equal(calls.length, 0);
  });

  test("the boundary lengths are accepted", async () => {
    const { client } = harness([json(201, INTENT), json(201, INTENT)]);
    await client.createPaymentIntent(params, { idempotencyKey: "k".repeat(16) });
    await client.createPaymentIntent(params, { idempotencyKey: "K_-9".repeat(32) });
  });

  test("an amount must be an integer string, never a number", async () => {
    const { client, calls } = harness([]);
    for (const amount_minor of [4999 as unknown as string, "49.99", "", "-1", "1e3"]) {
      await assert.rejects(client.createPaymentIntent({ ...params, amount_minor }, { idempotencyKey: KEY }), TypeError);
    }
    assert.equal(calls.length, 0);
  });

  test("ids must be UUIDs and tokens 64 hex characters, so nothing reaches the path unescaped", async () => {
    const { client, calls } = harness([]);
    await assert.rejects(client.getPaymentIntent("../operator/overview"), TypeError);
    await assert.rejects(client.createQuote(INTENT_ID, { asset_id: "USDT" }, { idempotencyKey: KEY }), TypeError);
    await assert.rejects(client.getCheckoutView(TOKEN.toUpperCase()), TypeError);
    assert.equal(calls.length, 0);
  });

  test("the constructor refuses a key sent in clear, a bad key and a bad URL", () => {
    const fetch = async () => json(200, {});
    assert.throws(() => new GatewayClient({ baseUrl: "http://pay.example.com", apiKey: API_KEY, fetch }), TypeError);
    assert.doesNotThrow(() => new GatewayClient({ baseUrl: "http://localhost:8080", apiKey: API_KEY, fetch }));
    assert.doesNotThrow(
      () => new GatewayClient({ baseUrl: "http://gateway:8080", apiKey: API_KEY, fetch, allowInsecureHttp: true }),
    );
    assert.throws(() => new GatewayClient({ baseUrl: BASE, apiKey: "short", fetch }), TypeError);
    assert.throws(() => new GatewayClient({ baseUrl: "pay.example.com", apiKey: API_KEY, fetch }), TypeError);
    assert.throws(() => new GatewayClient({ baseUrl: `${BASE}?x=1`, apiKey: API_KEY, fetch }), TypeError);
    assert.throws(() => new GatewayClient({ baseUrl: BASE, apiKey: API_KEY, fetch, timeoutMs: 0 }), RangeError);
    assert.throws(() => new GatewayClient({ baseUrl: BASE, apiKey: API_KEY, fetch, retry: { maxRetries: -1 } }), RangeError);
  });

  test("newIdempotencyKey is valid, unique and takes an optional prefix", async () => {
    const a = newIdempotencyKey();
    const b = newIdempotencyKey("order-1042");
    assert.match(a, /^[A-Za-z0-9_-]{16,128}$/);
    assert.match(b, /^order-1042-[0-9a-f-]{36}$/);
    assert.notEqual(newIdempotencyKey(), a);
    assert.throws(() => newIdempotencyKey("bad prefix"), TypeError);
    const { client } = harness([json(201, INTENT)]);
    await client.createPaymentIntent(params, { idempotencyKey: b });
  });
});

describe("error mapping", () => {
  const params = { amount_minor: "4999", currency: "USD", reference: "order-1042" };

  test("the error envelope becomes status, code and message", async () => {
    const { client } = harness([apiError(409, "payment_intent_reference_conflict", "the reference is already used")]);
    await assert.rejects(client.createPaymentIntent(params, { idempotencyKey: KEY }), (error: unknown) => {
      assert.ok(error instanceof GatewayError);
      assert.equal(error.status, 409);
      assert.equal(error.code, "payment_intent_reference_conflict");
      assert.equal(error.isKnownCode, true);
      assert.equal(error.retryAfterSeconds, null);
      assert.equal(error.requestId, null);
      assert.match(error.message, /409 payment_intent_reference_conflict: the reference is already used/);
      return true;
    });
  });

  for (const [status, code] of [
    [401, "authentication_failed"],
    [400, "invalid_idempotency_key"],
    [404, "payment_intent_not_found"],
    [409, "idempotency_conflict"],
    [409, "payment_intent_not_quotable"],
    [409, "payment_intent_not_cancellable"],
    [422, "invalid_request"],
  ] as const) {
    test(`${status} ${code} is thrown at once, never retried`, async () => {
      const { client, calls } = harness([apiError(status, code)]);
      await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
        return error instanceof GatewayError && error.status === status && error.code === code;
      });
      assert.equal(calls.length, 1);
    });
  }

  test("an unknown code is kept verbatim", async () => {
    const { client } = harness([apiError(418, "teapot_engaged")]);
    await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
      return error instanceof GatewayError && error.code === "teapot_engaged" && !error.isKnownCode;
    });
  });

  test("a body without the envelope has a null code and keeps the raw text", async () => {
    const page = "<html>bad gateway</html>";
    const { client } = harness([new Response(page, { status: 502, headers: { "x-request-id": "req-77" } })], {
      retry: false,
    });
    await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
      assert.ok(error instanceof GatewayError);
      assert.equal(error.code, null);
      assert.equal(error.body, page);
      assert.equal(error.requestId, "req-77");
      assert.match(error.message, /bad gateway/);
      return true;
    });
  });

  test("Retry-After is surfaced on the error", async () => {
    const { client } = harness([apiError(503, "quote_unavailable", "later", { "retry-after": "7" })], { retry: false });
    await assert.rejects(client.createQuote(INTENT_ID, { asset_id: ASSET_ID }, { idempotencyKey: KEY }), (error: unknown) => {
      return error instanceof GatewayError && error.code === "quote_unavailable" && error.retryAfterSeconds === 7;
    });
  });

  test("a 2xx that is not JSON, or not the promised resource, is a protocol error, never a default", async () => {
    const { client } = harness([
      new Response("", { status: 200 }),
      json(200, { ...INTENT, amount: { currency: "USD", minor_units: 4999 } }),
      json(201, { ...QUOTE, amount_raw: 49990017 }),
      json(200, null),
    ]);
    await assert.rejects(client.getPaymentIntent(INTENT_ID), GatewayProtocolError);
    await assert.rejects(client.getPaymentIntent(INTENT_ID), GatewayProtocolError);
    await assert.rejects(client.createQuote(INTENT_ID, { asset_id: ASSET_ID }, { idempotencyKey: KEY }), GatewayProtocolError);
    await assert.rejects(client.getCheckoutView(TOKEN), GatewayProtocolError);
  });

  test("a connection failure is a network error, distinct from an HTTP error", async () => {
    const { client } = harness([new TypeError("fetch failed")], { retry: false });
    await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
      return error instanceof GatewayNetworkError && error.kind === "network" && !(error instanceof GatewayError);
    });
  });

  test("a request that outlives timeoutMs is a timeout error", async () => {
    const hang = (call: Call) =>
      new Promise<Response>((_, reject) => {
        call.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      });
    const { client, calls } = harness([hang, hang], { timeoutMs: 20, retry: { maxRetries: 1, baseDelayMs: 1 } });
    await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
      return error instanceof GatewayTimeoutError && error.kind === "timeout" && error.timeoutMs === 20;
    });
    assert.equal(calls.length, 2);
  });

  test("a caller's abort is not retried", async () => {
    const controller = new AbortController();
    const hang = (call: Call) =>
      new Promise<Response>((_, reject) => {
        call.signal?.addEventListener("abort", () => reject(new Error("aborted")));
        controller.abort();
      });
    const { client, calls } = harness([hang]);
    await assert.rejects(client.getPaymentIntent(INTENT_ID, { signal: controller.signal }), (error: unknown) => {
      return error instanceof GatewayNetworkError && error.kind === "aborted";
    });
    assert.equal(calls.length, 1);
  });
});

describe("retries", () => {
  const params = { amount_minor: "4999", currency: "USD", reference: "order-1042" };

  test("a write with its key is retried on 5xx, with the same key and body", async () => {
    const { client, calls, sleeps } = harness([apiError(500, "internal_error"), apiError(503, "storage_unavailable"), json(201, INTENT)]);
    const result = await client.createPaymentIntent(params, { idempotencyKey: KEY });
    assert.equal(result.status, 201);
    assert.equal(calls.length, 3);
    assert.deepEqual(new Set(calls.map((call) => call.headers["idempotency-key"])), new Set([KEY]));
    assert.deepEqual(new Set(calls.map((call) => call.body)), new Set([calls[0]!.body]));
    // Full jitter with random() = 0.5 over ceilings 500 and 1000.
    assert.deepEqual(sleeps, [250, 500]);
  });

  test("a GET is retried on a network error and on 429", async () => {
    const { client, calls } = harness([new TypeError("fetch failed"), apiError(429, "rate_limited"), json(200, INTENT)]);
    assert.equal((await client.getPaymentIntent(INTENT_ID)).id, INTENT_ID);
    assert.equal(calls.length, 3);
  });

  test("Retry-After replaces the backoff", async () => {
    const { client, sleeps } = harness([apiError(429, "rate_limited", "slow down", { "retry-after": "2" }), json(200, INTENT)]);
    await client.getPaymentIntent(INTENT_ID);
    assert.deepEqual(sleeps, [2000]);
  });

  test("a Retry-After longer than maxRetryAfterMs is not slept through", async () => {
    const { client, calls, sleeps } = harness([apiError(429, "rate_limited", "later", { "retry-after": "3600" })]);
    await assert.rejects(client.getPaymentIntent(INTENT_ID), (error: unknown) => {
      return error instanceof GatewayError && error.status === 429 && error.retryAfterSeconds === 3600;
    });
    assert.equal(calls.length, 1);
    assert.deepEqual(sleeps, []);
  });

  test("gives up after maxRetries (default 3) and throws the last error", async () => {
    const replies = Array.from({ length: 4 }, () => apiError(503, "quote_unavailable"));
    const { client, calls, sleeps } = harness(replies);
    await assert.rejects(
      client.createQuote(INTENT_ID, { asset_id: ASSET_ID }, { idempotencyKey: KEY }),
      (error: unknown) => error instanceof GatewayError && error.code === "quote_unavailable",
    );
    assert.equal(calls.length, 4);
    assert.deepEqual(sleeps, [250, 500, 1000]);
  });

  test("backoff is capped by maxDelayMs", async () => {
    const replies = Array.from({ length: 5 }, () => apiError(500, "internal_error"));
    const { client, sleeps } = harness(replies, {
      random: () => 0.999,
      retry: { maxRetries: 4, baseDelayMs: 100, maxDelayMs: 300 },
    });
    await assert.rejects(client.getPaymentIntent(INTENT_ID), GatewayError);
    assert.deepEqual(sleeps, [99, 199, 299, 299]);
  });

  test("retry: false sends exactly once", async () => {
    const { client, calls } = harness([apiError(503, "storage_unavailable")], { retry: false });
    await assert.rejects(client.getCheckoutView(TOKEN), GatewayError);
    assert.equal(calls.length, 1);
  });

  test("4xx other than 408 and 429 is never retried", async () => {
    const { client, calls } = harness([apiError(409, "idempotency_conflict")]);
    await assert.rejects(client.createPaymentIntent(params, { idempotencyKey: KEY }), GatewayError);
    assert.equal(calls.length, 1);
  });

  test("a 2xx protocol error is not retried", async () => {
    const { client, calls } = harness([new Response("oops", { status: 200 })]);
    await assert.rejects(client.getPaymentIntent(INTENT_ID), GatewayProtocolError);
    assert.equal(calls.length, 1);
  });

  test("an abort during the retry wait stops the retries", async () => {
    const controller = new AbortController();
    const { client, calls } = harness([apiError(500, "internal_error")], {
      sleep: async () => {
        controller.abort();
        throw new Error("aborted");
      },
    });
    await assert.rejects(client.getPaymentIntent(INTENT_ID, { signal: controller.signal }), (error: unknown) => {
      return error instanceof GatewayNetworkError && error.kind === "aborted";
    });
    assert.equal(calls.length, 1);
  });
});
