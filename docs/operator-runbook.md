# Operator runbook

The operator feeds the gateway the evidence it will not invent, reads what it
found, onboards merchants, and is the only party that can reopen a rail.
Every HTTP route below needs an operator key; the scope each one needs is in
[`openapi.json`](openapi.json). Onboarding and rotation run through the admin
CLI, `gateway-worker admin <command>`, connected as the `gateway_provisioner`
database role.

## Keys and scopes

| Scope | Lets a key | Give it to |
|---|---|---|
| `ingest` | submit price readings and rail health | pricing and rail-health feeders |
| `risk_ingest` | submit current screening decisions for one DB-bound provider | one KYT integration credential per provider |
| `read` | read the overview, every queue, evidence bundles, pending honor proposals, the accounting export, `/metrics` | people, dashboards, Prometheus, bookkeeping |
| `admin` | close and reopen a rail, resolve parked money, propose, approve or reject an honor, redeliver a webhook | a person, never a job; two people for honors above the threshold |

A key carries only the scopes its holder needs. Prometheus gets a `read` key.
An `ingest` key cannot submit screening decisions. A `risk_ingest` key must
also have an active row in `operator_risk_provider_bindings`; the provider in
the request must match that row, and stale or future-dated evidence is refused.

## Feeding prices

A quote needs a price snapshot younger than the quote policy allows
(`max_price_age_seconds`), so a feeder runs continuously. Submit readings from
at least two independent provider groups per asset and currency:

```
POST /v1/operator/price-snapshots
{
  "asset_id": "...", "fiat_currency": "USD",
  "readings": [
    {"source_key": "vendor-a", "provider_group": "vendor-a",
     "rate_numerator": "10000", "rate_denominator": "1",
     "observed_at": "2026-09-24T10:00:00Z"},
    {"source_key": "vendor-b", "provider_group": "vendor-b",
     "rate_numerator": "10010", "rate_denominator": "1",
     "observed_at": "2026-09-24T10:00:01Z"}
  ]
}
```

A rate is a ratio of two integers, `rate_numerator / rate_denominator`, in
raw token units per fiat minor unit: a quote's `amount_raw` is
`minor_units × numerator / denominator`, rounded up. For a 6-decimal
stablecoin at 1:1 with the dollar, one cent is 10 000 raw units, so the
reading is `10000 / 1`; at 0.9985 it is `9985 / 1`. The service takes the
exact rational mean of the two middle readings, refuses when fewer groups
than the policy demands agree or when readings sit further apart than
`max_price_deviation_bps`, and stores every reading either way with the
reason it did not count. A snapshot is as old as its oldest reading.

`422 price_not_agreed` is the feed telling you a vendor drifted; look at the
readings before trusting either of them again.

## Rail health

```
POST /v1/operator/rail-health
{"asset_id": "...", "health": "healthy", "detail": null}
```

`degraded` and `unavailable` close new quotes on the asset; issued quotes stay
payable. Feed it from whatever watches the chain providers, on the same
cadence as prices.

## Closing and reopening a rail

```
POST /v1/operator/rail-stops
{"asset_id": "...", "reason_code": "provider_incident", "detail": "..."}

POST /v1/operator/rail-stops/{asset_id}/clear
{"reason": "provider restored; discrepancy 7f3a explained: duplicate feed"}
```

One open stop per asset; a second `open` returns the first. A stop opened by
the reconciler carries `reason_code = reconciliation_money_discrepancy` and the
discrepancy kind in `detail`. Clearing needs a reason; it is recorded with your
key.

## Reading the gateway

`GET /v1/operator/overview` is one page: every component's state and since
when, open rail stops, the latest reconciliation runs, open discrepancies by
kind, transfers by processing state, intents by status, the outbox backlog and
dead letters, open observation conflicts.

The queues, each a keyset page (`limit`, `before`, `next_before`):

| Route | What is waiting there |
|---|---|
| `/v1/operator/conflicts` | Sources disagreed about a chain event. No fact was made; decide which source lied. |
| `/v1/operator/unmatched-transfers` | Money that matched no reservation exactly. Refund, credit or match by hand. |
| `/v1/operator/held-payments` | Settlement bands wanted a person: amount over the auto-settle tier, risk review, ambiguity, a late payment, or a payment whose intent stopped being payable. |
| `/v1/operator/dead-letters` | Events no endpoint accepted after every retry, with the delivery history. |
| `/v1/operator/reconciliation/runs` | Every run, clean or not. |
| `/v1/operator/reconciliation/discrepancies` | Findings nobody has resolved. |
| `/v1/operator/manual-honor-proposals` | Honors one operator proposed and a second has not yet approved or rejected (see "Two operators for an honor"). Expired proposals are not listed. |

