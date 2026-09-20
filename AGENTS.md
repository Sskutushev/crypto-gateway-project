# Agent Instructions

These rules apply to every automated contributor working in this repository.

## Product boundary

This repository is a standalone, public, non-custodial cryptocurrency payment
gateway. It must not depend on private products, private repositories, or
organization-specific business entities.

The gateway receives and verifies inbound payments. It never stores wallet
private keys, signs transactions, keeps customer balances, or performs
withdrawals.

## Engineering rules

- Money and token quantities are integers at every boundary. Floating-point
  money is forbidden.
- A provider response is an observation, not a trusted blockchain fact.
- A single observer can never cause a payment to settle.
- Token identity is an allowlisted contract or master address, never a symbol.
- Addresses are matched by canonical bytes, never display strings.
- One blockchain transfer can belong to at most one payment intent.
- Every state transition is monotonic, explicit, and auditable.
- Every external side effect is delivered through a transactional outbox.
- Retries must be observable. Silent failover and empty catches are forbidden.
- API writes require an idempotency key scoped to the authenticated merchant.
- Secrets and raw credentials must never be committed or printed in logs.

## Workflow

1. Read `CLAUDE.md`, `docs/architecture.md`, and
   `docs/implementation-status.md` before editing.
2. Search for existing patterns before adding modules.
3. Add the lowest-level regression test that exposes the behavior.
4. Run formatting, linting, unit tests, migration checks, and dependency
   policy checks for touched code.
5. Update `docs/implementation-status.md` before ending a work session.
6. Do not commit, push, or open a pull request unless the owner asks.

## Git

- Work on a feature branch, never directly on `main`.
- Use English for code, documentation, commits, and pull requests.
- Do not add AI attribution or co-author trailers.
- Preserve unrelated user changes.

