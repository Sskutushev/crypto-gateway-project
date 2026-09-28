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

Supported rail: **USDT TRC20** (TRON). ERC20 and TON are planned behind the
same observer and verifier interface; neither exists yet.

## Quickstart (about ten minutes)

Prerequisites: Docker with Compose, `psql`, `curl`, `jq` and `openssl`. Rust
is not needed to run the image.

Merchants, their API keys, webhook endpoints and collector addresses are
created with the admin CLI, `gateway-worker admin <command>`, which ships in
the same image. Every command names the person who runs it (`--actor`), is
written to `audit_events` in the same transaction as its change, and prints
one JSON object. A secret is printed exactly once; the database keeps only a
hash or a fingerprint. Operator keys, assets, policies and chain sources have
no command yet and are still seeded with SQL.

```sh
# 1. PostgreSQL, then the schema, then the development rail (asset, operator
#    collector, policies, chain sources)
docker compose up -d postgres
docker compose run --rm -e GATEWAY_MIGRATE_ONLY=true gateway-api
export DB=postgres://gateway:gateway@127.0.0.1:54329/gateway
psql "$DB" -f scripts/seed-dev-rail.sql

# 2. An operator key (no admin command issues one yet; only its hash is stored)
OPERATOR_KEY=$(openssl rand -hex 24)
psql "$DB" -v key_id=00000000-0000-7000-8000-000000000003 -v api_key_prefix=cgop_dev \
  -v api_key_sha256_hex=$(printf '%s' "$OPERATOR_KEY" | sha256sum | cut -d' ' -f1) \
  -f scripts/create-dev-operator.sql

# 3. A merchant and its API key, through the admin CLI. 'shared' puts this
#    merchant on the operator collector the development rail seeded.
admin() { docker compose run --rm -T --no-deps --entrypoint /usr/local/bin/gateway-worker \
  ${GATEWAY_WEBHOOK_MASTER_KEY:+-e GATEWAY_WEBHOOK_MASTER_KEY} gateway-api admin "$@"; }
MERCHANT=$(admin merchant-create --actor "$USER" --external-id demo-shop \
  --name 'Demo shop' --collector-policy shared | jq -r .merchant_id)
MERCHANT_KEY=$(admin api-key-issue --actor "$USER" --merchant "$MERCHANT" \
  --label quickstart | jq -r .secret)

# 4. The API. It refuses to start until the database describes the rail it
#    was configured for, so this step is a test of the self-check too.
docker compose up -d gateway-api
curl -s localhost:8080/health/ready

# 5. Price evidence and rail health, from two independent groups. A rate is
#    raw token units per fiat minor unit: 1 cent = 10 000 units of a 6-decimal
#    stablecoin at 1:1, so numerator 10000 over denominator 1.
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ); ASSET=00000000-0000-7000-8000-000000000101
curl -s -X POST localhost:8080/v1/operator/price-snapshots \
  -H "Authorization: Bearer $OPERATOR_KEY" -H 'Content-Type: application/json' \
  -d "{\"asset_id\":\"$ASSET\",\"fiat_currency\":\"USD\",\"readings\":[
    {\"source_key\":\"a\",\"provider_group\":\"a\",\"rate_numerator\":\"10000\",\"rate_denominator\":\"1\",\"observed_at\":\"$NOW\"},
    {\"source_key\":\"b\",\"provider_group\":\"b\",\"rate_numerator\":\"10000\",\"rate_denominator\":\"1\",\"observed_at\":\"$NOW\"}]}"
curl -s -X POST localhost:8080/v1/operator/rail-health \
  -H "Authorization: Bearer $OPERATOR_KEY" -H 'Content-Type: application/json' \
  -d "{\"asset_id\":\"$ASSET\",\"health\":\"healthy\"}"

# 6. An order, and a quote for it
INTENT=$(curl -s -X POST localhost:8080/v1/payment-intents \
  -H "Authorization: Bearer $MERCHANT_KEY" -H 'Idempotency-Key: order-0001-attempt-1' \
  -H 'Content-Type: application/json' \
  -d '{"amount_minor":"4999","currency":"USD","reference":"order-1"}' | jq -r .id)
curl -s -X POST localhost:8080/v1/payment-intents/$INTENT/quotes \
  -H "Authorization: Bearer $MERCHANT_KEY" -H 'Idempotency-Key: quote-0001-attempt-1' \
  -H 'Content-Type: application/json' -d "{\"asset_id\":\"$ASSET\"}"

# 7. What the operator sees
curl -s localhost:8080/v1/operator/overview -H "Authorization: Bearer $OPERATOR_KEY"
curl -s localhost:8080/metrics -H "Authorization: Bearer $OPERATOR_KEY" | head
```

