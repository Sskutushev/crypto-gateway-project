# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/) once it is released.

## [Unreleased]

No version has been released yet. The first three groups below are the
changes since the P0 hardening branch; the last `Added` group is what existed
before them, by the slice that added it.

### Fixed

- **A transfer short of depth is read again until it is final.** Each lane
  reads a block once and a repeated answer is a duplicate, so a finality
  policy asking for more confirmations than the first readings showed (the
  Nile seed asks for 19 on top of `finalized`) left a transfer at `confirmed`
  with nothing to advance it. A verified event that is not finalized is now
  due again 15 seconds after its last verdict; the verifier reads the chain
  again, and its own reading counts the head in its identity, so a deeper
  answer is stored as new evidence.

### Added

- **Address pool and capacity.** A quote goes to the merchant's least loaded
  active address and moves to the next (up to eight) when every exact amount
  near its price is taken. `GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR` caps the
  reservations one address may hold; a merchant whose addresses are all full
  gets `503 quote_capacity_exhausted` with `Retry-After`. `/metrics` gains
  open and live reservations per address, quote outcomes, spillovers and a
  quote latency histogram.
- **Request budgets.** Per-merchant token buckets for writes and reads,
  per-client-address budgets for failed authentications and the payment
  page, all answering `429 rate_limited` with `Retry-After`; a 64 KiB request
  body limit and a 16 KiB `metadata` limit.
- **TypeScript SDK.** `sdk/typescript` is the npm package
  `@crypto-gateway/sdk` (zero runtime dependencies, Node 18+, ESM and
  CommonJS): a typed client for every merchant route, typed errors with the
  API's error codes and `Retry-After`, retries only for idempotent requests,
  exact `BigInt` amount formatting and parsing, and `verifyWebhook` with
  constant-time comparison, secret rotation and a typed union of every event
  the gateway emits. CI builds and tests it on Node 18, 20 and 22, and runs
  the TypeScript webhook receiver example's tests.
- **Documentation site.** `site/build.py` (Python standard library only)
  renders `docs/*.md` and `docs/openapi.json` into a static site with titles,
  descriptions, canonical URLs, Open Graph tags, a sitemap and `robots.txt`;
  `.github/workflows/pages.yml` publishes it to GitHub Pages from `main` and
  fails a pull request on a broken internal link.
- **Release process.** `.github/workflows/release.yml` turns a `vX.Y.Z` tag
  into a GitHub Release with the matching `CHANGELOG.md` section (refused when
  it is missing), the image digest and the SBOM, after the `ci` run of the tag
  succeeds. `docs/releasing.md` covers versioning, the compatibility matrix,
  the migrate-only upgrade and rollback rules.
- **Signed images.** Tagged images are signed by digest with cosign keyless
  signing; `docs/releasing.md` shows how to verify one.
- **Backup and restore.** `docs/backup-and-restore.md` (recovery objectives,
  point-in-time recovery, what to check after a restore, a monthly drill) and
  `scripts/backup-drill.sh`, which dumps a database, restores it into a
  scratch database, runs migrations on it and checks that the books still add
  up.
- **WooCommerce plugin.** `integrations/woocommerce/crypto-gateway-usdt/` is a
  payment method for WooCommerce 8+ on PHP 8.1+, compatible with HPOS and the
  Cart/Checkout Blocks. Checkout creates an intent and a quote with
  idempotency keys derived from the order, converts the total to minor units
  with string arithmetic, and redirects to the hosted checkout page; an
  expired quote is re-quoted on the same intent from the order page. A signed
  webhook (raw-body HMAC, several `v1` values, 300-second tolerance, event
  deduplication, per-order lock) and a five-minute WP-Cron status check apply
  the same mapping: `paid` completes the order once, `partially_paid` puts it
  on hold, `cancelled` cancels an unpaid order, `OVERPAID` adds a note.
  PHPUnit tests and a CI job cover signature verification, minor-unit
  conversion and the order-state mapping.
- **Chain simulator for integration tests.** `tools/chain-simulator` is a
  deterministic, in-memory TRON node (Python standard library) serving the
  calls the TRON source makes, with a control API (loopback or token only)
  that mines a transfer, a wrong amount or a reverted transaction; it refuses
  to mint the real USDT contracts. `scripts/seed-simulator-rail.sql` seeds a
  `simulator`/`testnet` rail, the Compose profile `simulator` runs it on its
  own database, `scripts/simulate-payment.py` pays a quote and follows the
  intent, and `tools/chain-simulator/e2e.sh` runs the whole unmodified
  pipeline end to end; CI runs the simulator's tests and that run. Testing
  only: it proves nothing about the real chain. No gateway code changed.
