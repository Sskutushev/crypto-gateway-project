# Merchant integration

This is the whole contract between a merchant's system and the gateway: three
HTTP routes, one webhook, and the rules that make them safe to retry. The
exact shapes are in [`openapi.json`](openapi.json). Working code to start
from is in [`examples/`](../examples/): a create-payment script in shell and
TypeScript, webhook receivers in TypeScript and Python with tests, and a
Postman collection for every route.

## Credentials

A merchant API key is a bearer token of 32 to 256 characters (keys issued by
the admin CLI look like `gw_` followed by 64 hex characters), issued once and
stored hashed; the gateway cannot show it again. If a key leaks, ask the
operator to revoke it and issue a new one; a revoked key is refused at once.
Every merchant route is isolated by the key's merchant: another merchant's
intent is a 404, not a 403, because its existence is not the caller's
business.

```
Authorization: Bearer <merchant key>
```

## Who owns the receiving address

Every quote names a collector address, and whose address that is depends on
the merchant's collector policy, set by the operator when the merchant is
created:

- **`own`** (the default for new merchants): quotes use only addresses
  registered to your merchant. You hold the key; the payer pays you directly
  and the gateway only watches the address. It can never move the funds.
- **`shared`**: quotes use only the operator's addresses. The payer pays the
  operator, and the operator owes you the money under an agreement outside
  this system.

There is no fallback between the two. On `own`, until you have an active
address registered, every quote is `503 quote_unavailable`; it is never
issued on someone else's address.

To register an address on `own`, you prove you control it:

1. The operator sends you a statement to sign. It names the gateway purpose,
   your merchant id, the address and the time it was issued:

   ```
   Self-hosted payment gateway: collector ownership
   merchant: <your merchant uuid>
   address: <T...>
   issued: <RFC 3339 time>
   ```

2. Sign that text exactly, unchanged, with the wallet that holds the address,
   in TronLink (`signMessageV2`, TIP-191). Send back the hex signature.
3. The operator registers the address within 24 hours of `issued`. The
   gateway recovers the signer from your signature and compares it with the
   address; a signature from another wallet, for another merchant or address,
   or older than 24 hours is refused.

Keep the key of that address in your own custody. Money sent to it is yours
the moment it confirms, whatever the gateway records.

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
address, for a bounded time. The quote route accepts an intent in
`requires_quote`, or an `expired` one that no money, hold or decision is
attached to (see "After `expires_at`").

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
| `amount_raw` | Exactly how much, in the token's smallest unit. Not a unit more or less. |
| `expires_at` | After this the quote is no longer offered to payers. |
| `late_payment_until` | Until this, a payment of exactly `amount_raw` is still recorded against the quote, for an operator to honour. |

`amount_raw` is in the token's smallest unit: for USDT on TRON (6 decimals),
`4999000` is 4.999 USDT. The quote also names the asset (`asset.chain`,
`asset.network`, `asset.symbol`, `asset.decimals`, `asset.contract_address`)
and gives the same amount in whole tokens as an exact string (`amount`),
and it carries
a `checkout_token`: send the payer to `/checkout/{checkout_token}` for the
hosted payment page, or show the address, amount, network and contract
yourself. Never round the amount for display.

The amount is exact because it is the match key: the gateway reserves this
amount at this collector for this obligation, and no other open reservation on
the collector has the same amount. Show the payer the exact amount and the
address.

The rate always rounds up and is never revisited. A replay of the same
idempotency key returns the same quote even while pricing is down.

**After `expires_at`.** The intent becomes `expired` (unless money already
arrived on it; see the status table). To let the payer try again, quote the
same intent again with a new idempotency key: a new attempt is issued on the
same intent and reference, and the earlier attempt keeps its amount reserved
until `late_payment_until`, so a late payment to the old amount is still
recognised. Once money, a hold or an operator decision is attached, a new
quote is `409 payment_intent_not_quotable`. Only one attempt is live at a
time.

## 3. Read the intent

```
GET /v1/payment-intents/{intent_id}
```

| Status | Meaning | Final? |
|---|---|---|
| `requires_quote` | Created, no quote yet. | no |
| `awaiting_payment` | A quote is live; the exact amount is reserved. | no |
| `partially_paid` | An operator accepted less than the amount; the remainder is still owed. It keeps this status after the quote window closes. | no |
| `risk_hold` | Reserved for a payment a person must decide. The current code does not set it: a held payment leaves the intent in its current status. | no |
| `paid` | Settled: independent chain evidence, allocated exactly once, webhook queued. | yes |
| `expired` | No quote was paid in time. Quote it again to let the payer retry. An operator may still honour an exact payment sent before `late_payment_until`, which moves it to `paid`. | see note |
| `cancelled` | Closed without payment by `POST /v1/payment-intents/{id}/cancel`, allowed only while no money, hold or decision is attached. Money that still arrives is held for a person. | yes |

Polling is fine; the webhook is faster.

## 4. The webhook

