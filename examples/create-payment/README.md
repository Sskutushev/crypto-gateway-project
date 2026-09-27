# Create a payment

Two versions of the same merchant flow: `create-payment.sh` (curl and jq) and
`create-payment.ts` (Node 22.6+, `fetch`, no dependencies).

1. `POST /v1/payment-intents` with an `Idempotency-Key`.
2. `POST /v1/payment-intents/{id}/quotes` with an `Idempotency-Key`.
3. Show the payer `collector_address` and the exact amount.
4. Poll `GET /v1/payment-intents/{id}` until `paid`, or until `expired` after
   `late_payment_until`.

```sh
export GATEWAY_URL=https://gateway.example.com MERCHANT_KEY=... ASSET_ID=<uuid>
export ASSET_DECIMALS=6 ORDER_REF=order-1042 AMOUNT_MINOR=4999 CURRENCY=USD
./create-payment.sh
node --experimental-strip-types create-payment.ts
```

## Idempotency and safe retries

- A key is 16 to 128 characters of `A-Z a-z 0-9 _ -`, scoped to the merchant,
  the route and the key.
- Both examples derive the key from a stable id: the intent key from the order
  reference, the quote key from the intent id. A crash and a re-run send the
  same key with the same body, and the gateway answers `200` with
  `Idempotent-Replayed: true` and the first result.
- Retry network failures, `429` and `5xx` with the same key and body.
  `503 quote_unavailable` means no fresh price or rail evidence, or no free
  amount slot; retry later with the same key.
- Do not retry a `4xx`. `409 idempotency_conflict` means the same key was sent
  with a different body. `409 payment_intent_reference_conflict` means the
  reference is taken under another key. `409 payment_intent_not_quotable` means
  the intent already has a quote or is closed.
- There is no route that reads a quote back. Replaying the quote request with
  the same key is how you get the quote again, so keep that key.

## Amounts

`amount_minor` is sent as a string of fiat minor units (`"4999"` is 49.99 USD).
`amount_raw` in the quote is a string of the token's smallest unit. The quote
does not carry the token's decimals; USDT TRC20 has 6, and the value should
come from the operator's asset configuration. Both examples convert with
string arithmetic only, never floating point, and do not round:
`"49990000"` with 6 decimals is `49.99`.

The payer must send exactly `amount_raw`. On TRON the gateway matches by exact
amount at the collector; any other amount is queued for the operator and does
not move the intent by itself.

## After the quote

- `expires_at`: after this the attempt and intent become `expired`. A new
  quote on the same intent is refused; create a new intent with a new
  reference.
- `late_payment_until`: an exact payment sent after `expires_at` but before
  this is not settled automatically; it waits for an operator, who may honour
  it.
- The signed webhook `payment_intent.paid` is the signal to fulfil; polling is
  a fallback. See `../webhook-receiver-typescript`.