- **Two operators for a manual honor.** An honor allocating at least
  `GATEWAY_MANUAL_HONOR_DUAL_CONTROL_MIN_RAW` raw units (every honor when it
  is unset) is refused with `dual_control_required`. One `admin` key proposes
  it at `/v1/operator/manual-honor-proposals`, which checks it against locked
  rows and stores the command with an evidence snapshot without writing money;
  a different `admin` key approves it, re-running every honor check under
  fresh locks, or rejects it. Proposals expire after 24 hours; both
  identities, both reasons and the resolution are recorded (migration 0020).
- **Webhook redelivery as the same event.**
  `POST /v1/operator/webhook-events/{event_id}/redeliver` and
  `gateway-worker admin webhook-redeliver` re-queue a delivered or
  dead-lettered event in place, same id and payload, for one active endpoint
  of its merchant or all of them, with a fresh retry budget, an idempotency
  key, a `webhook_redeliveries` row and an audit row (migration 0019).
- **Accounting export.** `GET /v1/operator/accounting/settlements` returns
  settled money per merchant, asset, fiat currency and UTC day over at most 92
  days, as JSON or CSV, with totals and a control sum taken from the
  allocation rows; a disagreement is reported as `balanced: false`.
- **Admin list commands.** `merchant-list`, `api-key-list`, `webhook-list` and
  `collector-list` (with open reservations), bounded and cursor-paged, printing
  no secret or hash.
- **Retention.** A `retention` worker role, running as the new
  `gateway_retention` database role, deletes old webhook delivery attempts,
  final unreferenced chain observations and superseded health transitions in
  bounded, audited batches. Nothing is deleted unless an age of at least 30
  days is configured; attested and conflicting observations, and every attempt
  of an undelivered event, are always kept.
- **Fair webhook delivery.** A batch takes at most
  `GATEWAY_WEBHOOK_MAX_EVENTS_PER_MERCHANT` events of one merchant and
  delivers up to `GATEWAY_WEBHOOK_DELIVERY_CONCURRENCY` merchants at once, so
  one slow endpoint no longer delays every other merchant.
- **Collector retirement without SQL.** `collector-stop-quoting` moves an
  address to `receiving_only` (never quoted again, still watched), audited.
  `collector-retire` refuses while any amount reservation on the address
  can still be paid, unless `--compromised yes` is given, which records the
  open reservations in the audit row.
- **Admin CLI for onboarding.** `gateway-worker admin <command>` creates
  merchants, issues and revokes API keys, adds, rotates, disables and tests
  webhook endpoints, and registers and retires collector addresses, without
  hand-written SQL. Every command names its actor, is written to
  `audit_events` in the same transaction as its change, and prints one JSON
  object; secrets are shown once and only hashes or fingerprints are stored.
- **Merchant-owned collectors.** A merchant's `collector_policy` decides
  whose address receives its money: `own` (only addresses registered to that
  merchant; the default for new merchants) or `shared` (only operator
  addresses). Migration 0015 adds the policy, `collector_addresses.merchant_id`
  and a trigger that refuses a quote on a collector that does not receive
  money for its merchant.
- **Ownership proof for a merchant address.** `collector-statement` builds a
  statement naming the gateway purpose, the merchant, the address and the
  issue time; the merchant signs it in TronLink (`signMessageV2`, TIP-191);
  `collector-register --issued --signature` verifies the signer against the
  address and accepts statements up to 24 hours old. A manual check is
  accepted only with a named, recorded reason.
- **Webhook secret rotation per endpoint.** Migration 0016 adds a key ring:
  after `webhook-rotate`, every delivery is signed with both the new and the
  previous secret (`t=...,v1=<new>,v1=<previous>`) until the chosen
  transition period ends (1 to 720 hours, 72 by default).
- **`webhook.test` event**, queued by `webhook-test` and delivered through the
  normal outbox, to confirm delivery and signature verification.
- **`gateway_provisioner` database role** for the admin CLI, with
  column-level grants: it can create, revoke, rotate and retire, and is
  refused the money path, key hashes and collector policies.
- **Examples.** Webhook receivers in TypeScript and Python (several `v1`
  values, timestamp tolerance, deduplication, tests), a create-payment script
  in shell and TypeScript, and a Postman collection for every route.
- **Documentation.** `docs/money-invariants.md` (each invariant with the tests
  that exercise it) and `docs/scope-and-limits.md` (what is supported and
  every exceptional payment case).

### Changed

- **A tagged image is published only when `CHANGELOG.md` has its section and
  `Cargo.toml` carries the same version** (`scripts/release-notes.py`).
- **Manual `honor` is bound to its attempt.** The transfer must be at the
  attempt's collector, in its quote's asset, on its chain, network and
  environment; the attempt and intent must still be open for money; and the
  transfer's block must be before `late_payment_until`. All of it is read from
  locked rows; any mismatch is a `409` and nothing is written.
- **Explicit payment state transitions.** Paid and partially-paid moves
  happen only from an explicit set of states and must change exactly one row;
  a refusal rolls back the whole settlement before a fulfilment claim, event
  or webhook is written.