`GET /v1/operator/payment-intents/{id}` is the evidence bundle for one payment,
across merchants: the obligation, quotes and reservations, allocations, the
settlement decision with its policy, groups and risk verdict, the fulfilment
claim, payment events and the canonical transfers with their attestation
counts. It is what you read during a dispute.

### Accounting export

`GET /v1/operator/accounting/settlements?from=YYYY-MM-DD&to=YYYY-MM-DD`
(`read` scope; optional `merchant_id`) returns settled money per merchant,
asset, fiat currency and UTC day, a total per asset and currency, and a
control sum. Add `format=csv` or send `Accept: text/csv` for a CSV download.
The range is inclusive, at most 92 days; an answer over 20000 rows is refused
with `export_too_large` rather than cut short.

What the numbers are:

- The basis is `payment_settlement_decisions` with outcome `settled`,
  `overpaid` or `partial`, on the UTC day of `decided_at`: the same rows
  reconciliation checks against fulfilment. `held`, `manual_required`,
  `rejected` and the others moved no money and are not counted.
- `payments` counts obligations completed (`settled` or `overpaid`), and
  `fiat_minor` sums their invoiced amounts, once each. A `partial` allocation
  adds raw units to `allocated_raw` and one to `partial_payments`, no fiat.
- `remainder_raw` is the overpayment recorded on `overpaid` decisions. It was
  never absorbed; its disposition is a separate, external record.
- `control` sums the `payment_allocations` rows behind the same decisions per
  asset, independently. `balanced: false` means the allocation rows and the
  decisions disagree: treat it as a reconciliation finding, not rounding.

Rows are ordered by day, merchant, asset and currency. In CSV every row has a
`row_type` (`day`, `total`, `control`); a text cell that begins with `=`, `+`,
`-` or `@` is prefixed with `'` so a spreadsheet shows it instead of running
it.

## Resolving parked money

`POST /v1/operator/manual-resolutions` requires `admin`, a 16–128 character
`Idempotency-Key`, and a recorded reason. The accepted actions are deliberately
narrow:

- `honor` assigns one finalized `held`/`unmatched` transfer to an existing
  intent and attempt. `allocate_raw` must equal the exact safe allocation
  `min(outstanding obligation, unallocated transfer)`; the normal claim,
  allocation, fulfilment, event and outbox transaction is reused. The binding
  is read from locked rows, never from the request: the transfer must be at
  the attempt's own collector, in its quote's asset, on its chain, network
  and environment; the attempt must be `awaiting_payment` or `expired`; the
  intent must be `awaiting_payment`, `partially_paid`, `risk_hold` or
  `expired`; and the transfer's block time must be before the attempt's
  `late_payment_until`. Anything else is a `409` and nothing is
  written. A paid or cancelled intent is final.
- `reject` closes a finalized held/unmatched transfer only when no allocation
  exists.
- `record_remainder_disposition` records how an overpayment remainder was
  handled outside the gateway. It requires the exact remainder and an external
  reference; it never claims the gateway sent a refund because the gateway has
  no wallet key.

An identical replay returns the first result. Reusing the key for a different
command returns `idempotency_conflict`. Never repair these states with direct
SQL updates.

### Two operators for an honor

An honor pays an intent and tells the merchant. The API reads
`GATEWAY_MANUAL_HONOR_DUAL_CONTROL_MIN_RAW` at start-up: an honor allocating at
least that many raw units of the transfer's asset needs two different operator
keys. **Unset means every honor needs two keys**; a value that is not a whole
number stops the API. To let one person honor small amounts, set the
threshold, for example `5000000` (5 USDT at 6 decimals). The threshold is in
raw units of whichever asset the transfer is in; a deployment with assets of
different precision should choose it for the most valuable raw unit.

A direct `honor` at or above the threshold answers `409 dual_control_required`
and writes nothing. Instead:

1. The first operator proposes:
   `POST /v1/operator/manual-honor-proposals` with `transfer_id`,
   `payment_intent_id`, `attempt_id`, `allocate_raw`, `reason` and an
   `Idempotency-Key`. Every honor check runs against locked rows; a proposal
   that would be refused is refused now. The proposal stores the command, a
   snapshot of the rows (`evidence`), the threshold and the proposing key. No
   money is written. A transfer has at most one pending proposal.
2. A second operator reads `GET /v1/operator/manual-honor-proposals` and the
   evidence bundle, then approves:
   `POST /v1/operator/manual-honor-proposals/{id}/approve` with a `reason` and
   its own `Idempotency-Key`. The proposing key gets `403
   dual_control_same_operator`. The approval locks and re-reads the transfer,
   attempt and intent and runs every check again: if the intent was cancelled
   or paid, money was allocated elsewhere, or the window closed, it answers
   `409 manual_resolution_conflict` and nothing is written. Otherwise the
   honor runs under the approving key, in the transaction that closes the
   proposal, and exactly one `payment_intent.paid` is queued.
3. Either operator can refuse instead:
   `POST /v1/operator/manual-honor-proposals/{id}/reject` with a `reason`.

A proposal expires 24 hours after it is made (`409 honor_proposal_expired`);
propose it again, with fresh eyes on the evidence. Both identities stay on
record: `manual_honor_proposals` holds the proposer, the decider, both reasons
and the resolution it produced; `manual_resolution_requests` names the
approving key as the one that executed the honor.

Automatic settlement follows the same rules: a payment state moves only from
an explicit set of states and must change exactly one row. When the intent or
attempt stopped being payable between the match and the settlement (cancelled,
or paid through another attempt), the whole settlement rolls back and the
transfer is parked as `held` with a `manual_required` decision instead of
being retried forever. It appears in `/v1/operator/held-payments`.

## Onboarding with the admin CLI

```
gateway-worker admin <command> --actor <your name> [flags]
```

The CLI reads `GATEWAY_DATABASE_URL` (the provisioner login) and, for
`webhook-add` and `webhook-rotate`, `GATEWAY_WEBHOOK_MASTER_KEY` — the same
master key the outbox worker signs with. Every command prints one JSON
object on stdout and writes its change and one `audit_events` row in the same
transaction. A secret (`api-key-issue`'s `secret`, a webhook
`signing_secret`) appears only in that output; store it before closing the
terminal.

| Command | Flags | Prints |
|---|---|---|
| `merchant-create` | `--external-id`, `--name`, `[--collector-policy own\|shared]` (default `own`) | `merchant_id`, `external_id`, `collector_policy`, `created` |
| `api-key-issue` | `--merchant`, `--label` | `key_id`, `merchant_id`, `prefix`, `secret` |
| `api-key-revoke` | `--key`, `--reason` | `key_id`, `revoked` |
| `webhook-add` | `--merchant`, `--url`, `[--description]` | `endpoint_id`, `merchant_id`, `url`, `secret_version`, `signing_secret` |
| `webhook-rotate` | `--endpoint`, `--reason`, `[--transition-hours 1..720]` (default 72) | `endpoint_id`, `secret_version`, `signing_secret`, `previous_secret_signs_until` |
| `webhook-disable` | `--endpoint`, `--reason` | `endpoint_id`, `disabled` |
| `webhook-test` | `--endpoint` | `endpoint_id`, `event_id` |
| `collector-statement` | `--merchant`, `--address`, `[--issued]`; no `--actor`, no database | `statement`, `issued`, `valid_for_hours` |
| `collector-register` | `--asset`, `--address`, and either `--merchant --issued --signature` or `[--merchant] --manual-evidence` | `collector_id`, `merchant_id`, `address`, `ownership_evidence` |
| `collector-stop-quoting` | `--collector`, `--reason` | `collector_id`, `state` |
| `collector-retire` | `--collector`, `--reason`, `[--compromised yes]` | `collector_id`, `retired`, `compromised` |

| `webhook-redeliver` | `--event`, `--reason`, `--idempotency-key`, `[--endpoint]` | `redelivery_id`, `event_id`, `previous_state`, `previous_attempts`, `replayed` |

Read-only commands take no `--actor` and print `{ "items": [...],
"next_cursor": ... }`. Pass `--cursor <next_cursor>` for the next page;
`--limit` is 1 to 200 (default 50), and a value outside that is refused, not
clamped. Nothing they print is a secret: a key appears as its prefix, an
endpoint without its fingerprint. The provisioner role cannot read a key hash
at all.