Events are written to an outbox in the same transaction as the change they
announce, and a worker delivers each one to every active endpoint of the
merchant over HTTPS.

| `type` | When |
|---|---|
| `payment_intent.paid` | The intent is settled. Fulfil the order on this event only. |
| `payment_intent.partially_paid` | An operator honoured an underpayment. Never fulfil on it. |
| `payment_intent.cancelled` | You cancelled the intent. Written in the same transaction as the cancellation. |
| `OVERPAID` | A payment exceeded the amount; `attributes.remainder_raw` names the excess. Written together with `payment_intent.paid`. |
| `webhook.test` | Sent by the operator (`webhook-test`) to check delivery and your signature verification. Not a payment; answer `2xx` and do nothing else. |

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

For `webhook.test`, `data.object` is `webhook_endpoint`, `data.id` is the
endpoint id, and `attributes` carries `endpoint_id` and a note.

Delivery is at least once with capped exponential backoff, and an endpoint
that never answers `2xx` sees the event dead-lettered where the operator can
find it. Deduplicate by `id`: it is stable across retries.

The endpoint URL must be `https` on port 443, with no credentials, query or
fragment, and resolve only to public addresses; the operator's registration
refuses anything else.

### Verifying the signature

Each endpoint has a signing secret handed to you once at registration: 64 hex
characters. The HMAC key is the **hex-decoded bytes** of that string (32
bytes), not the string itself. The signature signs the timestamp and the raw
body together:

```
key      = hex_decode(secret)
signed   = "<t>" + "." + <raw request body bytes>
expected = hex(HMAC-SHA256(key, signed))
```

The header carries one timestamp and one or more `v1` values:

```
Gateway-Signature: t=1758700000,v1=<hex>
Gateway-Signature: t=1758700000,v1=<hex under the new secret>,v1=<hex under the previous secret>
```

1. Parse `Gateway-Signature` into `t` and every `v1` value. Do not assume
   there is exactly one.
2. Refuse if `t` is more than five minutes from your clock: a captured
   delivery cannot be replayed later.
3. Compute `expected` over the raw bytes, before any JSON parsing, for each
   secret you currently hold.
4. Accept if any `v1` equals any `expected`, comparing in constant time.
5. Only then parse the body and act on `id`.

The secret is derived from the deployment's master key and the endpoint; the
gateway stores only its fingerprint, so a stolen database cannot forge an
event.

### When the secret rotates

The operator can rotate one endpoint's secret without touching anyone else's.
You receive the new secret once. For the transition period the operator
chose (72 hours unless they said otherwise, at most 30 days), every delivery
carries two `v1` values: the first under the new secret, the second under
the previous one. So:

1. Add the new secret to your verifier next to the old one; a receiver that
   accepts any matching `v1` keeps working throughout.
2. Ask the operator for a `webhook.test` event and confirm it verifies.
3. Remove the old secret before the transition ends. After it, deliveries
   carry only the new signature.

The receivers in [`examples/`](../examples/) accept several local secrets and
several `v1` values out of the box.

### Following a redirect

The gateway never follows one. A `3xx` from your endpoint is a refused
delivery, because a redirect would send a signed event to an address you never
registered.

## Idempotency

Every write carries `Idempotency-Key`: 16 to 128 URL-safe characters
(`A-Z a-z 0-9 _ -`), scoped to your merchant, the route and the key. The first
request's result is stored; a repeat with the same key and the same body
returns it with `Idempotent-Replayed: true` and status `200`; a repeat with a
different body is `409 idempotency_conflict`. Retry any write freely with the
same key.

## What the gateway refuses, and why

| Response | Why |
|---|---|
| `503 quote_unavailable` | No fresh price, policy or rail-health evidence, no free amount slot on any of your addresses, or (on `own`) no active address of yours for the asset. The gateway does not invent a rate or borrow an address. Retry later; a replay of an issued quote still works. |
| `503 quote_capacity_exhausted` | Every address of yours holds the operator's maximum of open reservations. Wait `Retry-After` seconds. |
| `429 rate_limited` | Your request budget (writes and reads counted separately), or your address's budget of failed authentications, is spent. Wait `Retry-After` seconds; retry writes with the same idempotency key. |
| `413` | The request body is larger than 64 KiB. |
| `503 rail_stopped` | A person, or the reconciler finding money that does not add up, closed the rail. Issued quotes stay payable. |
| `409 payment_intent_not_quotable` | A quote is already live, or money, a hold or a decision is attached, or the intent is paid or cancelled. |
| `409 payment_intent_not_cancellable` | Money, a hold or a decision is attached, or the intent is already closed. |
| `422 invalid_request` | The amount is not a positive integer string, the currency is not a code, or `metadata` is not an object of at most 16 KiB serialized. |

Money that arrives but matches no reservation exactly is never absorbed into an
intent: it is recorded as unmatched and put in front of the operator. Tell
payers the amount is exact. What happens to underpayments, overpayments, late
payments and payments on the wrong token or network is in
[`scope-and-limits.md`](scope-and-limits.md).
