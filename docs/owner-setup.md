# Owner setup: every account, key and registration, in order

Everything the code cannot provide for itself, with the environment variable
or GitHub secret each one maps to. **Blocking** means a first real payment
cannot happen without it; **optional** means the gateway runs without it and
records that it did.

Merchants, merchant API keys, webhook endpoints and collector addresses are
created with the admin CLI, `gateway-worker admin <command>`. Everything else
in this list (assets, policies, chain sources, operator keys, risk-provider
bindings) has no command yet and is written as SQL by a person, as described
in its step.

## 1. Repository identity — blocking for a public release

- **Name and description.** GitHub → Settings. Suggested description:
  *Open-source, non-custodial crypto payment gateway in Rust: exact integer
  money, independent chain evidence, signed webhooks, fail-closed. USDT TRC20
  first.*
- **Topics** (Settings → Topics): `crypto-payments`, `payment-gateway`,
  `usdt`, `tron`, `non-custodial`, `rust`, `postgresql`, `webhooks`,
  `self-hosted`, `stablecoin`.
- **Base branch.** `main` is the base branch. In GitHub Settings, verify that
  `main` is the default branch and that its protection rule requires a pull
  request, the `ci` checks (`fmt, clippy, unit tests`, `PostgreSQL scenarios`,
  `fuzz smoke`, `cargo deny, cargo audit`, `compose and kustomize validate`,
  `image, SBOM, scan, publish`), linear history, and no force pushes.
  Repository documentation cannot prove those remote settings; inspect them
  before a release.
- **Security policy and private reporting.** Settings → Code security →
  enable *Private vulnerability reporting*; `SECURITY.md` points there.
- **CODEOWNERS** names you; keep it that way until a second maintainer exists.

## 2. Container registry — blocking for a deployment from CI

Images publish to `ghcr.io/<owner>/crypto-gateway-project` from a version tag
(`v0.1.0`). The workflow uses the built-in `GITHUB_TOKEN`; nothing to create.
Make the package public (Packages → package → settings) if the deployment
pulls without credentials, or create a read-only deploy token for the cluster
(`imagePullSecrets`) otherwise.

Release: `git tag v0.1.0 && git push origin v0.1.0`. No release has been
tagged yet.

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
| `gateway-db-retention` (only if retention runs) | `gateway_retention` | `worker-retention` |
| a provisioner URL kept with the people who onboard merchants | `gateway_provisioner` | `gateway-worker admin` |
| a migrator URL kept outside the cluster | `gateway_migrator` | the release step |

Migrations run as the migrator with `GATEWAY_MIGRATE_ONLY=true`; then apply
`db/roles/10_grants.sql`, and again after every later migration.

**The provisioner role.** `gateway_provisioner` is what the admin CLI
connects as:

```
CREATE ROLE gateway_provisioner_prod LOGIN PASSWORD '<from secrets>' IN ROLE gateway_provisioner;
```

Its grants are column-level. It can read merchants, keys, webhook endpoints,
collectors and assets; insert merchants, keys, endpoints, collectors, audit
events and the test event; and update only the columns that revoke a key,
rotate or disable an endpoint, and retire a collector. It cannot touch a
quote, a transfer or an allocation, cannot rewrite a key's hash, and cannot
move a merchant between collector policies. Do not run the CLI as the
migrator or a superuser in production: the role's refusals are part of what
keeps a mistyped command from reaching money.

## 4. Two independent TRON data providers — blocking

Independence is counted by provider group: two keys from one vendor are one
opinion. Qualifying pairs: a hosted API such as TronGrid, plus a full node you
run (`own_node`) or a second vendor with its own infrastructure. The verifier
needs a third, or reuses the own node; it must not share the observer's
group.

For each provider, register a `chain_sources` row (`source_key`,
`provider_group`, `kind`, `db_principal`, `requires_dedicated_principal =
TRUE`) with SQL — there is no command for sources — and, for observers,
apply `db/roles/20_observer_source_role.sql.template`. Then:

| Variable / secret | Where |
|---|---|
| `GATEWAY_OBSERVER_SOURCE_KEY`, `GATEWAY_TRON_BASE_URL`, `GATEWAY_TRON_API_KEY_HEADER`, `GATEWAY_TRON_LANE` | ConfigMap or the observer env file |
| `GATEWAY_TRON_API_KEY` | secret `gateway-tron-observer` |
| `GATEWAY_VERIFIER_SOURCE_KEY`, `GATEWAY_VERIFIER_TRON_BASE_URL`, `GATEWAY_VERIFIER_TRON_API_KEY_HEADER` | ConfigMap or the verifier env file |
| `GATEWAY_VERIFIER_TRON_API_KEY` | secret `gateway-tron-verifier` |

Plan for the block lane: one request per block plus one per matching
transaction; a free tier on the hosted API is enough for a testnet run and
not for mainnet.

## 5. The asset and the collector policy — blocking

**The asset** has no command. Insert the `chain_assets` row for the token
contract with SQL (USDT mainnet: `TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t`),
`status = 'active'`, the canonical contract bytes and
`pinned_sha256 = sha256(bytes)`; `scripts/seed-dev-rail.sql` shows the row
for Nile. Set for every process:

- `GATEWAY_EXPECTED_ASSETS=tron:mainnet:<contract base58>`;
- `GATEWAY_CHAIN_ENVIRONMENT=mainnet`, explicitly; it has no default.

**Whose address receives the money** is a per-merchant decision, the
merchant's `collector_policy`:

| Policy | Quotes use | Who holds the key | What you owe |
|---|---|---|---|
| `own` (the default for every new merchant) | only addresses registered to that merchant | the merchant | nothing: the payer pays the merchant directly |
| `shared` | only operator addresses (no merchant) | you | the merchant's money, settled outside this system |

There is no fallback between the two: a merchant on `own` with no active
address of its own gets `503 quote_unavailable`, never a quote on your
address, and the database refuses a quote on a collector that does not
receive money for that merchant. Merchants that existed before migration 0015
were set to `shared`, the behaviour they were quoted under. The policy is
chosen at `merchant-create` and the provisioner role cannot change it; a
change is a deliberate SQL decision by the owner.

**A merchant's own address** is registered on proof that the merchant
controls it. The merchant, not you, signs:

1. You build the statement (it touches no database):

   ```
   gateway-worker admin collector-statement --merchant <uuid> --address <T...>
   ```

   It prints `statement`, `issued` and `valid_for_hours` (24). The statement
   names the gateway purpose, the merchant id, the address and the issue
   time, so a signature cannot be replayed for another merchant, address or
   moment.
2. The merchant signs the `statement` text exactly, in TronLink with
   `signMessageV2` (TIP-191), with the wallet that holds the address, and
   returns the hex signature.
3. You register it within 24 hours of `issued`:

   ```
   gateway-worker admin collector-register --actor <you> --asset <asset uuid> \
     --address <T...> --merchant <uuid> --issued <issued> --signature <hex>
   ```

   The gateway recovers the signer and compares it with the address as
   canonical bytes; a wrong signer, a statement older than 24 hours or more
   than five minutes in the future, or a malformed signature is refused.
   The verified proof is written into the audit row.

If the merchant cannot sign (a custody provider, a hardware wallet without
message signing), `--manual-evidence '<who checked and how>'` replaces
`--signature`/`--issued`. It is accepted only with that named reason, which
is recorded; treat it as the exception it is.

A merchant-owned address is pinned by the start-up self-check (its stored
bytes must re-hash to their pin) but is not listed in
`GATEWAY_EXPECTED_COLLECTORS`.

**An operator address** (for `shared` merchants) is a TRON account you
control, whose key lives in your treasury tooling and never near this
gateway:

```
gateway-worker admin collector-register --actor <you> --asset <asset uuid> \
  --address <T...> --manual-evidence '<who verified control of the key, and how>'
```

Then set `GATEWAY_EXPECTED_COLLECTORS=<base58>[,<base58>...]` for every
process. It names operator collectors only, in both directions: a process
whose database and configuration disagree refuses to start, and the API's
readiness fails until they agree, so plan registration and the configuration
change as one deployment. A deployment where every address belongs to a
merchant sets `GATEWAY_EXPECTED_COLLECTORS=none`; an empty value is an error,
so a forgotten variable is never read as that decision.

