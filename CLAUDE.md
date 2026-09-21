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

Two slices are complete and verified, and neither is committed:

1. Quote and exact-amount lease lifecycle: immutable quotes, payment attempts,
   one active lease per collector/raw-amount pair, separate quote and
   late-payment deadlines, fail-closed evidence, and audited transitions.
2. Bounded expiry scheduler (`crates/gateway-scheduler`): batched sweeps,
   single-flight guard, capped retries for storage outages only, counters, and
   shutdown between batches. `gateway-api` runs it and joins it before exit.

The next production slice is authenticated internal ingestion of immutable
price and rail-health snapshots. Today's snapshot rows are development
fixtures, so issuance still rests on hand-seeded evidence. That slice needs:

- an operator-authenticated internal write path, never the public merchant API;
- two genuinely independent sources per asset, recorded per snapshot;
- append-only snapshots: no update, no delete, no backdating;
- freshness and disagreement monitoring that fails closed for new quotes;
- proof that an ingestion outage leaves issued quotes replayable.

Do not copy private Refty entities or the root workspace crypto specification
into this public standalone product. Use it only as historical threat-model
input; this repository's `AGENTS.md`, architecture, and ADRs are authoritative.

Important local state at handoff:

- branch: `feat/standalone-gateway-foundation`;
- remote: `https://github.com/Sskutushev/crypto-gateway-project.git`;
- Rust is not installed on the host. The pinned toolchain runs in the
  `crypto-gateway-dev` container (`rust:1.90.0-bookworm`, `/workspace` bound to
  this repository), and PostgreSQL runs in the Compose service on
  `127.0.0.1:54329`. From the container that database is
  `postgres://gateway:gateway@host.docker.internal:54329/gateway`, which the
  ignored tests read from `GATEWAY_TEST_DATABASE_URL`;
- the complete verification record and unavailable checks are in
  `docs/implementation-status.md`;
- do not start TRON integration before an independent verifier and
  reconciliation path exist.
