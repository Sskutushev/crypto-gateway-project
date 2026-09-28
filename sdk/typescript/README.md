# @crypto-gateway/sdk

A typed TypeScript client and webhook verifier for the self-hosted USDT
(TRC20) payment gateway in this repository.

- Zero runtime dependencies. Node.js 18 or later (global `fetch`, `node:crypto`).
- ESM and CommonJS builds, with type declarations.
- Every merchant route in [`docs/openapi.json`](../../docs/openapi.json):
  create, read and cancel a payment intent, quote it, and read the public
  checkout view.
- Amounts are decimal strings end to end. The SDK never turns money into a
  JavaScript `number`.
- Every failure throws a typed error. No call resolves to an empty or default
  value in place of an answer.

## Install

```sh
npm install @crypto-gateway/sdk
```

## Quickstart

Create an intent, quote it, show the payer the hosted checkout page, and
fulfil the order when the signed `payment_intent.paid` webhook arrives.

```ts
import { GatewayClient, formatTokenAmount, newIdempotencyKey } from "@crypto-gateway/sdk";

const gateway = new GatewayClient({
  baseUrl: "https://pay.example.com",
  apiKey: process.env.MERCHANT_KEY!, // gw_... from the operator
});

// Generate each key once per operation and store it with the order before the
// first call: a retry after a crash must reuse it to be a retry.
const createKey = newIdempotencyKey("order-1042");
const quoteKey = newIdempotencyKey("order-1042-quote");

const { data: intent } = await gateway.createPaymentIntent(
  { amount_minor: "4999", currency: "USD", reference: "order-1042" },
  { idempotencyKey: createKey },
);

const { data: quote } = await gateway.createQuote(
  intent.id,
  { asset_id: process.env.USDT_ASSET_ID! },
  { idempotencyKey: quoteKey },
);

// Send the payer to the hosted page, or render the exact amount yourself.
console.log(gateway.checkoutUrl(quote));
console.log(`Send exactly ${formatTokenAmount(quote.amount_raw, quote.asset.decimals)} ${quote.asset.symbol}`);
console.log(`to ${quote.collector_address} (token contract ${quote.asset.contract_address})`);
console.log(`before ${quote.expires_at}`);
```

Verify the webhook over the raw body, before any JSON parsing:

```ts
import express from "express";
import { verifyWebhook, WebhookVerificationError, UnknownWebhookEventError } from "@crypto-gateway/sdk";

const app = express();

app.post("/gateway/webhook", express.raw({ type: "application/json" }), (req, res) => {
  let event;
  try {
    event = verifyWebhook({
      payload: req.body, // the Buffer as received
      header: req.header("gateway-signature"),
      secrets: [process.env.WEBHOOK_SECRET!], // during a rotation: [new, previous]
    });
  } catch (error) {
    if (error instanceof UnknownWebhookEventError) {
      // Genuine, but newer than this SDK. Acknowledge so it is not retried.
      console.warn("unhandled gateway event", error.envelope.type, error.envelope.id);
      return res.sendStatus(204);
    }
    if (error instanceof WebhookVerificationError) {
      return res.status(400).send(error.reason);
    }
    throw error;
  }

  // Deduplicate by event.id: delivery is at least once.
  switch (event.type) {
    case "payment_intent.paid":
      // The only event to fulfil on.
      break;
    case "payment_intent.partially_paid":
      // An operator honoured an underpayment. Never fulfil on it.
      break;
    case "OVERPAID":
      // event.data.attributes.remainder_raw is the excess, in token units.
      break;
    case "payment_intent.cancelled":
    case "webhook.test":
      break;
  }
  res.sendStatus(204);
});
```

## API

### `new GatewayClient(options)`

