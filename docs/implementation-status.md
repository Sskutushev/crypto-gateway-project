# Implementation Status

Last updated: 2026-09-24

## Repository state

- Branch: `feat/standalone-gateway-foundation`
- Remote: `https://github.com/Sskutushev/crypto-gateway-project.git`
- Working tree: clean after the roles, deployment and CI slice.
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

- Independent product and trust boundaries documented, with ADRs.
- Rust workspace: domain, application, storage, HTTP, scheduler, webhook
  delivery and the TRON address layer, plus the executable API.
- Merchant API-key authentication, merchant-isolated payment intents and
  transactional idempotency scoped by merchant, route and key.
- Exact integer money everywhere; public JSON carries decimal strings, and
  JSON numbers, signs, decimals, whitespace, zero and overflow are rejected.
- Quotes are issued from server-owned price, policy and rail-health snapshots
  and fail closed when any of them is missing, stale, future-dated or
  unhealthy. Fiat-to-token conversion is checked 256-bit rational arithmetic
  that always rounds up.
- One exact-amount lease per collector and amount, one lease per attempt, with
  PostgreSQL arbitrating allocation. Quote expiry and the late-payment window
  are separate deadlines, and an expired lease is archived and released in one
  transaction.
- A bounded expiry scheduler with a single-flight guard, capped retries for
  storage outages only, counters, and shutdown between batches.
- Chain evidence intake: immutable sources, append-only observations
  deduplicated by the identity of the claim, durable per-source cursors, and
  component leases with fence tokens. The database fills the writing principal,
  and row level security proves a compromised observer cannot speak for another
  source.
- The verifier creates a canonical fact only when independent provider groups
  agree, its own re-read of the chain agrees with them, and the evidence is
  fresh. Disagreement becomes a recorded conflict; an impostor token, a failed
  transaction or a foreign recipient is refused; confirmation depth decides
  finality; state advances by compare-and-swap, so a late confirmation cannot
  pull a finalized transfer backwards.
- Matching ties a transfer to at most one obligation: memo first, then a live
  exact-amount reservation, then the reservation that held the slot when the
  block was produced. Two candidates stop the machine instead of being guessed
  between, and money nobody can explain is recorded and queued.
- Settlement bands decide how much independent evidence an amount of a given
  size needs. Claim, allocation, statuses, the fulfilment claim, the decision,
  payment events and the outbox commit in one transaction, and the database
  refuses an allocation that would exceed its transfer.
- Signed webhook delivery: secrets are derived from a deployment master key and
  never stored, a fingerprint mismatch refuses to sign rather than send an
  unverifiable signature, deliveries retry with capped backoff and are
  dead-lettered instead of retried forever, and an event nobody listens to is
  never called delivered.
- A generic leased worker runtime: observation intake, verification, settlement
  and outbox delivery all run as bounded batches under a fenced lease, with
  retries only for transient failures and a counted "not the leader" state.
- Canonical TRON addresses: base58check, both hex forms and the 20-byte log
  form reduce to the same bytes, and a mistyped address is refused.
- The TRON HTTP source. One type serves the observer's scanner and the
  verifier's re-read, and both end at a transaction's event log, because only
  the log carries the event index a canonical fact is identified by. Two lanes:
  the block lane walks solidified blocks under a durable cursor and never
  advances past what the chain can still replace; the address lane asks a
  provider which transactions touched a collector and then reads those
  transactions' logs. Nothing retries inside the client, an undecodable answer
  is a permanent refusal, an absent execution result is a refusal rather than a
  success, and an unknown token keeps its own address and no scale.
- The worker runtime. `gateway-worker` runs the roles named by
  `GATEWAY_WORKER_ROLES` (expiry, observer, verifier, settlement, outbox,
  reconciler) with one shutdown path, per-role bounds, and a refusal to start
  under a source row that describes another chain or environment. The chain
  environment has no default.
- The operator surface. Operator keys are their own credential with `ingest`,
  `read` and `admin` scopes. Prices are submitted as readings and aggregated by
  policy: independence counted by provider group, the exact rational mean of
  the two middle readings, a deviation ceiling, and a refusal that still stores
  every reading with the reason it did not count. A snapshot is only as fresh
  as its stalest input. Rail health and screening decisions have the same
  attributable write path.
- Rail stops. A closed rail stops new quotes and leaves issued ones payable.
  Reopening requires a person and a recorded reason.
- Component health and reconciliation. Every component publishes its state and
  every transition is an event; reconciliation runs eight checks over a window
  and separates findings about counters from findings about money. Money that
  does not add up closes the rail by itself.
- Operator reads and Prometheus exposition. A separately scoped operator key
  can inspect cross-merchant health, conflicts, unmatched transfers, held
  payments, dead letters, reconciliation and a complete payment evidence
  bundle through bounded UUID keyset pages. The scrape exposes the same live
  database state plus in-process expiry counters, and fails as a whole when
  storage cannot answer so absent telemetry never resembles health.
- One PostgreSQL scenario per reconciliation check. Each drives the real
  pipeline to a settled payment, breaks exactly the one thing its check exists
  to find, and asserts the finding's keys, the run row, the stored discrepancy
  and the rail stop; then it corrects the state and proves a second run is
  quiet while the stop it opened stays open, because only a person clears one.
  A fulfilment with no settlement behind it is recorded as a hard stop that
  names no rail, and the scenario says so.
