# Crypto Gateway Project

An open-source, non-custodial gateway for accepting and verifying inbound
stablecoin payments.

The project is being built around four properties:

- exact integer accounting;
- independent blockchain evidence;
- idempotent merchant APIs and signed webhooks;
- a permanent, reconcilable audit trail.

The gateway never stores treasury private keys and does not implement
withdrawals or customer crypto balances.

## Status

Foundation work is in progress. No network rail is production-ready yet. See
[`docs/implementation-status.md`](docs/implementation-status.md) for the live
handoff and [`docs/architecture.md`](docs/architecture.md) for the target
design.

## Planned first release

The first production rail is USDT on TRON. Ethereum USDT and TON USDT follow
only after the generic observer/verifier interface is proven by the first
rail.

## Development

Both `gateway-api` and `gateway-worker` require these start-up self-check
settings. They deliberately have no production-friendly fallback:

- `GATEWAY_EXPECTED_COLLECTORS`: comma-separated canonical TRON base58
  collector addresses; prevents a database-only address change from redirecting receipts.
- `GATEWAY_EXPECTED_ASSETS`: comma-separated `chain:network:contract_base58`
  assets; prevents an unreviewed contract from becoming payable.
- `GATEWAY_CHAIN_ENVIRONMENT`: exactly `testnet` or `mainnet`; prevents a
  process and its active database rows from referring to different worlds.
- `GATEWAY_MAX_CLOCK_SKEW_SECONDS`: positive whole-second PostgreSQL/process
  clock-skew limit, default `5`; protects time-window and lease decisions.

Operator keys carrying the `read` scope can inspect the gateway through:

- `GET /v1/operator/overview`
- `GET /v1/operator/conflicts`
- `GET /v1/operator/unmatched-transfers`
- `GET /v1/operator/held-payments`
- `GET /v1/operator/dead-letters`
- `GET /v1/operator/reconciliation/runs`
- `GET /v1/operator/reconciliation/discrepancies`
- `GET /v1/operator/payment-intents/{intent_id}`
- `GET /metrics`

The list routes use UUID keyset pagination through `limit` and `before`.
Prometheus must send an operator key carrying `read` as its `bearer_token`;
`/metrics` is intended for in-cluster scraping only and must not be public.

Prerequisites:

- Rust stable (pinned in `rust-toolchain.toml`)
- Docker with Compose
- PostgreSQL 16+

The executable foundation exposes authenticated create/read payment intents
and quote issuance with merchant-scoped idempotency. Quote requests choose an
allowlisted asset; pricing, policy, rail health, and collector details always
come from server-owned PostgreSQL snapshots. It is still a development
foundation: no blockchain rail is production-ready.

`POST /v1/payment-intents/{intent_id}/quotes` requires an `Idempotency-Key`
header and accepts only:

```json
{"asset_id":"00000000-0000-0000-0000-000000000000"}
```

Amounts in the response are decimal strings. The endpoint returns `503
quote_unavailable` instead of inventing a price when any required snapshot is
missing, stale, future-dated, or unhealthy. A replay of an already issued quote
remains available during a later pricing or rail outage.

The API process also runs the quote-expiry scheduler. Every interval it asks
the application layer for one bounded transaction that expires due quotes and
archives leases whose late-payment window ended, repeating until nothing is due
or the per-tick ceiling is reached. Two sweeps never run at once, transient
storage errors are retried with capped backoff, and an invariant violation
stops the sweep instead of being retried. `SIGTERM` and `Ctrl-C` stop the loop
between batches, so an in-flight expiry transaction is never abandoned.

Its settings are read once at startup and an unreadable value fails startup
rather than silently using a default:

```text
GATEWAY_EXPIRY_ENABLED=true
GATEWAY_EXPIRY_INTERVAL_SECONDS=30
GATEWAY_EXPIRY_BATCH_LIMIT=200
GATEWAY_EXPIRY_MAX_BATCHES_PER_TICK=10
GATEWAY_EXPIRY_RETRY_ATTEMPTS=3
GATEWAY_EXPIRY_RETRY_INITIAL_BACKOFF_SECONDS=1
GATEWAY_EXPIRY_RETRY_MAX_BACKOFF_SECONDS=10
```

Start the local stack with `docker compose up --build`. Compose binds the API
and PostgreSQL only to the host loopback interface. The API serves plain HTTP
for local development; any non-local deployment must terminate TLS before the
API and must not expose PostgreSQL publicly. Production database credentials
must be unique, least-privileged, and supplied outside this repository.

Core checks for a local Rust toolchain are:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

The PostgreSQL-backed scenarios are intentionally ignored by the default test
command. After starting the Compose PostgreSQL service, run them explicitly
with `GATEWAY_TEST_DATABASE_URL` set to the disposable database and pass
`-- --ignored` to `cargo test --workspace`. They share one schema and
serialize themselves, so no special thread count is required.

## License

Licensed under the Apache License, Version 2.0.
