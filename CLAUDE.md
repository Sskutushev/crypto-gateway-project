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

## Current handoff: 2026-09-24

Every slice of the contour plan is implemented and verified: the payment
pipeline, the TRON HTTP source, the worker runtime, the operator surface,
component health and reconciliation with a scenario per check, the start-up
self-check, least-privilege database roles as SQL, deployment for Compose and
Kubernetes, CI with SBOM and scan, and the open-source packaging with an
OpenAPI document the router is tested against.

What remains is the owner's, in `docs/owner-setup.md`: repository identity and
a protected `main`, two independent providers, a collector address, the
webhook master key, keys, policies, then a testnet run with real providers.
After that: a second TRON provider group, the ERC20 adapter, a screening
provider port, and the operator decision path for overpayment and late
payment.

Local state:

- branch: feat/standalone-gateway-foundation, pushed; `main` does not exist
  yet (the owner creates and protects it);
- Rust is not installed on the host. The pinned toolchain runs in the
  crypto-gateway-dev container (rust:1.90.0-bookworm, /workspace bound to this
  repository), and PostgreSQL runs in Compose on 127.0.0.1:54329. From the
  container that database is
  postgres://gateway:gateway@host.docker.internal:54329/gateway, which the
  ignored tests read from GATEWAY_TEST_DATABASE_URL;
- the migrations are not released, so they are still edited in place; the dev
  schema is reset with DROP SCHEMA public CASCADE when one changes;
- the complete verification record is in docs/implementation-status.md.
