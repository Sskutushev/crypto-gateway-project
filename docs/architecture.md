# Architecture

## Scope

The system accepts inbound cryptocurrency payments for merchants. It creates
payment intents, quotes an exact amount, observes supported networks, verifies
canonical transfers from independent evidence, allocates funds exactly once,
and delivers signed merchant events.

The system does not custody keys, initiate transfers, maintain customer
balances, exchange assets, or provide refunds on-chain.

## Trust boundaries

```text
merchant -> public API -> payment core -> PostgreSQL -> webhook outbox
                              ^                 ^
                              |                 |
                      matcher/settler      verifier/reconciler
                                                ^
                                                |
                                     append-only observations
                                         ^               ^
                                  observer A       observer B
                                  provider group 1 provider group 2
                                         \               /
                                          blockchain/RPC
```

An observer records what one source claimed. It cannot write canonical
transfers or settlement state. The verifier produces a canonical fact only
after the configured policy has enough independent evidence. The settlement
worker consumes finalized canonical facts and commits allocation, ledger
entries, state changes, and webhook events in one database transaction.

## Deployable services

- `gateway-api`: merchant authentication, payment intents, status reads, and
  webhook configuration.
- `chain-observer-*`: one process per chain and source identity. Writes only
  append-only observations.
- `chain-verifier`: canonicalizes independently attested observations and
  advances finality state.
- `payment-worker`: matching, risk holds, allocation, settlement, and outbox
  production.
- `webhook-worker`: signed at-least-once delivery with retry and dead-letter
  state.
- `chain-reconciler`: scheduled gap scan and two-sided reconciliation.

All binaries share domain crates but use distinct database roles and runtime
credentials.

## Core aggregates

- Merchant and API credential
- Webhook endpoint
- Payment intent
- Quote and payment attempt
- Chain asset and collector address
- Chain source and immutable observation
- Canonical transfer and source attestation
- Transfer state history
- Transfer claim and allocation
- Financial ledger entry
- Transactional outbox event and delivery attempt
- Reconciliation run and discrepancy
- Audit event

## Money model

- Fiat values use signed 64-bit minor units plus an ISO currency code.
- Token values use unsigned 256-bit raw units plus an immutable asset decimal
  count.
- Public JSON represents both as decimal strings.
- Database token quantities use `NUMERIC(78,0)`.
- Floating-point values are rejected in APIs, domain types, SQL, and linting.

## State model

Payment intent states are monotonic:

```text
requires_quote -> awaiting_payment -> partially_paid -> paid
                       |                    |
                       +-> expired          +-> risk_hold
                       +-> cancelled
```

Terminal or exceptional corrections are new events, not destructive rewrites.
Transfer finality is separately monotonic:

```text
observed -> canonical -> confirmed -> finalized
    \          \            \            \
     +----------+------------+-------------> invalidated
```

## Delivery semantics

API writes are idempotent per merchant, route, and idempotency key. Blockchain
observations are idempotent per source and semantic assertion. A transfer has
one order claim. Ledger entries have unique business references. Webhook
events have stable IDs and are delivered at least once; merchants must dedupe
by event ID.

## Degradation

The system fails closed for new quotes when evidence, pricing, policy, or
reconciliation is stale. Existing unexpired quotes remain payable and are
never silently discarded. Every provider switch and degraded decision is an
audited event and metric.

## Initial network sequence

1. USDT TRC20: two independent provider groups, advisory-only period, then
   policy-controlled settlement.
2. USDT ERC20: adapter-only addition after the generic boundary is proven.
3. USDT on TON: dedicated Jetton verification and adversarial test suite.

