# USDT (TRC20) for WooCommerce

A WooCommerce payment method for this gateway. The buyer is sent to the
gateway's hosted payment page, pays the exact USDT amount on TRON, and the
order is completed only when the gateway says the intent is `paid`: by a
signed webhook, or by a status read when a webhook was missed. The plugin
never holds a key and never moves funds; neither does the gateway.

Requirements: WordPress 6.4+, WooCommerce 8.0+, PHP 8.1+. Works with the
classic checkout and the Cart/Checkout Blocks, and with High-Performance
Order Storage (HPOS) on or off.

## Install

1. Copy `integrations/woocommerce/crypto-gateway-usdt/` into
   `wp-content/plugins/` (or zip that directory and upload it under
   Plugins > Add New > Upload). Leave out `vendor/` and `tests/`; the plugin
   has no runtime dependencies.
2. Activate **USDT (TRC20) for WooCommerce — self-hosted gateway**.
3. WooCommerce > Settings > Payments > **USDT (TRC20)**.

## Configure

You need four things from the gateway operator (see
[`docs/owner-setup.md`](../../../docs/owner-setup.md) and
[`docs/operator-runbook.md`](../../../docs/operator-runbook.md)):

| Setting | Where it comes from |
|---|---|
| Gateway base URL | The public address of `gateway-api`, e.g. `https://pay.example.com`. HTTPS is required; plain `http://` is accepted only for `localhost`. |
| Merchant API key | `gateway-worker admin api-key-issue --merchant <uuid>`; shown once. |
| USDT asset id | The asset UUID of USDT TRC20 on the operator's rail. |
| Webhook signing secret | `gateway-worker admin webhook-add`; shown once (64 hex characters). |

Register the store's webhook URL with the operator. The settings page shows
it; it is

```
https://<your store>/wp-json/crypto-gateway-usdt/v1/webhook
```

```sh
gateway-worker admin webhook-add --actor <you> --merchant <merchant uuid> \
  --url https://<your store>/wp-json/crypto-gateway-usdt/v1/webhook
gateway-worker admin webhook-test --actor <you> --endpoint <endpoint_id>
```

The gateway delivers only to `https` on port 443 at a public address, and it
never follows a redirect: register the final URL (with or without `www`, as
your site actually answers). If your site uses plain permalinks, the REST
URL is `https://<your store>/?rest_route=/crypto-gateway-usdt/v1/webhook`.
The test event is logged under WooCommerce > Status > Logs, source
`crypto-gateway-usdt`.

Other settings:

- **Priced store currencies.** The gateway converts the order amount with the
  operator's price evidence for that fiat currency. List only the currencies
  the operator publishes prices for (default `USD`). The method is hidden at
  checkout when the store currency is not listed, because every quote would
  be refused with `quote_unavailable`.
- **Order reference prefix.** The gateway reference is the prefix plus the
  order number (`wc-1042`); a reference is unique per merchant. If several
  stores share one merchant, give each a different prefix.
- **Previous webhook secret.** During a rotation (`webhook-rotate`), put the
  new secret in the first field and the old one here; deliveries carry a
  signature under each, and either verifies. Clear this field before the
  transition ends.
- **Status check after (minutes).** See "Missed webhooks" below.
- **Test network label.** Adds "(test network)" to the method title. Which
  network is used is decided by the operator's deployment, not by this box.

### Amounts

The order total is converted to integer minor units with the ISO 4217
exponent of the currency (2 for USD and EUR, 0 for JPY, 3 for KWD), using
string arithmetic only; a float never touches an amount. An order total with
more decimals than its currency has is refused, not rounded. The operator's
rate must be expressed per minor unit of that same exponent.

## What happens at checkout

1. The plugin creates a payment intent with the order total and a reference
   derived from the order number, then a quote in USDT. Both requests carry an
   `Idempotency-Key` derived from the order and an attempt counter that only
   advances after the gateway answered, so a retried checkout replays the same
   intent and quote instead of creating new ones.
2. The buyer is redirected to `{base URL}/checkout/{token}`, the gateway's
   hosted page with the exact amount, the address, the token contract and a
   live status.
3. The order stays **Pending payment** until the gateway reports it paid.
   The order is never marked paid by the checkout request itself.

The hosted page has no "return to the store" link. The order page (the
thank-you page and My account > Orders > View) shows an "Open the payment
page" button while the quote is live, and "Pay again" after it expired.

### An expired quote

A quote is valid for a limited time. When it expires unpaid, the order stays
Pending payment and "Pay again" asks the gateway for a new quote on the same
intent and reference (a new attempt, with a new exact amount). If the intent
cannot be quoted again (`payment_intent_not_quotable`: money, a hold or a
decision exists on it, or another attempt is live) the plugin reads the
intent, applies its status to the order, and tells the buyer to contact the
store instead of paying again.

If an administrator changes the order total after the intent was created,
the next payment attempt cancels the old intent at the gateway and creates a
new one with the reference suffix `-2`, `-3`, and so on; this is refused when
money or a decision already exists on the old intent.

## Order statuses

