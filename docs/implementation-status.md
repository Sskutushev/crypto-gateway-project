# Implementation Status

Last updated: 2026-09-21

## Repository state

- Branch: `feat/standalone-gateway-foundation`
- Remote: `https://github.com/Sskutushev/crypto-gateway-project.git`
- Working tree: quote/amount-lease slice plus the bounded expiry scheduler are
  implemented and verified locally; changes are not committed or pushed.
- This tree is the repository's initial history; there is no earlier product
  implementation to preserve or migrate.

## Product decisions

- Standalone public product with no private application dependencies.
- Non-custodial inbound payments only.
- Rust workspace for the API, financial core, observers, verifier, workers,
  and reconciler.
- PostgreSQL is the sole operational source of truth.
- Merchant fulfillment is a signed webhook contract backed by a transactional
  outbox.
- Apache-2.0 is the initial license choice; the owner may change it before the
  first public release.
- First rail: USDT TRC20. ERC20 and TON are later adapters.

## Completed

- Independent product and trust boundaries documented.
- Persistent instructions and continuation guide added for future agents.
- Initial architecture decisions recorded.
- Canonical Apache-2.0 license text added from `apache.org`.
- Rust workspace, executable API, PostgreSQL migration, and local Compose
  environment created.
- Merchant API-key authentication and merchant-isolated create/read payment
  intent endpoints implemented.
- Merchant/route/key-scoped idempotency is transactional, detects request
  conflicts, survives concurrent same-key requests, and emits one audit event
  for the one created resource.
- Public fiat and token quantities use base-10 JSON strings; JSON numbers,
  signs, decimals, whitespace, zero, and overflow are rejected rather than
  coerced.
- Duplicate merchant references and idempotency conflicts return explicit
  `409` errors; malformed JSON returns an explicit `400` envelope.
- Local PostgreSQL and API ports bind only to loopback. The API has a 128
  request concurrency ceiling and a 15-second request timeout. Non-local
  deployment requires external TLS, edge rate limiting, and least-privileged
  database credentials.
- PostgreSQL-backed API coverage proves authentication, response key sets,
  string money, replay, conflicting reuse, duplicate references, tenant
  isolation, concurrent creation, and audit cardinality.
- The release Docker image builds and runs as UID 10001; `.dockerignore` keeps
  build context limited to required sources.
- Quote planning fails closed for missing, stale, future-dated, or unhealthy
  price, policy, and rail-health evidence.
- Fiat-to-token conversion uses checked 256-bit rational arithmetic and always
  rounds upward; exact-amount slot capacity is bounded to 10,000 per collector.
- Immutable quotes, payment attempts, active amount leases, and lease history
  are persisted with tenant/asset/collector consistency enforced by composite
  foreign keys.
- PostgreSQL serializes allocations per collector and enforces one active
  collector/raw-amount lease plus one lease per payment attempt.
- Quote expiry and late-payment retention are separate: attempts expire at the
  quote deadline, while their amount remains reserved until the late-payment
  deadline. Expired leases are archived and removed in the same transaction.
- Quote, attempt, payment-intent, allocation, expiry, and lease-archive
  transitions emit audit events.
- Immutable price, quote-policy, and rail-health snapshots are persisted and
  selected by the backend; issued quotes retain foreign keys to their exact
  evidence rows.
- `POST /v1/payment-intents/{intent_id}/quotes` accepts only an asset ID,
  rejects injected pricing facts and cross-merchant access, uses the existing
  API-key and idempotency boundaries, and pins its response key set in a real
  HTTP/PostgreSQL test.
- An issued quote replays during a later rail outage. Reusing its idempotency
  key for another asset returns a conflict before checking that asset's current
  availability.
- A dedicated `gateway-scheduler` crate drives quote expiry and lease archival.
  One sweep runs bounded batches until nothing is due or the per-tick ceiling is
  reached, and reports a remaining backlog instead of hiding it.
- Two sweeps cannot overlap: a single-flight guard rejects a concurrent sweep
  inside one process, the tick is delayed rather than doubled, and concurrent
  database sweeps archive each lease exactly once.
- Only storage outages are retried, with capped exponential backoff and an
  attempt ceiling; an invariant violation ends the sweep and is never retried.
- Scheduler counters cover started, succeeded, failed, skipped-overlapping and
  backlogged sweeps, executed batches, expired quotes, archived leases,
  transient retries, the consecutive failure streak, and the last successful
  sweep, which is absent rather than zero until one succeeds.