| Command | Flags | Prints per item |
|---|---|---|
| `merchant-list` | `[--limit] [--cursor]` | `merchant_id`, `external_id`, `display_name`, `status`, `collector_policy`, `created_at` |
| `api-key-list` | `--merchant`, `[--limit] [--cursor]` | `key_id`, `prefix`, `label`, `created_at`, `last_used_at`, `revoked_at` |
| `webhook-list` | `--merchant`, `[--limit] [--cursor]` | `endpoint_id`, `url`, `description`, `status`, `secret_version`, `previous_secret_signs_until`, `created_at`, `disabled_at` |
| `collector-list` | `[--merchant]`, `[--limit] [--cursor]` | `collector_id`, `asset_id`, `merchant_id`, `address`, `state`, `open_reservations`, `valid_from`, `retired_at` |

### Rotate a webhook secret

Rotate when a merchant's secret may have leaked, when a person who saw it
leaves, or on your schedule. One endpoint's rotation never affects another.

1. Agree a transition period with the merchant: long enough for them to
   deploy (72 hours by default, 1 to 720 hours).
2. Rotate:

   ```
   gateway-worker admin webhook-rotate --actor <you> --endpoint <uuid> \
     --reason '<why>' --transition-hours 72
   ```

   Hand `signing_secret` to the merchant over a trusted channel, with
   `previous_secret_signs_until`.
3. Until that moment every delivery carries
   `Gateway-Signature: t=<t>,v1=<new>,v1=<previous>`. The merchant adds the
   new secret beside the old one (a verifier that accepts any matching `v1`
   keeps working), then removes the old one before the transition ends.
4. Confirm with a test event once the merchant says the new secret is
   deployed:

   ```
   gateway-worker admin webhook-test --actor <you> --endpoint <uuid>
   ```

   The `webhook.test` event is delivered by the outbox worker like any other
   event, to every active endpoint of that merchant. Check the delivery in
   `webhook_deliveries` for the printed `event_id`, or ask the merchant; a
   refused test shows up there with its status, and after every retry in
   `/v1/operator/dead-letters`.

If the secret leaked, keep the transition short: the previous secret stays
valid for signing until it ends. A rotation started from a stale secret
version, or two rotations at once, is refused rather than overwritten; run
it again. Rotating a secret does not change the master key; rotating the
master key re-issues every endpoint's secret.

### Revoke a leaked API key

1. Find the key: its `prefix` (the first twelve characters, `gw_...`) against
   `merchant_api_keys.key_prefix`, as above.
2. Revoke it at once; the next request with it is refused:

   ```
   gateway-worker admin api-key-revoke --actor <you> --key <key uuid> --reason '<what leaked, where>'
   ```

3. Issue the merchant a replacement with `api-key-issue` and hand it over a
   trusted channel. For a planned rotation rather than a leak, issue the new
   key first, let the merchant deploy it, then revoke the old one.
4. Review what the leaked key did: every intent and quote it created is in
   `audit_events` with `actor_type = 'api_key'` and `actor_id = <key uuid>`.

A merchant key can create intents and quotes only for its own merchant; it
cannot move money or change where money goes.

### Register a merchant address with proof

For a merchant on the `own` policy. The merchant proves control of the
address by signing; you never see its key.

1. Build the statement and send it to the merchant:

   ```
   gateway-worker admin collector-statement --merchant <uuid> --address <T...>
   ```

2. The merchant signs the `statement` text exactly in TronLink
   (`signMessageV2`) with the wallet that holds the address and returns the
   hex signature.
3. Within 24 hours of `issued`, register it:

   ```
   gateway-worker admin collector-register --actor <you> --asset <asset uuid> \
     --address <T...> --merchant <uuid> --issued <issued> --signature <hex>
   ```

A wrong signer, a stale or future-dated statement, a merchant on `shared`,
an inactive asset, or an address already registered for the asset is
refused. Only when the merchant genuinely cannot sign, replace
`--issued`/`--signature` with `--manual-evidence '<who checked, and how>'`;
it is recorded verbatim.

