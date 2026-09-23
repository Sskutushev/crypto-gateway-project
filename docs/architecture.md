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

- `gateway-api`: merchant authentication, payment intents, status reads,
  webhook configuration, and, until the workers exist, the bounded quote-expiry
  scheduler.
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

## Operator surface

Operator reads are a separate authenticated surface spanning all merchants so
an incident can be reconstructed without weakening merchant isolation. A
dedicated `read` scope exposes bounded, keyset-paginated evidence for health,
conflicts, unmatched transfers, held payments, dead letters and reconciliation;
the same scope protects the Prometheus scrape. Metrics are rendered from one
complete database snapshot request and fail the scrape on storage error, since
silently missing a family would turn an observability outage into a false
healthy signal.

## Degradation

The system fails closed for new quotes when evidence, pricing, policy, or
reconciliation is stale. Existing unexpired quotes remain payable and are
never silently discarded. Every provider switch and degraded decision is an
audited event and metric.

## Quote and exact-amount lease lifecycle

The application accepts quote evidence only from trusted pricing, policy, and
rail-health adapters. Missing, stale, future-dated, or unhealthy evidence
fails closed before persistence. Fiat-to-token conversion uses a rational rate
and rounds upward with checked 256-bit integer arithmetic.

The public quote request contains only an allowlisted asset ID. The application
selects the active collector and the latest immutable price and rail-health
snapshots plus the one active versioned quote policy. Every issued quote stores
foreign keys to those exact snapshots. Client-supplied prices, amounts,
collector addresses, policy values, or health claims are rejected as unknown
request fields.

PostgreSQL serializes allocation per collector address and assigns one of at
most 10,000 exact-amount slots. Constraints guarantee one active lease for a
collector/raw-amount pair and one lease per payment attempt. Composite foreign
keys keep merchant, intent, quote, asset, collector, and attempt ownership
consistent even if a future caller bypasses the application service.

Quote expiry and amount reuse are deliberately separate. At quote expiry the
attempt and payment intent become expired, while the exact amount remains
reserved through the late-payment window. Only after that window does one
transaction copy the lease into immutable history, delete the active lease,
and append its audit event. There is no probability-based early release.
Idempotent replay is resolved before current health checks, so a merchant can
recover an already issued obligation during a later provider or rail outage.

Deadlines are advanced by a scheduler, not by a read. The scheduler owns no
expiry logic: it repeatedly asks the application layer for one bounded
transaction and stops when nothing is due, when the per-tick batch ceiling is
reached, or when shutdown is requested between batches. Overlap is impossible
by construction. A single process holds a single-flight guard and delays its
next tick until the running sweep ends; across processes PostgreSQL arbitrates,
because each batch locks the rows it claims and skips locked ones, so a lease
is archived exactly once even when two schedulers sweep the same instant.

Only a storage outage is retried, with capped backoff and a counted attempt
ceiling. A violated invariant -- an attempt that did not advance together with
its payment intent -- ends the sweep loudly instead of being retried against
state that will not change. Retries, overlaps, failure streaks, remaining
backlog, and the last successful sweep are counters, and an absent success is
reported as absent rather than as an old timestamp.

## Initial network sequence

1. USDT TRC20: two independent provider groups, advisory-only period, then
   policy-controlled settlement.
2. USDT ERC20: adapter-only addition after the generic boundary is proven.
3. USDT on TON: dedicated Jetton verification and adversarial test suite.
