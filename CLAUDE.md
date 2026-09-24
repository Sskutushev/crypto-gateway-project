# Continuation Guide

Start with these files:

1. `AGENTS.md` - non-negotiable safety and workflow rules.
2. `docs/architecture.md` - system boundaries and trust model.
3. `docs/implementation-status.md` - completed work, next slice, commands, and
   blockers.
4. `docs/decisions/` - accepted architecture decisions.

The repository is deliberately standalone and public. Do not import private
application concepts, source code, naming, schemas, or infrastructure.

At the end of every session, update `docs/implementation-status.md` with:

- the exact branch and working-tree state;
- what was completed and verified;
- the next smallest production-safe slice;
- any failed commands and why they failed;
- credentials, accounts, legal decisions, or infrastructure still required.

Never claim a chain integration is ready until its adapter, independent
verification, finality policy, adversarial tests, and reconciliation path all
pass.

## Current handoff: 2026-09-21

The payment pipeline is implemented and pushed, from a merchant's payment
intent to a signed webhook, with 107 unit tests and 11 PostgreSQL scenarios.

What exists: quotes and exact-amount leases; chain evidence intake with fenced
cursors and database-enforced source identity; the verifier that turns
independent readings plus its own re-read into canonical facts; matching;
settlement in one transaction; the outbox and signed delivery; a generic
leased worker runtime; canonical TRON addresses.

What does not exist yet, in order:

1. The TRON HTTP source. Parse transaction logs, never the address-indexed
   summary alone: only the log carries the event index that makes a canonical
   fact identifiable. Test it with captured fixtures, including hostile ones.
2. Starting the workers from the API binary; today only the expiry scheduler
   runs there.
3. The operator surface: metrics, payment health, and read APIs for conflicts,
   unmatched money, held payments and dead-lettered events.
4. Reconciliation and the degradation ladder.

Do not copy private Refty entities or the root workspace crypto specification
into this public standalone product. Use it only as historical threat-model
input; this repository's AGENTS.md, architecture and ADRs are authoritative.

Important local state at handoff:

- branch: feat/standalone-gateway-foundation, pushed;
- Rust is not installed on the host. The pinned toolchain runs in the
  crypto-gateway-dev container (rust:1.90.0-bookworm, /workspace bound to this
  repository), and PostgreSQL runs in Compose on 127.0.0.1:54329. From the
  container that database is
  postgres://gateway:gateway@host.docker.internal:54329/gateway, which the
  ignored tests read from GATEWAY_TEST_DATABASE_URL;
- the migrations are not released, so they are still edited in place; the dev
  schema is reset with DROP SCHEMA public CASCADE when one changes;
- the complete verification record is in docs/implementation-status.md.