The address is quoted for that merchant only, from the next quote on. A
merchant may have several active addresses for an asset: each quote goes to
the one holding the fewest amount reservations (the older one on a tie), and
moves to the next when every exact amount near its price is taken there. See
"Capacity" below. It needs no change to `GATEWAY_EXPECTED_COLLECTORS`. An operator address (no
`--merchant`, `--manual-evidence` only) does: add it to
`GATEWAY_EXPECTED_COLLECTORS` for every process in the same deployment, or
processes refuse to start and the API reports not ready.

### Retire a collector

`collector-retire` moves an address straight to `retired`. From then on it is
not quoted, and observers refuse any transfer to it as `retired_collector`
(logged as "a payment reached a retired collector address"), so money that
arrives there later is outside the gateway's books. Retire only when nothing
can still be paid there:

1. Stop new quotes on it first:

   ```
   gateway-worker admin collector-stop-quoting --actor <you> --collector <uuid> --reason '<why>'
   ```

   The address moves to `receiving_only` and the change is audited as
   `collector.stop_quoting`. Receiving-only addresses are still watched and
   still pinned, so money for quotes already issued is still settled.
2. Wait until no reservation remains on it. A lease is archived only after
   its `late_payment_until`.
3. Retire it:

   ```
   gateway-worker admin collector-retire --actor <you> --collector <uuid> --reason '<why>'
   ```

   The command refuses while any reservation remains and names how many.

4. For an operator address, remove it from `GATEWAY_EXPECTED_COLLECTORS` in
   the same deployment (`none` if no operator address is left). For a
   merchant on `own`, register the replacement address before step 1, or
   its quotes are `503 quote_unavailable` until you do.

If the key of an address is compromised, skip the wait: add
`--compromised yes`, which retires at once and records the open reservations
in the audit row, and handle anything paid to it afterwards outside the
gateway.

### Where each action is recorded

Every admin command writes one `audit_events` row with
`actor_type = 'operator'`, the `--actor` name in `payload.actor`, and the
reason in `reason` where the command takes one:

| `action` | `resource_type` | Also in `payload` |
|---|---|---|
| `merchant.create` | `merchant` | `external_id`, `collector_policy` |
| `api_key.issue` | `merchant_api_key` | `prefix`, `label` |
| `api_key.revoke` | `merchant_api_key` | |
| `webhook_endpoint.create` | `webhook_endpoint` | `url` |
| `webhook_endpoint.rotate_secret` | `webhook_endpoint` | `from_version`, `to_version`, `previous_valid_until` |
| `webhook_endpoint.disable` | `webhook_endpoint` | |
| `webhook_endpoint.test` | `webhook_endpoint` | `event_id` |
| `collector.register` | `collector_address` | `address`, `asset_id`, `ownership_evidence` |
| `collector.retire` | `collector_address` | |
| `webhook_event.redeliver` | `domain_event` | `principal`, `endpoint_id`, `previous_state`, `previous_attempts`, `redelivery_id` |

Operator API writes are recorded the same way with the key in `actor_id`:
`webhook_event.redeliver` as above, `honor.propose`, `honor.approve` and
`honor.reject` on `manual_honor_proposal`, and `honor`, `reject` and
`record_remainder_disposition` on `manual_resolution`. Retention records
`retention.purge` with `actor_type = 'system'`, the table as `resource_type`
and the number of rows and the cutoff in `payload`.

```
SELECT created_at, action, payload->>'actor' AS actor, resource_id, reason, payload
  FROM audit_events
 WHERE actor_type = 'operator' AND resource_type = 'webhook_endpoint'
   AND resource_id = '<endpoint uuid>'
 ORDER BY created_at;
```

A refused command writes nothing, neither the change nor an audit row. The
expiry worker records `payment_intent.quote_window_closed` when a quote
closes on an intent that already has money on it; that intent keeps its
status for a person.

## Sending a webhook again

A merchant lost an event, or its endpoint was down until the event was
dead-lettered. Re-queue the event itself:

```
POST /v1/operator/webhook-events/{event_id}/redeliver      (admin, Idempotency-Key)
{ "reason": "merchant restored its endpoint after the outage", "endpoint_id": "<optional>" }

gateway-worker admin webhook-redeliver --actor <you> --event <uuid> \
    --reason "<why>" --idempotency-key <16-128 chars> [--endpoint <uuid>]
```

