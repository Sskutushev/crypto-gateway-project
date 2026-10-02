# 6. Singleton workers are singletons by a fenced lease, not by replica count

Date: 2026-10-02 (recorded; the mechanism dates from migration 0006)

## Status

Accepted

## Context

The observer, the verifier and the settlement worker each advance a cursor
or a state machine that must have one writer. Running one replica makes that
true until a node drains, a rollout happens or the pod dies, and then the
component has no writer until the scheduler brings one back.

## Decision

Each such component takes a lease in `component_leases` before working. The
lease carries a fence token that increases on every takeover. A worker that
holds the lease checks it, by token, inside the transaction that writes; a
worker whose lease was taken over has its write refused (`LeaseLost`) even if
it wakes up believing it still leads. A replica that cannot take the lease
idles and reports itself as not the leader.

With that in place the deployment runs two replicas of each leased worker
behind a PodDisruptionBudget, spread across nodes and zones: a drain hands
the lease over instead of stopping the work.

## Consequences

- Correctness does not depend on replica count; replica count buys
  availability only.
- The outbox, the reconciler and the retention worker take the lease but
  claim their rows with `FOR UPDATE SKIP LOCKED`, so they are safe with
  several workers whether or not the lease is honoured at write time.
- The expiry sweep uses no lease: it claims rows with `SKIP LOCKED` and is
  idempotent, so overlapping sweeps are harmless and it stays at one replica.
- A failover test belongs in the test suite: the first replica loses the
  lease, the second takes it, the first's late write is refused by its stale
  token.
