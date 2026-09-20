# ADR 0002: PostgreSQL and transactional outbox

- Status: Accepted
- Date: 2026-09-19

## Context

Settlement, allocation, ledger writes, and merchant notification must never
silently diverge. The gateway has no pre-existing queue or product database.

## Decision

Use PostgreSQL as the sole operational source of truth. Commit settlement and
an outbox event in one transaction. Deliver signed webhooks asynchronously
with stable event IDs, retries, and visible dead-letter state.

Do not require a message broker for correctness. A broker may later accelerate
wakeup, but database polling remains the recovery path.

## Consequences

- Database constraints carry the core financial invariants.
- Queue loss cannot lose a settlement event.
- Webhooks are at-least-once and merchants must deduplicate.