The event keeps its id, payload and `created_at`, so the envelope is the one
the merchant may already have and deduplicates by id; no second event is
created. `endpoint_id` sends it to that one active endpoint of the event's
merchant; without it, every active endpoint gets it. The retry budget starts
again, while attempt numbers continue, so the earlier delivery history stays.
Refused, with nothing written: an unknown event (`404
webhook_event_not_found`), an endpoint that is not an active endpoint of the
event's merchant or a merchant with none (`404 webhook_endpoint_not_found`),
an event still queued (`409 webhook_event_still_queued`), an operator-channel
event (`409 not_a_webhook_event`). Every redelivery is a row in
`webhook_redeliveries` with who asked and why.

Delivery is shared fairly: one batch takes at most
`GATEWAY_WEBHOOK_MAX_EVENTS_PER_MERCHANT` (default 20) events of one merchant,
and delivers up to `GATEWAY_WEBHOOK_DELIVERY_CONCURRENCY` (default 8)
merchants at the same time, each merchant's events in order. An endpoint that
answers slowly holds back its own merchant, not everyone else's.

## Retention

The `retention` worker role deletes old rows from three append-only
operational tables, in bounded batches, as `gateway_retention`: the only role
that can delete, and only there. Nothing is deleted unless an age is set, and
an age under 30 days is refused.

| Setting | Deletes | Always keeps |
|---|---|---|
| `GATEWAY_RETENTION_WEBHOOK_DELIVERIES_DAYS` | delivery attempts older than the age, of events delivered before the age | the newest `GATEWAY_RETENTION_KEEP_DELIVERY_ATTEMPTS` (default 3) attempts per event and endpoint; every attempt of a pending or dead-lettered event |
| `GATEWAY_RETENTION_OBSERVATIONS_DAYS` | observations older than the age of a chain event whose canonical transfer is `finalized` or `invalidated` | every observation an attestation or a conflict references; every observation of an event with an open conflict or without a final canonical transfer |
| `GATEWAY_RETENTION_HEALTH_EVENTS_DAYS` | health transitions older than the age | the newest transition of every component |

Evidence bundles and reconciliation read attestations, conflicts, canonical
transfers and settlement rows, none of which retention touches, and the
attested observations stay by rule. The database repeats the observation rule
as a row policy on `chain_observations`, so a statement that asked for more
would still delete only what the rule allows. Each batch that deleted rows
writes a `retention.purge` audit row. A good starting point is in
`deploy/env/worker-retention.env.example`; choose ages longer than any dispute
or chargeback window you are bound by.

## When reconciliation says `hard_stop`

The reconciler runs on its interval, re-reads what was observed, canonical,
allocated and fulfilled, and reports every place they disagree. A counter
finding (`observed_not_canonical`, `unmatched_inbound_aging`,
`held_payment_aging`, `observer_behind`) is `drift`: explain it today. A money
finding (`allocation_exceeds_transfer`, `settled_not_fulfilled`,
`fulfilled_not_settled`, `allocated_on_invalidated_transfer`) is `hard_stop`:
the rail of every asset it can name is closed on the spot, and the component
`reconciler` is `stopped`.

1. Read the discrepancy: `transfer_id`, `payment_intent_id`, `asset_id`,
   `detail`.
2. Read the evidence bundle of the intent.
3. Fix the world, not the finding: record the missing fulfilment, reverse the
   allocation, handle the invalidated transfer's product.
4. The next run reports `ok`. The rail stays closed.
5. Clear the rail stop with the explanation. Only a person does this.

A `fulfilled_not_settled` finding recovers the asset from the intent's
immutable quote and closes that rail. If a future corruption cannot be mapped
to an asset, the run still hard-stops and must be treated as a system-wide
incident until a dedicated global stop exists.

## Capacity

An address holds one reservation per exact amount, for the quote's lifetime
plus its late-payment window; `amount_slot_count` in the quote policy is how
many exact amounts near one price it may use. When they are all taken on one
address, the quote moves to the merchant's next active address, up to eight.
Watch it on `/metrics`:

- `gateway_collector_open_leases` and `gateway_collector_live_leases` per
  address: all reservations, and those whose quote is still payable.
- `gateway_quote_requests_total{outcome=...}`: `slots_exhausted` means every
  address was full near that price; add an address or widen
  `amount_slot_count`.
- `gateway_quote_collector_spillovers_total`: quotes that did not fit on the
  least loaded address.
