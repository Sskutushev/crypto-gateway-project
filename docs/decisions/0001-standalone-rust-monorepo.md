# ADR 0001: Standalone Rust monorepo

- Status: Accepted
- Date: 2026-09-19

## Context

The gateway begins as a new public product with no existing application core
or transaction boundary to preserve. Blockchain input is adversarial, token
amounts require 256-bit integer arithmetic, and settlement invariants must be
shared by API and worker processes.

## Decision

Use one Rust workspace containing shared domain crates and multiple separately
deployed binaries. Each binary receives its own database role, network policy,
and credentials.

## Consequences

- Integer-only domain types are enforced by the compiler across the system.
- One language and migration model covers the first production release.
- Deployments remain isolated even though code is shared.
- UI work, if added, may use a separate TypeScript application without owning
  payment decisions.

