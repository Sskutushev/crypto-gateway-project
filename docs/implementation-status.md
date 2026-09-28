# Implementation Status

Last updated: 2026-09-28

## Repository state

- `main` baseline: `c19028965e93f4d3731d67fec5c219d4fbd0b5e2`. `main`
  exists; do not recreate it or commit directly to it, and verify the remote
  branch-protection settings before release work.
- Active branch: `feat/merchant-owned-collectors`, stacked on
  `feat/production-readiness-p0` (PR #16). Head at this update: `f6ac2d3`.
  The branch has no pull request of its own yet.
- Remote: `https://github.com/Sskutushev/crypto-gateway-project.git`
- No release has been tagged.
- This file distinguishes implemented repository capabilities from external
  production readiness. It does not certify providers, KYT, secrets, backups,
  disaster recovery, capacity, legal approval or a mainnet deployment.

## Session 2026-09-28: address pool, capacity and request budgets

Branch `feat/api-limits-and-capacity`, from `main` at `2dc7ff7`.

- Quotes spread over a merchant's active addresses, least loaded first, and
  move to the next address (up to eight) when every exact amount near the
  price is taken; a refused attempt rolls back with its idempotency record.
- `GATEWAY_MAX_OPEN_LEASES_PER_COLLECTOR` caps reservations per address;
  all-full is `503 quote_capacity_exhausted` with `Retry-After`.
- `/metrics`: open and live reservations per address, quote outcomes,
  spillovers, quote latency histogram.
- Request budgets per merchant (writes, reads) and per client address
  (failed authentications, payment page), `429 rate_limited` with
  `Retry-After`; 64 KiB body limit; 16 KiB metadata limit.

Verified in `crypto-gateway-dev`: fmt, clippy `-D warnings`, unit tests, and
all 50 PostgreSQL scenarios, including
`quotes_spread_over_the_address_pool_and_stop_at_its_limits`. Not measured:
quote latency and lock wait under load; the histogram exists so a load test
can report them.

## Session 2026-09-28: documentation site, release process, restore drill

Branch `feat/docs-site-and-release`, from `main` at `2dc7ff7`. No tag was
created.

- `site/build.py` renders `docs/*.md` and `docs/openapi.json` into a static
  site; `.github/workflows/pages.yml` deploys it to GitHub Pages from `main`
  and fails a pull request on a broken internal link. Verified locally:
  13 pages, sitemap and `robots.txt` built, internal links and anchors
  resolve.
- `.github/workflows/release.yml`, cosign keyless signing in the `image` job
  of `ci.yml`, `scripts/release-notes.py`, `docs/releasing.md`. Verified with
  actionlint and by running the notes script (it refuses today: there is no
  `[0.1.0]` section yet). Not verified: a real tag run on GitHub.
- `docs/backup-and-restore.md` and `scripts/backup-drill.sh`. Run against
  the local Compose PostgreSQL: a freshly migrated source restores, migrates
  and passes every check; a corrupted scratch copy fails the count and the
  paid-without-fulfilment checks. The existing local databases
  (`gateway`, `gateway_demo`) are refused by the current image because they
  were migrated before `d5dbe2f` changed the line endings of migration `0006`;
  the drill reports that as a FAIL, which is correct. No production backup or
  point-in-time restore has been exercised.

## Session 2026-09-27: money binding, merchant-owned collectors, onboarding

Branch `feat/merchant-owned-collectors`, stacked on
`feat/production-readiness-p0` (PR #16, whose head is `189d3ac`).

Commits on top of the P0 work:

- `189d3ac` — money binding. Manual `honor` binds the transfer to the
  attempt's collector, its quote's asset and rail, an open attempt and
  intent, and the late-payment window, all read from locked rows. Paid and
  partially-paid transitions move only from an explicit set of states and
  require exactly one updated row; a refusal rolls the whole settlement back
  before a claim, event or webhook, and automatic settlement parks the
  transfer as `held` / `manual_required`. Migration 0014 adds the collector to
  allocations and claims with composite foreign keys to the attempt and the
  transfer. Webhook delivery ignores system proxies and bounds DNS
  resolution. Supply chain: documented advisory ignores in
  `.cargo/audit.toml` and `deny.toml`, and a CI guard that fails if an
  advisory ignored as not compiled starts being compiled.
- `39f9d17` — merchant-owned collectors. Migration 0015 adds
  `merchants.collector_policy` (`own` by default for new merchants; merchants
  that existed before are `shared`), `collector_addresses.merchant_id`, and a
  trigger on `payment_quotes` refusing a quote on a collector that does not
  receive money for its merchant. Quote context and the issue-time re-check
  select by policy with no fallback. The self-check pins every active or
  receiving-only collector; the two-way agreement with
  `GATEWAY_EXPECTED_COLLECTORS` covers operator collectors only, and `none`
  declares a deployment without them.
- `6fc15a2` — onboarding. `gateway-worker admin` (`apps/gateway-worker/src/admin.rs`):
  `merchant-create`, `api-key-issue`, `api-key-revoke`, `webhook-add`,
  `webhook-rotate`, `webhook-disable`, `webhook-test`, `collector-statement`,
  `collector-register`, `collector-retire`, each audited in its own
  transaction and printing one JSON object. TIP-191 ownership proof
  (`crates/gateway-tron/src/ownership.rs`): a fixed statement naming the
  gateway purpose, merchant, address and issue time, valid for 24 hours and
  at most five minutes in the future. Migration 0016 webhook key ring:
  rotation with a transition period in which deliveries carry
  `t=..,v1=<new>,v1=<previous>`. The `gateway_provisioner` role with
  column-level grants (`db/roles/00_roles.sql`, `db/roles/10_grants.sql`).
  `examples/` (webhook receivers in TypeScript and Python, create-payment,
  Postman), `docs/money-invariants.md`, `docs/scope-and-limits.md`.
- `64b0e3a` — the expiry batch no longer stalls on a partially paid or held
  intent: the attempt always expires, only an intent still awaiting its first
  money becomes `expired`, and the closure on one with money is audited as
  `payment_intent.quote_window_closed`. Covered by the PostgreSQL scenario
  `a_partially_paid_intent_never_stops_the_expiry_of_the_others`.
- `f6ac2d3` — lateness is judged by the block time against the quote's
  `expires_at`, not by the attempt's status when settlement runs; the manual
  honor window uses the same half-open boundary (`block_time <
  late_payment_until`).
- Documentation (this update): README quickstart onboards through the admin
  CLI, with Integrate and Scope pointers; `docs/owner-setup.md`,
  `docs/merchant-integration.md`, `docs/operator-runbook.md` and
  `CHANGELOG.md` describe the CLI, collector policies, the ownership proof,
  the provisioner role, secret rotation and the audit trail.

Verified (reported for the tree carrying `189d3ac`, `39f9d17` and `6fc15a2`,
in the `crypto-gateway-dev` container): `cargo fmt --all -- --check` clean;
`cargo clippy --workspace --all-targets --locked -- -D warnings` clean;
`cargo test --workspace --locked` passed; PostgreSQL scenarios passed —
storage 40, HTTP 3, and the 2 role scenarios, which now include the
provisioner role's refusals. Gate results for `64b0e3a` and `f6ac2d3` are not
recorded here; rerun the full set on the current head before opening the pull
request. Not run in this session: `cargo deny`, `cargo audit`, the GitHub
Actions workflow, a testnet payment.

Working tree at this update: uncommitted work in progress by another
engineer (`crates/gateway-storage/src/oversight.rs`,
`crates/gateway-storage/src/postgres.rs`, untracked
`db/migrations/0017_attempts_and_checkout.sql`). It is not described above
and is not verified.

Next smallest slices, in order:

1. Record the gates for `64b0e3a` and `f6ac2d3` (the expiry defect for
   partially paid intents is fixed in `64b0e3a`), and update the "Known gap"
   and late-payment rows of `docs/scope-and-limits.md`, which still describe
   the behaviour before those two commits.
2. A cancel route for an intent that has no money on it, audited, with the
   existing refusal path for money that arrives afterwards.
3. Re-quoting an intent: a new attempt supersedes the previous one, whose
   lease stays until its `late_payment_until`, instead of today's one quote
   per intent.
4. The quote response carries the token's `decimals` and an exact display
   amount, so merchants stop hard-coding decimals.
5. A hosted checkout page for the payer (address, exact amount, expiry).
6. The first release tag, once the branch stack is merged and CI is green on
   `main`.

Closed on 2026-09-28: `collector-stop-quoting` moves a collector to
`receiving_only` with an audit row, and `collector-retire` refuses while a
reservation remains unless `--compromised yes` (scenario
`a_collector_holding_a_reservation_stops_quoting_but_is_not_retired`). The
CLI still has no list commands; identifiers are read with the
`gateway_readonly` role.

Still needs the owner: genuinely independent TRON providers and keys, a
sustained Nile testnet run with a merchant address registered by signature,
an external security audit, and the remaining steps of `docs/owner-setup.md`
(branch protection, master key, provisioner and process credentials,
policies, KYT, legal).

## Product decisions

- Standalone public product with no private application dependencies.
- Non-custodial inbound payments only.
- Rust workspace for the API, financial core, observers, verifier, workers,
  and reconciler.
- PostgreSQL is the sole operational source of truth.
- Merchant fulfillment is a signed webhook contract backed by a transactional
  outbox.
- By default a merchant is paid on its own address (`collector_policy =
  'own'`); an operator-owned address is an explicit `shared` choice.
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
  `risk_ingest`, `read` and `admin` scopes. Risk keys are bound to one named
  provider and stale/future evaluations are refused. Prices are submitted as readings and aggregated by
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
  collector and active asset from stored bytes and require every operator
  collector and every active asset to be named by
  `GATEWAY_EXPECTED_COLLECTORS` and `GATEWAY_EXPECTED_ASSETS`, in both
  directions (since 2026-09-27 merchant-owned collectors are pinned but not
  listed, and `none` declares a deployment without operator collectors);
  every active source, asset and collector must carry the process's
  `GATEWAY_CHAIN_ENVIRONMENT`; every active asset needs an active finality
  policy; no block cursor may stand ahead of the head its own source reported;
  and PostgreSQL and the process may not disagree by more than
  `GATEWAY_MAX_CLOCK_SKEW_SECONDS`. A failed check stops the process before it
  serves traffic or takes a lease, `GET /health/ready` answers 503 with every
  row and its reason, and a passed report is cached for at most ten seconds
  under a visible `evaluated_at`.
- Least-privilege database roles as applied SQL. `db/roles/00_roles.sql`
  creates the migrator, api, observer, verifier, payment, reconciler,
  readonly and (since 2026-09-27) provisioner groups; `10_grants.sql` moves
  every table under the migrator and grants each group exactly the tables its crate reads and writes;
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
  and sources, with both RustSec advisories and cargo audit blocking the job;
  compose and kustomize validation; the image with an SPDX SBOM and a Trivy
  scan; publication to GHCR from a version tag. Dependabot watches Cargo,
  Actions and the base images.
- Open-source packaging. A README written for a merchant CTO and for search:
  what the gateway is, the four properties, a ten-minute quickstart, the
  security model, how it fails on purpose, and the documentation map.
  `docs/openapi.json` describes every served route with the exact shapes the
  handlers produce, and a unit test proves the route list, the router and
  the document cannot drift. `docs/merchant-integration.md`,
  `docs/operator-runbook.md`, `docs/threat-model.md` and
  `docs/owner-setup.md` (every account, key and secret, in order, with what
  is blocking). `SECURITY.md`, `CONTRIBUTING.md`, the Contributor Covenant
  2.1, issue and pull request templates, `CODEOWNERS`, `CHANGELOG.md`. The
  API binary gained `GATEWAY_MIGRATE_ONLY` for the release step and the
  quickstart; `scripts/seed-dev-rail.sql` and
  `scripts/create-dev-operator.sql` seed a Nile testnet rail and an operator
  key; evidence-bundle timestamps now serialise as RFC 3339 like every other
  response.
- Manual resolution of parked money. Admin-only `honor` of a finalized
  held/unmatched transfer through the existing atomic settlement
  transaction, `reject` of an unallocated transfer, and a recorded external
  overpayment disposition. Every command is idempotent and audited; the
  gateway still does not send refunds. Since 2026-09-27 `honor` is bound to
  the attempt's collector, asset, rail, open states and late window.
- Merchant-owned collectors, the admin CLI for onboarding, the TIP-191
  ownership proof, the webhook key ring with rotation, the `webhook.test`
  event and the `gateway_provisioner` role: see the 2026-09-27 session above.

## In progress

- Dependency advisory scanning needs a reliable RustSec index connection; the
  local full `cargo deny check` stalled while fetching the advisory database.
  Ignored advisories are now documented, with a CI guard (2026-09-27); the
  CI job is the authority on the current result.
- Risk screening has an attributable push path and no provider behind it: a
  transfer nobody screened reads as skipped, which the settlement bands treat
  as not screened, never as clean.

## Next slices

The product slices for the current branch are listed in the 2026-09-27
session above. Before real money, independently of them:

1. The owner's remaining steps in `docs/owner-setup.md`: verify repository and
   branch-protection settings, provision providers, master key, provisioner
   and scoped credentials, register collectors, approve policies, then run a sustained testnet soak
   with genuinely independent providers.
2. Integrate a real KYT provider and exercise the implemented operator decision
   paths for held, unmatched, overpaid and late payments in the testnet soak.
3. Prove PostgreSQL backup, point-in-time recovery and restore procedures, and
   establish measured capacity and failure-recovery evidence before mainnet.
4. Add another rail only after the first TRON rail meets those production
   gates; an additional adapter does not substitute for hardening.

## Historical verification evidence

The entries below record what passed for the named tree and date. They are
evidence for those revisions, not a claim that the current working tree, a
GitHub workflow, external providers or a deployed mainnet environment is
healthy. Run the relevant checks again after every change.

- Money binding, merchant-owned collectors and onboarding (2026-09-27): see
  the session section above for the gates reported and what was not run.

- Open-source packaging (2026-09-24), in `crypto-gateway-dev`: fmt clean;
  clippy with `-D warnings` clean; `cargo test --workspace --locked` passed
  166 tests with 27 ignored, among them
  `every_route_is_served_and_documented_and_nothing_else_is`, which builds
  the router on a pool that never connects, answers every listed route with
  something other than 404 or 405, refuses an unlisted path and a wrong
  method, and matches the list against `docs/openapi.json` in both
  directions; all 27 PostgreSQL scenarios passed. A link check over the
  README and every document found every relative link resolving; external
links were listed, not fetched.

- P0 production-readiness hardening (2026-09-24), on the uncommitted
  `feat/production-readiness-p0` working tree: `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --locked` and
  `cargo clippy --workspace --all-targets --locked -- -D warnings` passed;
  `cargo test --workspace --locked` passed 174 ordinary tests with 31 database
  tests ignored; a final `cargo test --workspace --locked -- --ignored` passed
  all 31 PostgreSQL scenarios (3 HTTP and 28 storage), including concurrent
  manual-resolution idempotency, reject and external remainder-disposition
  paths, provider-bound KYT intake, least-privilege roles and every
  reconciliation invariant. `docs/openapi.json` parsed
  successfully, actionlint accepted the workflow, production Compose validated
  and `kubectl kustomize deploy/k8s` rendered without error. One held-payment
  ageing scenario failed once in an earlier parallel full ignored run, then
  passed in isolation, in the repeated 26-scenario storage run and in the final
  31-scenario workspace run; no code was changed to conceal it. This is local
  verification, not CI, provider independence,
  testnet soak, disaster-recovery, capacity or mainnet evidence.
  The workspace-level `python .agents/gate.py --tag pre-push` remained blocked
  outside this repository: 8 checks passed, 3 failed and 1 was unavailable.
  It reported missing product receipts for `unit-detect` and this repository,
  the known Windows certificate-store failure in Semgrep, and root hook/audit
  packaging test setup errors. A direct product-receipt attempt for this repo
  was refused because no complete product-gate configuration exists; the
  standalone checks above are therefore the evidence for this working tree.

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
  `gateway-api` and `gateway-worker`. Not run in this local verification:
  `cargo deny`, `cargo audit`, or the GitHub Actions workflow. Consult the
  workflow result for the exact commit instead of inferring CI health from
  this local record.

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
- The TRON adapter, independent-evidence verifier, matching and settlement,
  webhook delivery, and reconciliation are implemented. Their presence does
  not by itself make the gateway safe for real-money mainnet operation; the
  external and operational gates above remain required.

## External inputs still required

- Final public product name and package/container namespace.
- Hosting target and region.
- Managed PostgreSQL choice and credentials (later; local development does not
  need them).
- Two genuinely independent TRON data sources and API keys.
- For merchants on `shared`: the operator collector address for each enabled
  network. For merchants on `own`: each merchant's address and its signed
  ownership statement. Public addresses only; never provide a private key or
  seed phrase.
- KYT provider/account and policy thresholds before automatic settlement.
- Legal entity, supported payer jurisdictions, sanctions/KYC requirements, and
  written approval before enabling a production payment policy.
- Public API hostname, webhook signing-domain choice, and email/alert channel.
