# Crypto Gateway — open-source, non-custodial stablecoin payment gateway in Rust

[![ci](https://github.com/Sskutushev/crypto-gateway-project/actions/workflows/ci.yml/badge.svg)](https://github.com/Sskutushev/crypto-gateway-project/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A self-hosted payment gateway that accepts and verifies inbound stablecoin
payments (USDT on TRON first) for a merchant's orders, without ever holding a
private key. A merchant creates a payment intent, quotes it in a token, shows
the payer an exact amount and an address; the gateway watches the chain
through independent providers, verifies the transfer, settles the obligation
exactly once, and tells the merchant with a signed webhook. Rust, PostgreSQL,
one container image.

**Four properties**

- **Exact integer money.** Fiat minor units and 256-bit token units, decimal
  strings on the wire, floating point forbidden by the linter.
- **Independent blockchain evidence.** A provider's answer is an observation;
  a payment becomes a fact only when independent providers and the gateway's
  own re-read agree.
- **Idempotent API, signed webhooks.** Every write carries an idempotency key;
  every event is signed with a per-endpoint secret derived from a master key
  the database never sees.
- **A reconcilable audit trail.** Every state change is a row; a reconciler
  re-adds the books and closes the rail by itself when money stops adding up.

Supported rail: **USDT TRC20** (TRON). ERC20 and TON follow through the same
observer and verifier interface.

## Quickstart (about ten minutes)

Prerequisites: Docker with Compose, `psql`, `curl` and `jq`. Rust is not
needed to run the image.

```sh
# 1. PostgreSQL, then the schema, then the development rail
docker compose up -d postgres
docker compose run --rm -e GATEWAY_MIGRATE_ONLY=true gateway-api
export DB=postgres://gateway:gateway@127.0.0.1:54329/gateway
psql "$DB" -f scripts/seed-dev-rail.sql

# 2. A merchant key and an operator key (hashes only reach the database)
MERCHANT_KEY=$(openssl rand -hex 24); OPERATOR_KEY=$(openssl rand -hex 24)
psql "$DB" -v merchant_id=00000000-0000-7000-8000-000000000001 \
  -v key_id=00000000-0000-7000-8000-000000000002 -v api_key_prefix=cg_dev \
  -v api_key_sha256_hex=$(printf '%s' "$MERCHANT_KEY" | sha256sum | cut -d' ' -f1) \
  -f scripts/create-dev-merchant.sql
psql "$DB" -v key_id=00000000-0000-7000-8000-000000000003 -v api_key_prefix=cgop_dev \
  -v api_key_sha256_hex=$(printf '%s' "$OPERATOR_KEY" | sha256sum | cut -d' ' -f1) \
  -f scripts/create-dev-operator.sql

# 3. The API. It refuses to start until the database describes the rail it
#    was configured for, so this step is a test of the self-check too.
docker compose up -d gateway-api
curl -s localhost:8080/health/ready

# 4. Price evidence and rail health, from two independent groups
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ); ASSET=00000000-0000-7000-8000-000000000101
curl -s -X POST localhost:8080/v1/operator/price-snapshots \
  -H "Authorization: Bearer $OPERATOR_KEY" -H 'Content-Type: application/json' \
  -d "{\"asset_id\":\"$ASSET\",\"fiat_currency\":\"USD\",\"readings\":[
    {\"source_key\":\"a\",\"provider_group\":\"a\",\"rate_numerator\":\"1\",\"rate_denominator\":\"10000\",\"observed_at\":\"$NOW\"},
    {\"source_key\":\"b\",\"provider_group\":\"b\",\"rate_numerator\":\"1\",\"rate_denominator\":\"10000\",\"observed_at\":\"$NOW\"}]}"
curl -s -X POST localhost:8080/v1/operator/rail-health \
  -H "Authorization: Bearer $OPERATOR_KEY" -H 'Content-Type: application/json' \
  -d "{\"asset_id\":\"$ASSET\",\"health\":\"healthy\"}"

# 5. An order, and a quote for it
INTENT=$(curl -s -X POST localhost:8080/v1/payment-intents \
  -H "Authorization: Bearer $MERCHANT_KEY" -H 'Idempotency-Key: order-1' \
  -H 'Content-Type: application/json' \
  -d '{"amount_minor":"4999","currency":"USD","reference":"order-1"}' | jq -r .id)
curl -s -X POST localhost:8080/v1/payment-intents/$INTENT/quotes \
  -H "Authorization: Bearer $MERCHANT_KEY" -H 'Idempotency-Key: quote-1' \
  -H 'Content-Type: application/json' -d "{\"asset_id\":\"$ASSET\"}"

# 6. What the operator sees
curl -s localhost:8080/v1/operator/overview -H "Authorization: Bearer $OPERATOR_KEY"
curl -s localhost:8080/metrics -H "Authorization: Bearer $OPERATOR_KEY" | head
```

The quote names a collector address and an exact `amount_raw`. To watch a
real Nile testnet payment settle, put a TronGrid API key and a webhook master
key into `.env` (see `.env.example`), replace the placeholder collector with
an address you control in `.env` and `scripts/seed-dev-rail.sql`, and start
the workers:

```sh
docker compose --profile workers up -d
```

Two observers read TronGrid and the public Nile node, the verifier re-reads
through the node, and paying the exact `amount_raw` of test USDT to the
collector settles the intent and delivers `payment_intent.paid` to the
merchant's endpoint. Evidence and queues: `GET /v1/operator/payment-intents/{id}`
and the routes in [`docs/operator-runbook.md`](docs/operator-runbook.md).

## How it works

```
merchant ──► API ──► payment intent ──► quote: exact amount at a collector
                                             │
chain provider A ──► observer A ──┐          ▼
chain provider B ──► observer B ──┼──► verifier ──► canonical transfer
                    (own re-read) ┘             │
                                                ▼
                               settlement (one transaction) ──► outbox ──► signed webhook
                                                │
                               reconciler: does it still add up? ──► rail stop
```

Seven processes from one image: the API and one worker per role. Each has
its own PostgreSQL role with only the privileges its code uses, and row level
security binds every observer to the chain source it speaks for. Full design:
[`docs/architecture.md`](docs/architecture.md).

## Security model

- **Non-custodial.** No private keys, no signing, no withdrawals, no balances.
  A stolen host can lie about receipts and nothing else.
- **Two-layer chain facts.** Observations are append-only and never trusted
  alone; the verifier alone writes canonical transfers, from independent
  agreement plus its own re-read.
- **Integer money end to end.** `NUMERIC(78,0)`, `U256`, decimal strings;
  parsers refuse signs, decimals, zero and overflow; fuzzed.
- **Signed webhooks.** HMAC-SHA256 over timestamp and body under a derived
  per-endpoint secret; fingerprints only in the database; no redirects.
- **Fail closed.** Missing or stale evidence refuses a quote; a process whose
  database disagrees with its configuration refuses to start; money that does
  not add up closes the rail until a person clears it.

Details and the attacks each layer refuses: [`docs/threat-model.md`](docs/threat-model.md).

## How it fails, on purpose

| The gateway refuses when | Because |
|---|---|
| fewer than two independent price groups agree, or they disagree beyond policy | a rate nobody corroborated is a guess about someone's money |
| price, policy or rail-health evidence is missing or stale | an issued quote must rest on evidence that existed when it was issued |
| a rail is closed by an operator or by reconciliation | new obligations must not pile onto a rail under investigation |
| a transfer matches two reservations, or none | ambiguity is a decision for a person; money nobody can explain is queued, not absorbed |
| observers disagree about a chain event | the disagreement is the finding; no fact is made from it |
| a process's database does not describe its configured collectors, assets or environment | a database-only address change must not redirect receipts |

## Documentation

- [Architecture](docs/architecture.md) and [decisions](docs/decisions/)
- [Merchant integration](docs/merchant-integration.md): intents, quotes, statuses, webhook verification
- [OpenAPI 3.1](docs/openapi.json): every route the router serves, checked by a test
- [Operator runbook](docs/operator-runbook.md): feeding evidence, reading queues, clearing a hard stop, alerts
- [Deployment](docs/deployment.md): roles, Compose, Kubernetes, the TLS and network boundary
- [Threat model](docs/threat-model.md)
- [Owner setup](docs/owner-setup.md): every account, key and secret, in order
- [Implementation status](docs/implementation-status.md): what is verified, what is next

## Development

Rust 1.90 (pinned in `rust-toolchain.toml`), Docker with Compose, PostgreSQL 16.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
GATEWAY_TEST_DATABASE_URL=postgres://gateway:gateway@127.0.0.1:54329/gateway \
  cargo test --workspace --locked -- --ignored     # PostgreSQL scenarios
GATEWAY_FUZZ_ITERATIONS=200000 cargo test --release -- fuzz_smoke
cargo deny check
```

CI runs all of it on every push, plus compose and kustomize validation, an
SBOM and an image scan; images publish to GHCR from a version tag. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) for the rules, [`SECURITY.md`](SECURITY.md)
for reporting a vulnerability, and [`CHANGELOG.md`](CHANGELOG.md) for what
exists today.

## Status

Every layer from payment intent to signed webhook exists and is verified by
unit tests, PostgreSQL scenarios and seeded property tests. No rail is
declared production-ready until it has run on a testnet with two real
providers; the steps are in [`docs/owner-setup.md`](docs/owner-setup.md).

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
