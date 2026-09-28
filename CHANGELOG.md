# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/) once it is released.

## [Unreleased]

No version has been released yet. The first three groups below are the
changes since the P0 hardening branch; the last `Added` group is what existed
before them, by the slice that added it.

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