- Start-up self-check. Both binaries recompute the pins of every receiving
  collector and active asset from stored bytes and require them to be named by
  `GATEWAY_EXPECTED_COLLECTORS` and `GATEWAY_EXPECTED_ASSETS`, in both
  directions; every active source, asset and collector must carry the process's
  `GATEWAY_CHAIN_ENVIRONMENT`; every active asset needs an active finality
  policy; no block cursor may stand ahead of the head its own source reported;
  and PostgreSQL and the process may not disagree by more than
  `GATEWAY_MAX_CLOCK_SKEW_SECONDS`. A failed check stops the process before it
  serves traffic or takes a lease, `GET /health/ready` answers 503 with every
  row and its reason, and a passed report is cached for at most ten seconds
  under a visible `evaluated_at`.
- Least-privilege database roles as applied SQL. `db/roles/00_roles.sql`
  creates the migrator, api, observer, verifier, payment, reconciler and
  readonly groups; `10_grants.sql` moves every table under the migrator and
  grants each group exactly the tables its crate reads and writes;
  `20_observer_source_role.sql.template` binds one login role per chain
  source to its `db_principal`. Migration 0011 puts row level security on
  `chain_cursors`, so an observer moves its own recovery point and nobody
  else's. Two PostgreSQL scenarios apply migrations and role SQL twice, then
  connect as each group and prove every forbidden write is refused with
  SQLSTATE 42501.
- Deployment. A production Compose topology (API plus one worker per role,
  read-only containers, no database container, env files with no secrets), a
  Kubernetes kustomization (a Deployment per role, Service and disruption
  budget for the API, probes on the self-check, restricted pod security,
  network policies per role), the image built with both binaries, and
  `docs/deployment.md` with the topology, the TLS boundary, the order of
  operations and rotation.
- CI and supply chain. `.github/workflows/ci.yml` runs fmt, clippy and unit
  tests; every PostgreSQL scenario against a service container; seeded
  property tests over the money parser, the TRON address codec and the
  event-log decoder at two hundred thousand iterations (a stand-in for
  cargo-fuzz, which needs a nightly toolchain); cargo deny for licences, bans
  and sources with advisories and cargo audit reported but not blocking;
  compose and kustomize validation; the image with an SPDX SBOM and a Trivy
  scan; publication to GHCR from a version tag. Dependabot watches Cargo,
  Actions and the base images.

## In progress

- Dependency advisory scanning needs a reliable RustSec index connection; the
  local full `cargo deny check` stalled while fetching the advisory database.
- Risk screening has an attributable push path and no provider behind it: a
  transfer nobody screened reads as skipped, which the settlement bands treat
  as not screened, never as clean.

## Next slices

1. Open-source packaging: README for people and search, SECURITY, CONTRIBUTING,
   OpenAPI, quickstart, and the owner's key and account instructions.
2. A testnet run with two real providers, then a second independent TRON
   provider group, then the ERC20 adapter.

## Verification

- Roles, deployment and CI (2026-09-24), in `crypto-gateway-dev`: fmt clean;
  clippy with `-D warnings` clean; `cargo test --workspace --locked` passed
  165 tests with 27 ignored, the seven `fuzz_smoke` tests among them at the
  default 2 000 iterations; the same seven passed at
  `GATEWAY_FUZZ_ITERATIONS=200000` in release in 2.8 s of test time; all 27
  PostgreSQL scenarios passed (3 HTTP, 24 storage), including
  `roles::migrations_and_role_sql_apply_twice` and
  `roles::each_role_is_refused_the_writes_that_are_not_its_own`. On the host:
  `docker compose -f deploy/compose.production.yaml config --quiet` passed
  with env files copied from the examples; `kubectl kustomize deploy/k8s`
  rendered 17 objects; `docker build` produced an image carrying both
  `gateway-api` and `gateway-worker`. Not run: `cargo deny` and `cargo audit`
  (not installed in the container; CI runs them), and the workflow itself,
  which runs on the first push to GitHub.

- Reconciliation scenarios and start-up self-check (2026-09-24), in
  `crypto-gateway-dev`: `cargo fmt --all -- --check` clean;
  `cargo clippy --workspace --all-targets --locked -- -D warnings` finished
  with no warnings; `cargo test --workspace --locked` passed 158 tests with 25
  ignored; and
  `GATEWAY_TEST_DATABASE_URL=postgres://gateway:gateway@host.docker.internal:54329/gateway cargo test --workspace --locked -- --ignored`
  passed all 25 PostgreSQL scenarios (3 HTTP, 22 storage) with no failures,
  among them the nine reconciliation scenarios in
  `crates/gateway-storage/src/oversight/tests.rs`, the self-check scenario
  that breaks each invariant in turn, and the readiness route answering 503
  with the failed report.

- Operator read/metrics slice (2026-09-23):
  owner-review fixes replaced row-to-JSON evidence with explicit bounded reads,
  preserved all fiat and token quantities as decimal strings, made keyset
  exhaustion exact, and expanded the PostgreSQL scenario with discrepancies,
  unmatched money, a held intent, a dead letter and delivery history.
  The four gates ran in `crypto-gateway-dev`:
  `cargo fmt --all -- --check` exited 0 with no output;
  `cargo clippy --workspace --all-targets --locked -- -D warnings` finished the
  dev profile with no warnings in 33.80s;
  `cargo test --workspace --locked` passed 156 tests with 14 ignored and no
  failures (plus all doc-test targets passed with zero tests); and
  `GATEWAY_TEST_DATABASE_URL=postgres://gateway:gateway@host.docker.internal:54329/gateway cargo test --workspace --locked -- --ignored`
  passed all 14 PostgreSQL scenarios (2 HTTP and 12 storage) with no failures.
  The focused operator scenario also passed `1 passed; 0 failed; 0 ignored; 3
  filtered out` and now asserts exact item keys, discrepancy pagination through
  a null final cursor, explicit evidence fields and string amounts beyond
  JavaScript's safe integer, and the four reviewed metric samples.

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
