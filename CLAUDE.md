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

The current `main` baseline is commit
`c19028965e93f4d3731d67fec5c219d4fbd0b5e2`. The repository implements the
payment pipeline, TRON HTTP source, independent-evidence verifier, matching and
settlement, signed webhook outbox and delivery, reconciliation, the worker
runtime, the operator read surface, start-up self-check, least-privilege
database roles, Compose and Kubernetes deployment definitions, CI with SBOM
and scan, and an OpenAPI document tested against the router.

Implemented does not mean ready for unsupervised mainnet money. The remaining
owner and operational work is tracked in `docs/owner-setup.md`: verify branch
protection, provision genuinely independent providers and a collector, create
the webhook master key and scoped credentials, approve policies, and complete
a sustained testnet run. KYT integration, disaster-recovery drills, production
capacity evidence, and operator decision paths for held, unmatched, late and
overpaid transfers remain incomplete. Do not describe them as available.

Local state:

- baseline reviewed: `main` at
  `c19028965e93f4d3731d67fec5c219d4fbd0b5e2`;
- active hardening work is on `feat/production-readiness-p0`; do not commit
  directly to `main`;
- Rust is not installed on the host. The pinned toolchain runs in the
  crypto-gateway-dev container (rust:1.90.0-bookworm, /workspace bound to this
  repository), and PostgreSQL runs in Compose on 127.0.0.1:54329. From the
  container that database is
  postgres://gateway:gateway@host.docker.internal:54329/gateway, which the
  ignored tests read from GATEWAY_TEST_DATABASE_URL;
- do not infer the state of external providers, production secrets, backups,
  branch protection or a deployed environment from repository tests;
- historical repository verification records are in
  `docs/implementation-status.md`; rerun the relevant gates for every new
  change rather than treating an earlier result as proof for the current tree.
