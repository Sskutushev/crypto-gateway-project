# Operator runbook

The operator feeds the gateway the evidence it will not invent, reads what it
found, and is the only party that can reopen a rail. Every route below needs
an operator key; the scope each one needs is in [`openapi.json`](openapi.json).

## Keys and scopes

| Scope | Lets a key | Give it to |
|---|---|---|
| `ingest` | submit price readings and rail health | pricing and rail-health feeders |
| `risk_ingest` | submit current screening decisions for one DB-bound provider | one KYT integration credential per provider |
| `read` | read the overview, every queue, evidence bundles, `/metrics` | people, dashboards, Prometheus |
| `admin` | close and reopen a rail | a person, never a job |

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
| `/v1/operator/held-payments` | Settlement bands wanted a person: amount over the auto-settle tier, risk review, ambiguity. |
| `/v1/operator/dead-letters` | Events no endpoint accepted after every retry, with the delivery history. |
| `/v1/operator/reconciliation/runs` | Every run, clean or not. |
| `/v1/operator/reconciliation/discrepancies` | Findings nobody has resolved. |

`GET /v1/operator/payment-intents/{id}` is the evidence bundle for one payment,
across merchants: the obligation, quotes and reservations, allocations, the
settlement decision with its policy, groups and risk verdict, the fulfilment
claim, payment events and the canonical transfers with their attestation
counts. It is what you read during a dispute.

## Resolving parked money

`POST /v1/operator/manual-resolutions` requires `admin`, a 16–128 character
`Idempotency-Key`, and a recorded reason. The accepted actions are deliberately
narrow:

- `honor` assigns one finalized `held`/`unmatched` transfer to an existing
  intent and attempt. `allocate_raw` must equal the exact safe allocation
  `min(outstanding obligation, unallocated transfer)`; the normal claim,
  allocation, fulfilment, event and outbox transaction is reused.
- `reject` closes a finalized held/unmatched transfer only when no allocation
  exists.
- `record_remainder_disposition` records how an overpayment remainder was
  handled outside the gateway. It requires the exact remainder and an external
  reference; it never claims the gateway sent a refund because the gateway has
  no wallet key.

An identical replay returns the first result. Reusing the key for a different
command returns `idempotency_conflict`. Never repair these states with direct
SQL updates.

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

Absent telemetry is not health: the scrape fails as a whole when storage
cannot answer, so alert on the scrape failing too.
