# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/) once it is released.

## [Unreleased]

What exists today, by the slice that added it.

### Added

- **Open-source packaging.** README for people and search engines, OpenAPI
  3.1 for every served route with a test that the router and the document
  cannot drift, merchant integration and operator runbooks, a threat model,
  the owner's setup order, security policy, contributing guide, Contributor
  Covenant, issue and pull request templates, a migrate-only mode for the API
  binary, and development scripts for an operator key and a testnet rail.
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
