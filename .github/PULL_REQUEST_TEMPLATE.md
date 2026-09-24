## What changed

<!-- One concern. Say what, and why it is the right shape. -->

## How it was verified

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] PostgreSQL scenarios: `cargo test --workspace --locked -- --ignored` against a disposable database
- [ ] `cargo deny check` (required when a dependency changed)
- [ ] A migration changed: applied twice to a fresh database, and `db/roles/10_grants.sql` names any new table

## Invariants touched

<!-- Money, chain facts, settlement, webhooks, roles: which, and what test proves the invariant still holds. "None" is an answer. -->

## Documentation

- [ ] `docs/implementation-status.md` updated
- [ ] `CHANGELOG.md` updated under Unreleased
- [ ] `docs/openapi.json` updated if a route or a response changed (the contract test fails otherwise)

Signed-off-by under the DCO on every commit.