- `gateway-api` runs the scheduler, stops it on `SIGTERM` or `Ctrl-C` between
  batches, and joins it before exit. `GATEWAY_EXPIRY_*` settings are read at
  startup; an unreadable value fails startup instead of defaulting silently.

## In progress

- Dependency advisory scanning needs a reliable RustSec index connection; the
  local full `cargo deny check` stalled while fetching the advisory database.
- CI workflow and production deployment manifests have not been added.
- Production adapters that ingest price and rail-health snapshots are not yet
  implemented; current snapshot rows are test/development fixtures only.
- The scheduler lives inside the API process. It must move to its own worker
  deployment once the payment workers exist.
- Scheduler counters are in-process only; no metrics endpoint or scrape target
  exposes them yet.

## Next slices

1. Add authenticated internal adapters for immutable price and rail-health
   snapshot ingestion, with independent-source and freshness monitoring.
2. Expose scheduler and API health counters on an operator-only endpoint, and
   alert on the consecutive failure streak and on a stale last successful sweep.
3. Append-only chain observation ingestion.
4. Independent verifier and canonical transfer lifecycle.
5. Matching, allocation, ledger, and transactional webhook outbox.
6. TRON adapter with two independent sources and reconciliation.

## Verification

- Rust is not installed on the host; all Rust checks ran in the pinned
  `rust:1.90.0-bookworm` container.
- `cargo fmt --all -- --check`: passed after the scheduler changes.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed
  after the scheduler changes.
- `cargo test --workspace --locked`: passed (25 unit tests; three database
  tests are intentionally ignored in this command).
- PostgreSQL-backed quote/lease scenario: passed. It covers concurrent exact
  amount allocation, idempotent replay, merchant isolation, separate quote and
  late-payment deadlines, atomic lease archival, and audit cardinality. Its
  expiry and archival steps now run two sweeps at once and assert that each
  attempt and each lease is processed exactly once.
- PostgreSQL-backed scheduler scenario: passed. The scheduler drives the real
  quote service and repository, expires an issued quote, archives its lease,
  moves the payment intent to `expired`, and finds nothing left on a second
  sweep.
- Existing PostgreSQL-backed API contract/isolation scenario: passed after the
  new migrations and the scheduler changes.
- Live container run: the release image logged the scheduler's configured
  bounds at startup, stopped the loop on `SIGTERM`, and exited with code 0.
- The foundation's `cargo deny check bans licenses sources` passed. It was not
  rerun in the persistent development container because `cargo-deny` is not
  installed there; this slice adds no third-party dependency. `Cargo.lock`
  changed only by gaining the new local `gateway-scheduler` member, and the
  `tokio` features `sync` and `time` were enabled on the already locked
  version.
- Full `cargo deny check`: unavailable because fetching the RustSec advisory
  database stalled; the process was stopped after repeated no-output waits.
- `docker compose config --quiet`: passed; the live PostgreSQL port was
  verified as `127.0.0.1:54329`.
- `docker build --pull=false -t crypto-gateway-project:local .`: passed.
- The first final image export hit a transient Docker Desktop BuildKit error
  because a cached parent snapshot was missing. A non-destructive retry passed;
  no cache prune or source change was required.
- Workspace-level `python .agents/gate.py --tag core` still does not cover this
  ignored nested repository. Its last run, on 2026-09-21 before this slice,
  reported 8 passed, 2 failed, and 1 unavailable: stale root Cursor adapter
  generation, unrelated root hook and audit-packaging test setup errors, and the
  known Windows certificate-store failure in Semgrep. It was not rerun for this
  slice. The standalone repository checks above are authoritative.
- No blockchain adapter, verifier, matching, ledger, webhook delivery, or
  reconciliation exists yet; do not treat the gateway as deployable.

## External inputs still required

- Final public product name and package/container namespace.
- Hosting target and region.
- Managed PostgreSQL choice and credentials (later; local development does not
  need them).
- Two genuinely independent TRON data sources and API keys.
- Corporate collector wallet address for each enabled network. Public address
  only; never provide a private key or seed phrase.
- KYT provider/account and policy thresholds before automatic settlement.
- Legal entity, supported payer jurisdictions, sanctions/KYC requirements, and
  written approval before enabling a production payment policy.
- Public API hostname, webhook signing-domain choice, and email/alert channel.
