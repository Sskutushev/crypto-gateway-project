# Owner setup: every account, key and registration, in order

Everything the code cannot provide for itself, with the environment variable
or GitHub secret each one maps to. **Blocking** means a first real payment
cannot happen without it; **optional** means the gateway runs without it and
records that it did.

## 1. Repository identity — blocking for a public release

- **Name and description.** GitHub → Settings. Suggested description:
  *Open-source, non-custodial crypto payment gateway in Rust: exact integer
  money, independent chain evidence, signed webhooks, fail-closed. USDT TRC20
  first.*
- **Topics** (Settings → Topics): `crypto-payments`, `payment-gateway`,
  `usdt`, `tron`, `non-custodial`, `rust`, `postgresql`, `webhooks`,
  `self-hosted`, `stablecoin`.
- **Base branch.** `main` does not exist yet. Create it from the current
  branch and protect it:
  ```
  git branch main feat/standalone-gateway-foundation
  git push origin main
  ```
  Then Settings → Branches → add a rule for `main`: require a pull request,
  require the `ci` checks (`fmt, clippy, unit tests`, `PostgreSQL scenarios`,
  `fuzz smoke`, `cargo deny, cargo audit`, `compose and kustomize validate`,
  `image, SBOM, scan, publish`), require linear history, no force pushes.
  Set `main` as the default branch afterwards.
- **Security policy and private reporting.** Settings → Code security →
  enable *Private vulnerability reporting*; `SECURITY.md` points there.
- **CODEOWNERS** names you; keep it that way until a second maintainer exists.

## 2. Container registry — blocking for a deployment from CI

Images publish to `ghcr.io/<owner>/crypto-gateway-project` from a version tag
(`v0.1.0`). The workflow uses the built-in `GITHUB_TOKEN`; nothing to create.
Make the package public (Packages → package → settings) if the deployment
pulls without credentials, or create a read-only deploy token for the cluster
(`imagePullSecrets`) otherwise.

Release: `git tag v0.1.0 && git push origin v0.1.0`.

## 3. PostgreSQL — blocking

A managed PostgreSQL 16 with TLS. Create the database, apply
`db/roles/00_roles.sql` as the administrator, then one login role per
process (the commands are in that file), taking passwords from your secret
manager. Each becomes a `GATEWAY_DATABASE_URL` with `sslmode=verify-full`:

| Secret | Role group | Used by |
|---|---|---|
| `gateway-db-api` | `gateway_api` | `gateway-api` |
| `gateway-db-payment` | `gateway_payment` | `worker-expiry`, `worker-settlement`, `worker-outbox` |
| `gateway-db-observer` | `gateway_observer` (login named as the source's `db_principal`) | `worker-observer`, one per source |
| `gateway-db-verifier` | `gateway_verifier` (login named as the verifier source's `db_principal`) | `worker-verifier` |
| `gateway-db-reconciler` | `gateway_reconciler` | `worker-reconciler` |
| a migrator URL kept outside the cluster | `gateway_migrator` | the release step |

Migrations run as the migrator with `GATEWAY_MIGRATE_ONLY=true`; then apply
`db/roles/10_grants.sql`, and again after every later migration.

## 4. Two independent TRON data providers — blocking

Independence is counted by provider group: two keys from one vendor are one
opinion. Qualifying pairs: a hosted API such as TronGrid, plus a full node you
run (`own_node`) or a second vendor with its own infrastructure. The verifier
needs a third, or reuses the own node; it must not share the observer's
group.

For each provider, register a `chain_sources` row (`source_key`,
`provider_group`, `kind`, `db_principal`, `requires_dedicated_principal =
TRUE`) and, for observers, apply `db/roles/20_observer_source_role.sql.template`.
Then:

| Variable / secret | Where |
|---|---|
| `GATEWAY_OBSERVER_SOURCE_KEY`, `GATEWAY_TRON_BASE_URL`, `GATEWAY_TRON_API_KEY_HEADER`, `GATEWAY_TRON_LANE` | ConfigMap or the observer env file |
| `GATEWAY_TRON_API_KEY` | secret `gateway-tron-observer` |
| `GATEWAY_VERIFIER_SOURCE_KEY`, `GATEWAY_VERIFIER_TRON_BASE_URL`, `GATEWAY_VERIFIER_TRON_API_KEY_HEADER` | ConfigMap or the verifier env file |
| `GATEWAY_VERIFIER_TRON_API_KEY` | secret `gateway-tron-verifier` |

Plan for the block lane: one request per block plus one per matching
transaction; a free tier on the hosted API is enough for a testnet run and
not for mainnet.

## 5. The collector address — blocking

A TRON account you control, whose key lives in your treasury tooling and
never near this gateway. Give the gateway the public address only:

1. Insert the `collector_addresses` row (state `active`, the canonical bytes,
   `pinned_sha256 = sha256(bytes)`), and the `chain_assets` row for the token
   contract (USDT mainnet: `TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t`).
2. Set `GATEWAY_EXPECTED_COLLECTORS=<base58>` and
   `GATEWAY_EXPECTED_ASSETS=tron:mainnet:<contract base58>` for every process.
   The self-check refuses to start any process whose database and
   configuration disagree, in either direction.
3. Set `GATEWAY_CHAIN_ENVIRONMENT=mainnet` explicitly; it has no default.

Rotation: `active → receiving_only → retired` in the table, and the
allowlist updated at each step.

## 6. The webhook signing master key — blocking

```
openssl rand -hex 32
```

Store it as secret `gateway-webhook` (`GATEWAY_WEBHOOK_MASTER_KEY`), for the
outbox worker only. Every endpoint's signing secret is derived from it and
never stored; rotating the master key means re-issuing every merchant's
secret. Register merchant endpoints with `secret_version` and the fingerprint
of the derived secret.

## 7. Operator and merchant keys — blocking

Generate 32 to 256 random characters, hash with SHA-256, insert the hash:
`scripts/create-dev-operator.sql` and `scripts/create-dev-merchant.sql` show
the rows. In production, generate keys in your secret manager and insert only
hashes. Give Prometheus a `read` key, the price feeder an `ingest` key, and
`admin` to a person.

## 8. Price and rail-health feeds — blocking

Something must call `POST /v1/operator/price-snapshots` with readings from at
least two provider groups, and `POST /v1/operator/rail-health`, at least as
often as the quote policy's `max_price_age_seconds` and
`max_rail_health_age_seconds`. Until then every quote is refused, by design.
Two price sources with different infrastructure qualify (an exchange API and
an aggregator, for instance).

## 9. Policies and reference data — blocking

Per environment: `chain_finality_policies`, `quote_policies` (per asset and
currency), `payment_settlement_policies` with tiers. `scripts/seed-dev-rail.sql`
shows every row for the Nile testnet; mainnet values are a decision to write
down before the first payment: confirmations, evidence age, quote TTL, the
amount above which a person settles.

## 10. Screening provider — optional

A KYT vendor account, if you want screening before automatic settlement. Its
integration posts `POST /v1/operator/transfers/{id}/risk-evaluations` with an
`ingest` key. Without one, every transfer reads as `skipped`, and the
settlement tiers that require `allow` hold the payment for a person.

## 11. Legal — blocking for real money, outside this repository

The operating entity, supported payer jurisdictions, sanctions and KYC
obligations, and a written approval of the production settlement policy. The
gateway records decisions; it does not make these.

## Order

1 → 2 → 3 → 4 → 5 → 6 → 7 → 9 → deploy (`docs/deployment.md`) → 8 → first
testnet payment → 10 and 11 → mainnet.