| Option | Default | |
|---|---|---|
| `baseUrl` | required | The gateway's URL. `https` is required unless the host is localhost or `allowInsecureHttp` is set: the API key is in every request. |
| `apiKey` | required | The merchant key, 32 to 256 characters. |
| `fetch` | global `fetch` | Any `fetch`-compatible function. |
| `timeoutMs` | `30000` | Per attempt, including reading the body. Also settable per call. |
| `retry` | `{ maxRetries: 3 }` | See [Retries](#retries). `false` disables them. |

| Method | Route |
|---|---|
| `createPaymentIntent(params, { idempotencyKey })` | `POST /v1/payment-intents` |
| `getPaymentIntent(intentId)` | `GET /v1/payment-intents/{intent_id}` |
| `createQuote(intentId, { asset_id }, { idempotencyKey })` | `POST /v1/payment-intents/{intent_id}/quotes` |
| `cancelPaymentIntent(intentId, { reason? }, { idempotencyKey })` | `POST /v1/payment-intents/{intent_id}/cancel` |
| `getCheckoutView(checkoutToken)` | `GET /v1/checkout/{checkout_token}` (public, sends no key) |
| `checkoutUrl(quote)`, `checkoutQrUrl(quote)` | URLs of the hosted page and its QR code |

Writes resolve to `{ data, replayed, status }`. `replayed` is `true` when the
gateway returned the stored result of an earlier request with the same key
(status 200), `false` for a fresh one, and `null` if the response did not say.
Every call also accepts `{ signal, timeoutMs }`.

Every write requires an idempotency key of 16 to 128 characters of
`A-Z a-z 0-9 _ -`, checked before anything is sent. `newIdempotencyKey(prefix?)`
makes a random one; a key derived from your own order id works as well.

## Errors

All errors extend `GatewaySdkError`.

| Class | When | Fields |
|---|---|---|
| `GatewayError` | The gateway answered with a non-2xx status. | `status`, `code`, `message`, `retryAfterSeconds`, `requestId`, `body` |
| `GatewayNetworkError` | No response: connection failure (`kind: "network"`) or the caller's abort (`"aborted"`). | `kind` |
| `GatewayTimeoutError` | A `GatewayNetworkError` with `kind: "timeout"`. | `timeoutMs` |
| `GatewayProtocolError` | A 2xx whose body is not the promised JSON. | `status`, `body` |
| `TypeError` / `RangeError` | An argument the gateway would refuse; nothing was sent. | |

`code` is the API's error code, typed as `GatewayErrorCode`:

| Status | `code` | What to do |
|---|---|---|
| 400 | `invalid_request`, `invalid_idempotency_key` | Fix the request. |
| 401 | `authentication_failed` | Check the key; a revoked key is refused at once. |
| 404 | `payment_intent_not_found`, `checkout_not_found` | Another merchant's intent is also a 404. |
| 409 | `payment_intent_reference_conflict` | This order already has an intent. |
| 409 | `idempotency_conflict` | The same key was used with a different body. |
| 409 | `payment_intent_not_quotable`, `payment_intent_not_cancellable` | The intent is not in a state that allows it. |
| 422 | `invalid_request` | The amount is not a positive integer string, the currency not a code, or the reason too long. |
| 503 | `quote_unavailable`, `rail_stopped` | No safe quote now. Retry later with the same key. |
| 500 / 503 | `internal_error`, `storage_unavailable` | Retry with the same key. |

`code` is `null` when the body carried no error envelope (a proxy's error
page, the server's request timeout), and holds a code this version does not
know verbatim; `error.isKnownCode` tells them apart.

A network error or a timeout on a write means the outcome is unknown, not
that it failed. Repeat the call with the same idempotency key: the gateway
answers with the stored result if the first attempt went through.

```ts
import { GatewayError, GatewayNetworkError } from "@crypto-gateway/sdk";

try {
  await gateway.createQuote(intent.id, { asset_id }, { idempotencyKey: quoteKey });
} catch (error) {
  if (error instanceof GatewayError && error.code === "quote_unavailable") {
    // Try again later, after error.retryAfterSeconds if it is set.
  } else if (error instanceof GatewayNetworkError) {
    // Unknown outcome: retry later with the same quoteKey.
  } else {
    throw error;
  }
}
```

## Retries

Only requests the gateway recognises as repeats are retried: a `GET`, or a
write carrying its idempotency key (every write does). They are retried on a
network error or timeout, and on `408`, `429` and `5xx`, up to `maxRetries`
times (default 3), with capped exponential backoff and full jitter
(`baseDelayMs` 500, `maxDelayMs` 8000). A `Retry-After` header, in seconds or
as an HTTP date, replaces the backoff; one longer than `maxRetryAfterMs`
(default 60 s) is not slept through, and the error is thrown with
`retryAfterSeconds` set. Other `4xx` statuses, a malformed 2xx and a caller's
abort are never retried.

## Money

- Every amount is a decimal string of integer units: `amount_minor` and
  `minor_units` in fiat minor units, `amount_raw` in the token's smallest
  unit. Passing a `number` is refused.
- Pay `amount_raw` exactly. It is the match key: a payment of any other
  amount is not credited to the quote.
- `formatTokenAmount(raw, decimals)` renders an integer amount exactly:
  `formatTokenAmount("4999000", 6)` is `"4.999"`. It never rounds and drops
  trailing zeros, like the gateway's own `amount` field.
- `parseMinorUnits(amount, decimals)` goes the other way:
  `parseMinorUnits("49.99", 2)` is `"4999"`. More fractional digits than
  `decimals` allow is an error, not a rounding.
- Both use `BigInt`, so amounts beyond 2^53 (routine for 18-decimal tokens)
  are exact.

## Webhooks

`verifyWebhook({ payload, header, secrets, toleranceSeconds = 300, now? })`
implements the scheme in
[`docs/merchant-integration.md`](../../docs/merchant-integration.md):
`HMAC-SHA256(hex_decode(secret), "<t>." + body)`, header
`Gateway-Signature: t=<unix>,v1=<hex>[,v1=<hex>]`. It compares every `v1`
value against every secret in constant time, so a secret rotation works by
listing the new and the previous secret. It returns a typed `WebhookEvent`:

| `type` | `data.object` | `data.attributes` |
|---|---|---|
| `payment_intent.paid` | `payment_intent` | `payment_intent_id`, `transfer_id`, `attempt_id` |
| `payment_intent.partially_paid` | `payment_intent` | `payment_intent_id`, `transfer_id` |
| `payment_intent.cancelled` | `payment_intent` | `payment_intent_id`, `reason` (string or null) |
| `OVERPAID` | `payment_intent` | `payment_intent_id`, `transfer_id`, `remainder_raw` |
| `webhook.test` | `webhook_endpoint` | `endpoint_id`, `note` |

It throws `WebhookVerificationError` with a `reason` of `missing_header`,
`malformed_header`, `timestamp_outside_tolerance`, `signature_mismatch` or
`invalid_payload`, and `UnknownWebhookEventError` for a correctly signed event
of a type this version does not know. A receiver configured with no secret or
a secret that is not 64 hex characters gets a `TypeError`.
`signWebhookPayload` produces a header the same way the gateway does, for
testing your own receiver.

## Development

```sh
npm ci
npm run build   # dist/esm and dist/cjs
npm test        # builds, compiles the tests, runs node:test
```

The webhook tests use the signature vectors pinned by the gateway's Rust
tests, so a change to the scheme on either side fails here.

## License

Apache-2.0, like the rest of the repository.