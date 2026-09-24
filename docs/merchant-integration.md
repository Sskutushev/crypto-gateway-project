# Merchant integration

This is the whole contract between a merchant's system and the gateway: three
HTTP routes, one webhook, and the rules that make them safe to retry. The
exact shapes are in [`openapi.json`](openapi.json).

## Credentials

A merchant API key is a bearer token of 32 to 256 characters, issued once and
stored hashed; the gateway cannot show it again. Every merchant route is
isolated by the key's merchant: another merchant's intent is a 404, not a 403,
because its existence is not the caller's business.

```
Authorization: Bearer <merchant key>
```

## 1. Create a payment intent

An intent is the obligation: an amount in a fiat currency and the merchant's
own reference. Amounts are decimal strings of minor units; a JSON number is
refused.

```
POST /v1/payment-intents
Idempotency-Key: order-1042-attempt-1
Content-Type: application/json

{"amount_minor": "4999", "currency": "USD", "reference": "order-1042"}
```

`201` carries the intent in `requires_quote`. The reference is unique per
merchant: a second intent for the same order is a `409
payment_intent_reference_conflict`, which is the gateway refusing to let one
order be paid twice.

## 2. Quote it in an asset

A quote converts the obligation into an exact token amount at one collector
address, for a bounded time.

```
POST /v1/payment-intents/{intent_id}/quotes
Idempotency-Key: order-1042-quote-1
Content-Type: application/json

{"asset_id": "<the USDT asset id the operator gave you>"}
```

`201` returns the quote. The fields a payer needs:

| Field | Meaning |
|---|---|
| `collector_address` | Where to pay. Display form; compare nothing by string. |
| `amount_raw` | Exactly how much, in the token's smallest unit. Not a cent more or less. |
| `expires_at` | After this the quote is no longer offered; ask for a new one. |
| `late_payment_until` | Until this, a payment of exactly `amount_raw` is still honoured. |

The amount is exact because it is the match key: the gateway reserves this
amount at this collector for this obligation, and no other open reservation on
the collector has the same amount. Show the payer the exact amount and the
address; do not round for display.

The rate always rounds up and is never revisited. A replay of the same
idempotency key returns the same quote even while pricing is down.

## 3. Read the intent

```
GET /v1/payment-intents/{intent_id}
```

| Status | Meaning | Final? |
|---|---|---|
| `requires_quote` | Created, no quote yet. | no |
| `awaiting_payment` | A quote is live; the exact amount is reserved. | no |
| `partially_paid` | Less than the amount arrived; the remainder is still owed. | no |
| `risk_hold` | Money arrived and a person must decide. | no |
| `paid` | Settled: independent chain evidence, allocated exactly once, webhook queued. | yes |
| `expired` | No quote was paid in time. Create a new intent. | yes |
| `cancelled` | Closed without payment. | yes |

Polling is fine; the webhook is faster.

## 4. The webhook

When an intent settles, the gateway writes `payment_intent.paid` to its outbox
in the same transaction as the money, and a worker delivers it to every active
endpoint of the merchant over HTTPS.

```
POST <your endpoint>
Content-Type: application/json
Gateway-Event-Id: <uuid>
Gateway-Signature: t=<unix seconds>,v1=<hex hmac>

{
  "id": "<event uuid>",
  "type": "payment_intent.paid",
  "created_at": 1758700000,
  "data": {
    "object": "payment_intent",
    "id": "<intent uuid>",
    "attributes": {
      "payment_intent_id": "<intent uuid>",
      "transfer_id": "<transfer uuid>",
      "attempt_id": "<attempt uuid>"
    }
  }
}
```

Delivery is at least once with capped exponential backoff, and an endpoint
that never answers `2xx` sees the event dead-lettered where the operator can
find it. Deduplicate by `id`: it is stable across retries.

### Verifying the signature

Each endpoint has a secret handed to you once at registration, hex encoded.
The signature line signs the timestamp and the raw body together:

```
signed = "<t>" + "." + <raw request body bytes>
expected = hex(HMAC-SHA256(secret, signed))
```

1. Parse `Gateway-Signature` into `t` and `v1`.
2. Refuse if `t` is more than five minutes from your clock: a captured
   delivery cannot be replayed later.
3. Compute `expected` over the raw bytes, before any JSON parsing.
4. Compare with `v1` in constant time.
5. Only then parse the body and act on `id`.

The secret is derived from the deployment's master key and the endpoint; the
gateway stores only its fingerprint, so a stolen database cannot forge an
event. If the deployment rotates its master key, you receive a new secret and
the old one stops verifying at the moment the operator says.

### Following a redirect

The gateway never follows one. A `3xx` from your endpoint is a refused
delivery, because a redirect would send a signed event to an address you never
registered.

## Idempotency

Every write carries `Idempotency-Key`: 16 to 128 URL-safe characters
(`A-Z a-z 0-9 _ -`), scoped to your merchant, the route and the key. The first request's result is stored; a repeat with the same key and
the same body returns it with `Idempotent-Replayed: true` and status `200`; a
repeat with a different body is `409 idempotency_conflict`. Retry any write
freely with the same key.

## What the gateway refuses, and why

| Response | Why |
|---|---|
| `503 quote_unavailable` | No fresh price, policy or rail-health evidence, or no free amount slot. The gateway does not invent a rate. Retry later; a replay of an issued quote still works. |
| `503 rail_stopped` | A person, or the reconciler finding money that does not add up, closed the rail. Issued quotes stay payable. |
| `409 payment_intent_not_quotable` | The intent is already quoted, paid or closed. |
| `422 invalid_request` | The amount is not a positive integer string or the currency is not a code. |

Money that arrives but matches no reservation exactly is never absorbed into an
intent: it is recorded as unmatched and put in front of the operator. Tell
payers the amount is exact.