- **Automatic settlement parks what it cannot apply.** A transfer whose intent
  or attempt stopped being payable is held with a `manual_required` decision
  for a person, instead of being retried forever.
- **Allocations carry their collector.** Migration 0014 adds the collector to
  allocations and claims, with composite foreign keys to both the attempt and
  the transfer.
- **Quote expiry no longer stalls on a partially paid intent.** The attempt
  always expires; an intent that already has money on it keeps its status and
  the closure is audited as `payment_intent.quote_window_closed`.
- **Lateness is judged by the block.** A payment is late only when its block
  came at or after the quote's `expires_at`, so an on-time payment is no
  longer sent to a person because the expiry sweep happened to run before
  settlement. The manual `honor` window uses the same half-open boundary
  (block time before `late_payment_until`).
- **Self-check scope.** Every active or receiving-only collector is still
  pinned, but `GATEWAY_EXPECTED_COLLECTORS` now names operator collectors
  only; `none` declares a deployment where every address belongs to a
  merchant. Merchants that existed before migration 0015 are `shared`.
- **Quickstart and owner setup** onboard through the admin CLI instead of SQL
  where a command exists.

### Security

- A manual honor locked the immutable `chain_transfers` row, which needs an
  UPDATE privilege the least-privilege API role does not hold, so an honor
  through the API failed with `storage_unavailable` in a deployment that
  applied `db/roles`. It now locks the attempt, the intent and the transfer's
  processing row, and a scenario runs an honor under the API login.
- The provisioner role can no longer read merchant API key hashes; it reads
  only the columns the list commands print.

- Webhook delivery ignores system proxy settings, which would otherwise
  bypass the public-address pinning, and bounds DNS resolution time.
- A webhook URL is validated at registration against the same policy delivery
  enforces (https, port 443, no credentials, query or fragment).
- The database refuses a quote on another merchant's address, independently
  of the application check.
- Documented supply-chain advisory ignores, with a CI guard that fails if an
  advisory ignored as not compiled ever starts being compiled.

### Added (before the changes above)

- **Open-source packaging.** README for people and search engines, OpenAPI
  3.1 for every served route with a test that the router and the document
  cannot drift, merchant integration and operator runbooks, a threat model,
  the owner's setup order, security policy, contributing guide, Contributor
  Covenant, issue and pull request templates, a migrate-only mode for the API
  binary, and development scripts for an operator key and a testnet rail.
- **Manual resolution of parked money.** `honor`, `reject` and
  `record_remainder_disposition` through one idempotent, audited admin route;
  provider-bound `risk_ingest` keys for screening decisions.
- **Roles, deployment and CI.** Least-privilege PostgreSQL roles as applied
  SQL with a scenario that proves every refusal by SQLSTATE; row level
  security on chain cursors; a production Compose topology and a Kubernetes
  kustomization with one Deployment per role, probes on the self-check and
  network policies; a CI pipeline with formatting, lints, unit tests,
  PostgreSQL scenarios, seeded property tests, dependency policy, manifest
  validation, an SBOM and an image scan; images published from version tags.
- **Reconciliation scenarios and the start-up self-check.** One PostgreSQL
  scenario per reconciliation check; both binaries prove pinned collectors and
  assets, chain environment, finality policies, cursor sanity and clock skew
  before serving or taking a lease, and readiness re-evaluates the report.
- **Operator reads and Prometheus.** Cross-merchant health, conflicts,
  unmatched transfers, held payments, dead letters, reconciliation and a
  complete evidence bundle per payment, through keyset pages; a `/metrics`
  scrape that fails as a whole when storage cannot answer.
- **Component health and reconciliation.** Every component publishes its
  state; reconciliation runs eight checks over a window, separates counter
  findings from money findings, and closes the rail on money that does not
  add up.
- **The operator surface.** Operator keys with `ingest`, `read` and `admin`
  scopes; prices aggregated from independent provider groups with a deviation
  ceiling; rail health, screening decisions and rail stops with an
  attributable write path.
- **The worker runtime.** One binary, roles selected by configuration, one
  shutdown path, per-role bounds, a refusal to start under a foreign chain or
  environment.
- **The TRON HTTP source.** Transaction logs as the only reading, two scan
  lanes, no retries inside the client, hostile fixtures.
- **Canonical TRON addresses, leased workers, signed webhook delivery.**
- **Matching and settlement in one transaction**, with exact-amount
  reservations, memo and historical-slot matching, settlement bands and a
  transactional outbox.
- **Chain evidence intake and the independent verifier.** Immutable sources,
  append-only observations under a database-enforced principal, fenced
  cursors, canonical transfers from independent agreement plus a re-read.
- **Quotes and exact-amount leases** with a bounded expiry scheduler.
- **The foundation.** Merchant API keys, isolated payment intents,
  transactional idempotency, integer money everywhere.

[Unreleased]: https://github.com/Sskutushev/crypto-gateway-project/commits/main