The quote names a collector address and an exact `amount_raw`. The seeded
operator collector is a placeholder nobody holds a key for, so do not pay it.

### A real Nile testnet payment, on your own address

Put a TronGrid API key and a webhook master key into `.env` (see
`.env.example`). Then create a merchant on the default collector policy,
`own`, and register a Nile address you control for it. The address is
accepted only on a signature from the wallet that holds it:

```sh
export GATEWAY_WEBHOOK_MASTER_KEY=$(sed -n 's/^GATEWAY_WEBHOOK_MASTER_KEY=//p' .env)
SHOP=$(admin merchant-create --actor "$USER" --external-id my-shop --name 'My shop' \
  | jq -r .merchant_id)
SHOP_KEY=$(admin api-key-issue --actor "$USER" --merchant "$SHOP" --label testnet | jq -r .secret)

# The statement to sign. Sign its "statement" text exactly, in TronLink
# (signMessageV2), with the wallet that holds the address; it stays valid 24 hours.
admin collector-statement --merchant "$SHOP" --address <your Nile address>
admin collector-register --actor "$USER" --asset "$ASSET" --address <your Nile address> \
  --merchant "$SHOP" --issued <"issued" from the statement> --signature <hex signature>

# A webhook endpoint: a public https URL on port 443 (a tunnel works). The
# signing secret is printed once; send yourself a signed test event.
ENDPOINT=$(admin webhook-add --actor "$USER" --merchant "$SHOP" \
  --url https://<your receiver>/webhooks | jq -r .endpoint_id)
admin webhook-test --actor "$USER" --endpoint "$ENDPOINT"

docker compose --profile workers up -d
```

A merchant-owned address is pinned by the self-check but is not listed in
`GATEWAY_EXPECTED_COLLECTORS`; that variable names operator collectors only.
Two observers read TronGrid and the public Nile node, the verifier re-reads
through the node, and paying the exact `amount_raw` of test USDT to the
quoted address settles the intent and delivers `payment_intent.paid` to the
merchant's endpoint. Evidence and queues:
`GET /v1/operator/payment-intents/{id}` and the routes in
[`docs/operator-runbook.md`](docs/operator-runbook.md).

## Integrate

A merchant integration is two HTTP calls, a status read and one signed
webhook: [`docs/merchant-integration.md`](docs/merchant-integration.md).
[`examples/`](examples/) has working code to start from:

- [`examples/create-payment`](examples/create-payment): intent, quote and
  polling, in shell and in TypeScript;
- [`examples/webhook-receiver-typescript`](examples/webhook-receiver-typescript)
  and [`examples/webhook-receiver-python`](examples/webhook-receiver-python):
  signature verification over the raw body with several `v1` values during a
  secret rotation, timestamp tolerance and deduplication, with tests;
- [`examples/postman`](examples/postman): a collection for every route.

## Scope

What is supported, what is not, and what happens to an underpayment, an
overpayment, a late payment, the wrong token or the wrong network:
[`docs/scope-and-limits.md`](docs/scope-and-limits.md). In short: incoming
USDT TRC20 only, no payouts or refunds from the gateway, one quote per intent,
and no API route to cancel an intent yet.

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