- `gateway_quote_duration_seconds`: the time to answer a quote request,
  refusals included.

`GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR` (API, unset by default) caps the
reservations one address may hold. A full address is skipped, and when every
address of the merchant is full the quote is refused with
`503 quote_capacity_exhausted` and `Retry-After: 60`, before any lock is
taken. The count is read before the collector lock, so concurrent quotes may
overshoot the cap by a few: it is admission control, not a ledger limit.

## Request budgets

The API keeps a token bucket per merchant and per client address, in each
replica's memory (so the effective limit is the setting times the replica
count). A spent budget answers `429 rate_limited` with `Retry-After`.

| Variable | Default | Budget |
|---|---|---|
| `GATEWAY_RATE_LIMIT_MERCHANT_WRITES_PER_MINUTE` | 600 | `POST` per merchant |
| `GATEWAY_RATE_LIMIT_MERCHANT_READS_PER_MINUTE` | 3000 | `GET` per merchant |
| `GATEWAY_RATE_LIMIT_AUTH_FAILURES_PER_MINUTE` | 30 | failed authentications per client address, merchant and operator routes; once spent, every request from that address is refused until it refills |
| `GATEWAY_RATE_LIMIT_CHECKOUT_READS_PER_MINUTE` | 600 | payment-page requests per client address |
| `GATEWAY_CLIENT_IP_HEADER` | unset | header your reverse proxy appends the client address to, for example `x-forwarded-for`; its last entry is used |

`0` disables a budget. A value that is set but not a number stops the
process at start-up. Behind a reverse proxy, set `GATEWAY_CLIENT_IP_HEADER`,
or every client shares the proxy's address and one caller's failed keys
lock out everyone. Request bodies above 64 KiB are refused with `413`, and
`metadata` above 16 KiB serialized with `422`.

## Latency

Every process serves its own latency on `GATEWAY_METRICS_BIND_ADDRESS`
(no key; cluster-internal). What to read when something is slow:

| Series | Says |
|---|---|
| `gateway_http_request_duration_seconds{route,status}` | how long the API took per matched route; `unmatched` is a 404 |
| `gateway_db_pool_connections{state}` against `gateway_db_pool_max_connections` | a pool pinned at `busy == max` is the API waiting for the database |
| `gateway_db_pool_acquire_wait_seconds{path,outcome}` | how long a transaction (`path="transaction"`) or the sampler's probe waited for a connection; a rising wait with `busy == max` means the pool, not the database, is the queue |
| `gateway_chain_source_request_duration_seconds{provider,endpoint,outcome}` | how long each provider takes; `refused` is a non-2xx answer, `unreachable` a timeout or a connection failure |
| `gateway_worker_batch_duration_seconds{worker}` and `gateway_worker_run_duration_seconds{worker,outcome}` | how long a worker's batches and whole runs take; a run near its interval is a worker that cannot keep up |
| `gateway_outbox_first_attempt_delay_seconds` | how long an event waits for its first delivery; the outbox's backlog, as a merchant feels it |
| `gateway_webhook_delivery_duration_seconds{outcome}` | how long merchant endpoints take to answer |

## Alerts worth setting

From `/metrics`, scraped with a `read` key:

| Alert | Condition |
|---|---|
| Rail closed | `gateway_rail_stops_open > 0` |
| Reconciliation not clean | `gateway_reconciliation_last_run_status != 0` |
| Reconciliation stale | `time() - gateway_reconciliation_last_run_timestamp_seconds > 2 × interval` |
| Money waiting for a person | `gateway_transfers_processing{state="unmatched"} > 0` or `{state="held"} > 0` for longer than your SLA |
| Merchants not told | `gateway_outbox_events_dead_lettered > 0`; `gateway_outbox_events_pending` growing |
| Sources disagree | `gateway_observation_conflicts_open > 0` |
| A component is down | `gateway_component_state{component=~".*"} > 1` for more than one interval |
| Expiry sweep stuck | `time() - gateway_expiry_last_success_timestamp_seconds > 5 × interval` |
| Addresses filling up | `rate(gateway_quote_requests_total{outcome=~"slots_exhausted\|capacity_exhausted"}[15m]) > 0` |

Absent telemetry is not health: the scrape fails as a whole when storage
cannot answer, so alert on the scrape failing too. Also alert on the observer
log line "a payment reached a retired collector address": it is money outside
the books.
