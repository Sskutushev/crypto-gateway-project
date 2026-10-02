# 5. A payment is matched by its exact amount, not by a memo

Date: 2026-10-02 (recorded)

## Status

Accepted

## Context

A payer sending USDT on TRON has no reliable way to attach a reference to the
transfer: wallets differ, memos are not part of a TRC-20 transfer, and asking
the payer to type one is where payments get lost. The gateway still has to
know which obligation an inbound transfer pays.

## Decision

Each quote reserves a unique raw amount on a collector address: the fiat
obligation converted at the quoted rate, rounded up, plus a small distinct
increment, so no two open quotes on one address expect the same amount. The
inbound transfer is matched by (collector address, exact raw amount) against
the open reservations. PostgreSQL enforces one active reservation per
(collector, amount) and one per payment attempt; the reservation outlives
the quote's expiry through the late-payment window so a slow payer is still
matched, and only then is archived.

## Consequences

- The amount a payer must send is exact. The checkout page says so, and an
  underpayment or overpayment is not a match: it is queued as unmatched for a
  person, with the behaviour per case listed in `docs/scope-and-limits.md`.
- One collector address holds at most 10,000 open reservations; quotes spill
  to the next address and a deployment adds addresses before the pool is
  full (`gateway_collector_open_leases`).
- A second payment of the same exact amount inside the window is the known
  limitation of this scheme and is documented rather than hidden.