Seven processes from one image: the API and one worker per role, plus the
one-shot admin CLI. Each has its own PostgreSQL role with only the privileges
its code uses, and row level security binds every observer to the chain
source it speaks for. Full design: [`docs/architecture.md`](docs/architecture.md).

## Security model

- **Non-custodial.** No private keys, no signing, no withdrawals, no balances.
  A stolen host can lie about receipts and nothing else.
- **A merchant is paid on its own address.** New merchants default to the
  `own` collector policy: quotes use only addresses registered to that
  merchant, each proven by a signature from the wallet that holds it. The
  database refuses a quote on another merchant's address, and there is no
  fallback between the `own` and `shared` policies.
- **Two-layer chain facts.** Observations are append-only and never trusted
  alone; the verifier alone writes canonical transfers, from independent
  agreement plus its own re-read.
- **Money is tied to its own attempt.** A transfer is allocated only at its
  attempt's collector, in its quote's asset and rail; payment states move
  only from an explicit set of states, and a refused move rolls the whole
  settlement back and parks the transfer for a person.
- **Integer money end to end.** `NUMERIC(78,0)`, `U256`, decimal strings;
  parsers refuse signs, decimals, zero and overflow; fuzzed.
- **Signed webhooks.** HMAC-SHA256 over timestamp and body under a derived
  per-endpoint secret; fingerprints only in the database; no redirects, no
  system proxies, public addresses only. A secret rotates per endpoint with a
  transition period in which both secrets sign.
- **Fail closed.** Missing or stale evidence refuses a quote; a process whose
  database disagrees with its configuration refuses to start; money that does
  not add up closes the rail until a person clears it.

Details and the attacks each layer refuses: [`docs/threat-model.md`](docs/threat-model.md).
Each money invariant with the tests that exercise it:
[`docs/money-invariants.md`](docs/money-invariants.md).

## How it fails, on purpose

| The gateway refuses when | Because |
|---|---|
| fewer than two independent price groups agree, or they disagree beyond policy | a rate nobody corroborated is a guess about someone's money |
| price, policy or rail-health evidence is missing or stale | an issued quote must rest on evidence that existed when it was issued |
| a merchant on the `own` policy has no active address of its own | a quote on someone else's address would pay a stranger |
| a rail is closed by an operator or by reconciliation | new obligations must not pile onto a rail under investigation |
| a transfer matches two reservations, or none | ambiguity is a decision for a person; money nobody can explain is queued, not absorbed |
| observers disagree about a chain event | the disagreement is the finding; no fact is made from it |
| a process's database does not describe its configured collectors, assets or environment | a database-only address change must not redirect receipts |

## Documentation

- [Architecture](docs/architecture.md) and [decisions](docs/decisions/)
- [Merchant integration](docs/merchant-integration.md): intents, quotes, statuses, webhook verification
- [Examples](examples/): webhook receivers, a create-payment script, a Postman collection
- [TypeScript SDK](sdk/typescript/): typed client, webhook verification and exact amount helpers (`@crypto-gateway/sdk`)
- [Scope and limits](docs/scope-and-limits.md): what is supported and every exceptional payment case
- [Money invariants](docs/money-invariants.md): each invariant and the tests that falsify it
- [OpenAPI 3.1](docs/openapi.json): every route the router serves, checked by a test
- [Operator runbook](docs/operator-runbook.md): feeding evidence, reading queues, onboarding and rotation procedures, clearing a hard stop, alerts
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
unit tests, PostgreSQL scenarios and seeded property tests. That is not
production readiness: no release has been tagged, no rail has run a sustained
testnet soak with two genuinely independent providers, and no external audit
has been done. Do not put real money through it until those steps in
[`docs/owner-setup.md`](docs/owner-setup.md) are complete.

## License

Apache License 2.0. See [`LICENSE`](LICENSE).
