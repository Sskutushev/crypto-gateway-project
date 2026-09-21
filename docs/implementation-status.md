# Implementation Status

Last updated: 2026-09-21

## Repository state

- Branch: `feat/standalone-gateway-foundation`
- Remote: `https://github.com/Sskutushev/crypto-gateway-project.git`
- Working tree: clean; the payment pipeline through settlement and signed
  webhook delivery is committed and pushed.
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

## In progress

- Dependency advisory scanning needs a reliable RustSec index connection; the
  local full `cargo deny check` stalled while fetching the advisory database.
- CI workflow and production deployment manifests have not been added.
- Production adapters that ingest price and rail-health snapshots are not yet
  implemented; current snapshot rows are test/development fixtures only.
- The verification, settlement, outbox and observation workers exist but are
  not yet started by the API binary; only the expiry scheduler is wired in.
- The TRON crate carries the address layer only. The HTTP source that turns
  provider answers into observations is the next slice, and it must parse
  transaction logs rather than the address-indexed summary, because only the
  log carries the event identity a canonical fact needs.
- Worker counters are in-process only; no metrics endpoint or scrape target
  exposes them yet.
- No reconciliation run, no component health ladder, and no operator API for
  conflicts, unmatched money or risk decisions.
- Risk screening is a stored decision with no provider behind it: every
  transfer reads as skipped, which the settlement bands treat as not
  screened, never as clean.

## Next slices

1. The TRON HTTP source: list candidates from the address-indexed endpoint,
   then read each transaction's logs for the event identity, with fixture
   tests for hostile and malformed answers.
2. Start every worker from the API binary, with per-worker configuration and
   one shutdown path.
3. Operator surface: metrics, payment health, and read APIs for conflicts,
   unmatched money, held payments and dead-lettered events.
4. Reconciliation: hourly incremental and daily full runs, with a money
   discrepancy automatically closing new quotes on the affected rail.
5. The component degradation ladder, where every transition is an event, a
   metric and an alert.
6. Authenticated internal ingestion of price and rail-health snapshots.
7. A second independent TRON provider group, then the ERC20 adapter.

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
