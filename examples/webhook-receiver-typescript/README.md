# Webhook receiver (TypeScript, Node)

A minimal receiver for the gateway's signed webhooks, with no framework and no
runtime dependencies: `node:http` and `node:crypto` only.

What it does, in order:

1. Reads the raw request body (capped at 1 MiB) and verifies
   `Gateway-Signature` against those exact bytes, before any JSON parsing.
2. Refuses a timestamp more than `WEBHOOK_TOLERANCE_SECONDS` (default 300)
   away from the local clock, in either direction.
3. Accepts a header with several `v1=` values, and several local secrets;
   the delivery is valid if any pair matches. Comparison is constant time.
4. Checks that `Gateway-Event-Id` equals the envelope `id`.
5. Deduplicates by event id, runs the handler, and answers `2xx` only after
   the handler has finished.
6. Fulfils an order only on `payment_intent.paid`.

## Run

Node 22.6 or later runs the TypeScript files directly (type stripping):

```sh
WEBHOOK_SECRETS=<hex secret from the operator> PORT=8080 npm start
npm test
```

On Node 20, compile first: `npm i -D typescript @types/node && npm run build`,
then run `node dist/server.js`.

| Variable | Default | Meaning |
|---|---|---|
| `WEBHOOK_SECRETS` | required | Comma-separated hex secrets, newest first. |
| `WEBHOOK_TOLERANCE_SECONDS` | `300` | Replay window. |
| `WEBHOOK_PATH` | `/webhooks/gateway` | The only path that accepts events. |
| `PORT` | `8080` | Listen port. |

The gateway delivers only to `https://` URLs on port 443 that resolve to
public addresses. Put this process behind a TLS-terminating proxy.

## The wire format

```
POST <endpoint>
Content-Type: application/json
Gateway-Event-Id: <event uuid>
Gateway-Signature: t=<unix seconds>,v1=<64 lowercase hex>
```

```
signed   = "<t>" + "." + <raw body bytes>
key      = hex_decode(secret)          # 32 bytes, not the 64-character text
v1       = hex(HMAC-SHA256(key, signed))
```

The body is `{"id", "type", "created_at", "data": {"object", "id",
"attributes"}}`, where `created_at` is Unix seconds. Webhook event types the
current code writes:

| `type` | When | `data.attributes` |
|---|---|---|
| `payment_intent.paid` | The intent became `paid`. At most once per intent. | `payment_intent_id`, `transfer_id`, `attempt_id` |
| `payment_intent.partially_paid` | Money was allocated but the amount is not covered. | `payment_intent_id`, `transfer_id` |
| `OVERPAID` | Written in the same transaction as `payment_intent.paid` when more arrived than was owed. | `payment_intent_id`, `transfer_id`, `remainder_raw` |

Unknown types are acknowledged and kept for review, so a new type is not
retried until it is dead-lettered.

## Delivery behaviour to design for

- At least once. An event goes to every active endpoint of the merchant and
  counts as delivered only when all of them answer `2xx`; otherwise the whole
  event is retried, so an endpoint that already accepted it sees it again.
- Request timeout 10 s by default (`GATEWAY_WEBHOOK_REQUEST_TIMEOUT_SECONDS`).
  Retries back off from 60 s, doubling per attempt, up to 7680 s; after 12
  attempts by default (`GATEWAY_WEBHOOK_MAX_ATTEMPTS`) the event is
  dead-lettered for the operator.
- Redirects are never followed; a `3xx` is a refused delivery.
- Answer quickly. Long work belongs in your own queue, written durably before
  you answer `2xx`.

## Key rotation

The gateway derives one secret per endpoint and key version. Outside a
rotation a delivery carries one `v1=` value. During a rotation's transition
period (migration `0016_webhook_key_ring.sql`) the gateway signs each delivery
with the new and the previous secret, `t=<t>,v1=<new>,v1=<previous>`, until
the previous secret's deadline. A delivery is valid when any `v1=` value
verifies under any secret you hold.

When you receive a new secret, put it first in `WEBHOOK_SECRETS` and keep the
previous one until the transition period ends, then remove it.

## Production notes

- `InMemoryProcessedEvents` is lost on restart and not shared between
  instances. Persist processed event ids in your database, under a unique
  constraint, in the same transaction as the business effect.
- Key fulfilment by `data.id` (the payment intent) as well as by event id.
- A failed signature returns `400` and is logged with a reason; it is never
  answered `2xx`.

## Test vector

`receiver.test.ts` pins a vector computed exactly as
`crates/gateway-domain/src/webhook.rs` does: master key of 32 bytes `0x07`,
merchant `Uuid::from_u128(1)`, endpoint `Uuid::from_u128(2)`, key version 1.
That gives the secret pinned by the Rust test
`the_signature_is_stable_across_releases`. The timestamp is `1700000000` and
the body is the envelope `serde_json` emits for a `payment_intent.paid` event
(keys sorted, no whitespace). A second secret, for key version 2, exercises
rotation. The Python example uses the same vector.
