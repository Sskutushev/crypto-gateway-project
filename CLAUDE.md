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

## Current handoff: 2026-09-20

The first executable vertical slice is complete and verified: merchant API-key
authentication, merchant-isolated create/read payment intents, transactional
idempotency, one creation audit event, string-only public money, explicit API
errors, PostgreSQL integration coverage, and a non-root release container.

The next production slice is quote and amount-lease lifecycle. Before editing,
design the PostgreSQL constraints and domain transitions for all of these
invariants:

- one active lease per collector address and exact raw amount;
- one lease per payment attempt;
- database arbitration under concurrent allocation;
- quote expiry plus a separately explicit late-payment window;
- no early release merely because payment is considered unlikely;
- archive lease history in the same transaction that releases an expired slot;
- preserve exact integer/string money at every boundary;
- fail closed when pricing, policy, or rail health is unknown or stale;
- audit every quote/lease state transition.

Do not copy private Refty entities or the root workspace crypto specification
into this public standalone product. Use it only as historical threat-model
input; this repository's `AGENTS.md`, architecture, and ADRs are authoritative.

Important local state at handoff:

- branch: `feat/standalone-gateway-foundation`;
- remote: `https://github.com/Sskutushev/crypto-gateway-project.git`;
- Rust is not installed on the host, so use the pinned Docker toolchain;
- the complete verification record and unavailable checks are in
  `docs/implementation-status.md`;
- do not start TRON integration before quote/lease invariants and their
  concurrency tests are complete.
