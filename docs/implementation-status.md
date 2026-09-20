# Implementation Status

Last updated: 2026-09-20

## Repository state

- Branch: `feat/standalone-gateway-foundation`
- Remote: `https://github.com/Sskutushev/crypto-gateway-project.git`
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

## In progress

- Dependency advisory scanning needs a reliable RustSec index connection; the
  local full `cargo deny check` stalled while fetching the advisory database.
- CI workflow and production deployment manifests have not been added.

## Next slices

1. Quote and amount-lease lifecycle, including expiry and exact amount
   reservation under concurrency.
2. Append-only chain observation ingestion.
3. Independent verifier and canonical transfer lifecycle.
4. Matching, allocation, ledger, and transactional webhook outbox.
5. TRON adapter with two independent sources and reconciliation.

## Verification

- Rust is not installed on the host; all Rust checks ran in the pinned
  `rust:1.90.0-bookworm` container.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo test --workspace --locked`: passed (7 unit tests; the database test is
  intentionally ignored in this command).
- PostgreSQL-backed ignored API test: passed (1 test).
- `cargo deny check bans licenses sources`: passed; duplicate-version notices
  are warnings by policy.
- Full `cargo deny check`: unavailable because fetching the RustSec advisory
  database stalled; the process was stopped after repeated no-output waits.
- `docker compose config --quiet`: passed; the live PostgreSQL port was
  verified as `127.0.0.1:54329`.
- `docker build --pull=false -t crypto-gateway-project:local .`: passed.
- Workspace-level `python .agents/gate.py --tag core` does not cover this
  ignored nested repository and was blocked by unrelated root automation tests
  plus a Windows certificate-store failure in Semgrep.
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