Retirement: `gateway-worker admin collector-stop-quoting`, then, once no
reservation remains, `gateway-worker admin collector-retire --actor <you>
--collector <uuid> --reason <text>`; see the runbook.

## 6. The webhook signing master key — blocking

```
openssl rand -hex 32
```

Store it as secret `gateway-webhook` (`GATEWAY_WEBHOOK_MASTER_KEY`). Two
things need it: the outbox worker, to sign deliveries, and the admin CLI, for
`webhook-add` and `webhook-rotate`, which derive an endpoint's secret from it
and store only its fingerprint. No other process should hold it. Rotating
the master key itself means re-issuing every merchant's secret; per-endpoint
rotation (`webhook-rotate`) does not touch the master key.

## 7. Operator and merchant keys — blocking

**Merchants and their keys** through the CLI, as the provisioner role:

```
gateway-worker admin merchant-create --actor <you> --external-id <id> --name <display name> [--collector-policy own|shared]
gateway-worker admin api-key-issue   --actor <you> --merchant <uuid> --label <text>
gateway-worker admin webhook-add     --actor <you> --merchant <uuid> --url https://<host>/<path> [--description <text>]
gateway-worker admin webhook-test    --actor <you> --endpoint <uuid>
```

`merchant-create` is idempotent on `--external-id`: the same request returns
the merchant it made (`"created": false`), a different name or policy under
the same id is refused. `api-key-issue` prints a `gw_...` secret once; the
gateway keeps its SHA-256. `webhook-add` refuses a URL that delivery would
refuse (not https, not port 443, credentials, query or fragment) and prints
the signing secret once. Hand both secrets to the merchant over a channel you
would trust with a password.

**Operator keys** have no command. Generate 32 to 256 random characters in
your secret manager, hash with SHA-256 and insert only the hash;
`scripts/create-dev-operator.sql` shows the row. Give Prometheus a `read`
key, the price/rail-health feeder an `ingest` key, a separate `risk_ingest`
key to each KYT integration, and `admin` to a person. Bind every risk key to
its exact provider in `operator_risk_provider_bindings` (SQL); an unbound key
cannot submit a verdict.

## 8. Price and rail-health feeds — blocking

Something must call `POST /v1/operator/price-snapshots` with readings from at
least two provider groups, and `POST /v1/operator/rail-health`, at least as
often as the quote policy's `max_price_age_seconds` and
`max_rail_health_age_seconds`. Until then every quote is refused, by design.
Two price sources with different infrastructure qualify (an exchange API and
an aggregator, for instance).

## 9. Policies and reference data — blocking

Per environment, with SQL: `chain_finality_policies`, `quote_policies` (per
asset and currency), `payment_settlement_policies` with tiers.
`scripts/seed-dev-rail.sql` shows every row for the Nile testnet; mainnet
values are a decision to write down before the first payment: confirmations,
evidence age, quote TTL, the amount above which a person settles.

## 10. Screening provider — optional

A KYT vendor account, if you want screening before automatic settlement. Its
integration posts `POST /v1/operator/transfers/{id}/risk-evaluations` with its
provider-bound `risk_ingest` key. Without one, every transfer reads as
`skipped`, and the settlement tiers that require `allow` hold the payment for
a person.

## 11. Testnet soak and audit — blocking for real money

A sustained Nile run with genuinely independent providers, real merchant
addresses registered by signature, and every exceptional case in
`docs/scope-and-limits.md` exercised by hand; then an external security
review. Repository tests do not substitute for either.

## 12. Legal — blocking for real money, outside this repository

The operating entity, supported payer jurisdictions, sanctions and KYC
obligations, and a written approval of the production settlement policy. The
gateway records decisions; it does not make these.

## Order

1 → 2 → 3 → 4 → 5 (asset, and operator collectors if any merchant will be
`shared`) → 6 → 9 → deploy (`docs/deployment.md`) → 8 → 7 and 5 (merchants,
keys, endpoints, merchant addresses) → first testnet payment → 10 and 11 →
12 → mainnet.