| Gateway fact | Order | Notes |
|---|---|---|
| Intent created, quote live (`awaiting_payment`) | Pending payment | The buyer can reopen the payment page. |
| Quote expired, no money (`expired`) | Pending payment | A note once per quote; "Pay again" re-quotes. |
| `payment_intent.paid` / intent `paid` | Processing or Completed (WooCommerce's `payment_complete`) | Exactly once. The transaction id is the TRON transaction hash when the latest attempt's page shows it, otherwise the intent id. |
| `payment_intent.partially_paid` / intent `partially_paid` | On hold | The operator accepted less than the amount. **Do not ship.** The remainder is still owed; resolve it with the operator. |
| `OVERPAID` | unchanged (already paid) | A note with the exact remainder. The operator records what happens to it; the gateway never refunds. |
| `payment_intent.cancelled` / intent `cancelled` | Cancelled, if not paid | A paid order is never changed; a note flags the conflict. |
| `webhook.test` | none | Answered `200`, logged. |
| Any other event type | none | Answered `200` and logged, so a type added to the gateway later is not retried forever. |

A gateway status the order has already recorded is not applied again, so an
administrator's later decision (reopening a cancelled order, resolving a
partial payment by hand) is not undone by a repeated webhook or status read.

Cancelling an order in WooCommerce cancels its intent at the gateway. The
gateway refuses when money, a hold or a decision exists on the intent; the
order then gets a note, and the case belongs to the operator. WooCommerce's
automatic cancellation of unpaid orders ("Hold stock") is postponed for these
orders until the quote's late-payment window has ended, because an exact
payment sent before then can still settle the intent.

## Webhook handling

- The signature is checked over the raw body before any JSON parsing:
  `HMAC-SHA256(hex_decode(secret), "<t>.<body>")`, every `v1` value against
  every configured secret, compared with `hash_equals`, and a timestamp more
  than 300 seconds from the store's clock is refused. A bad signature is
  answered `401`.
- A `payment_intent.paid` event is confirmed with a read of the intent before
  the order is completed.
- Events are deduplicated by their `id` (the last 100 per order), and every
  order change runs under a per-order lock shared with the status checks.
- `2xx` is answered only after the order is saved. A failure answers `5xx`,
  and the gateway redelivers the same event later.
- An event for an intent that belongs to no order of this store is answered
  `200` and logged.

### Missed webhooks

Every five minutes, WP-Cron reads the intent of up to 20 Pending payment or
On hold orders that are older than the configured delay (default 10 minutes)
and were not checked within it, newest first, and applies the same mapping
as the webhook. Orders older than 30 days, and expired intents whose
late-payment window has ended, are no longer polled. On a low-traffic site,
WP-Cron runs only on page views; trigger `wp-cron.php` from a system cron if
that is too slow.

The order screen has a **USDT payment** box with the intent id, status,
exact amount, network, token contract, address, quote times and a link to
the payment page, and a **Check status now** button that runs the same check
immediately.

## Test on Nile testnet

1. Run the gateway on Nile as in the repository [README](../../../README.md#a-real-nile-testnet-payment-on-your-own-address):
   a merchant on the `own` collector policy with your Nile address
   registered, price evidence for your store currency, rail health, and the
   workers running.
2. Expose the store over public HTTPS (a tunnel works), register the webhook
   URL, and send `webhook-test`. The log shows `CGUSDT_WEBHOOK_TEST`.
3. Configure the plugin, tick "Test network label", and place an order.
4. On the hosted page, send exactly the shown amount of Nile test USDT from a
   TronLink wallet on Nile to the shown address, on the shown token contract.
5. After finality the order moves to Processing, with the transaction hash as
   its transaction id. Without the webhook, "Check status now" or the
   five-minute job does the same.
6. Let a second order's quote expire, then use "Pay again" on the order page:
   a new quote is issued on the same intent.

## Limits

- **No refunds from the gateway.** The gateway never sends funds. Refund the
  buyer from the wallet that holds the collector key, and ask the operator
  to record the disposition. The plugin does not declare refund support.
- **Underpayment and overpayment are operator cases.** An amount that is not
  exact is not matched automatically. The operator may honour it; the order
  then moves to On hold (partial) or Processing (full, with an `OVERPAID`
  note).
- **Wrong network or token.** USDT sent on another chain or as another token
  is never seen by the gateway; recovery is outside the gateway.
- One store currency per order; the operator must price it.
- The hosted page formats the fiat amount with two decimals.

Full list of exceptional cases: [`docs/scope-and-limits.md`](../../../docs/scope-and-limits.md).

## Uninstall

Deleting the plugin removes its settings, its scheduled job and any leftover
order locks. Orders and their payment meta are kept.

## Development

```sh
# from this directory
docker run --rm -v "$PWD:/app" -w /app composer:2 composer install
docker run --rm -v "$PWD:/app" -w /app php:8.2-cli vendor/bin/phpunit
```

The unit tests cover the parts that decide money and access without
WordPress: signature verification (the gateway's fixed test vector, rotation,
tolerance, tampering, malformed headers), minor-unit conversion (0, 2, 3 and
4 decimals, large totals, refusals) and the event and status mapping on a
fake order.
