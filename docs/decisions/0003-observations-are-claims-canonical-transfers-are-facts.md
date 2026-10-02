# 3. An observation is a claim; a canonical transfer is a fact

Date: 2026-10-02 (recorded; the decision dates from the evidence intake in
migration 0006 and the verifier in 0007)

## Status

Accepted

## Context

Most gateways treat a provider's answer as the blockchain: "the RPC returned
the transaction, so the payment is confirmed". A provider is a company's HTTP
service with its own bugs, outages, caches and incentives. One compromised or
mistaken provider would then move money.

## Decision

The row an observer writes (`chain_observations`) is what one source claimed
it saw, under that source's own database login, append-only. It is never read
by settlement. A separate verifier re-reads the event through its own source,
collects attestations, applies the finality policy and only then writes a
canonical transfer (`chain_transfers`). Settlement consumes canonical
transfers only.

Two sources that disagree produce a recorded conflict and nothing settles on
the disputed fact until a person decides. A transfer short of the required
depth is re-queued and read again until it is final.

## Consequences

- A compromised observer can lie under its own name about its own source and
  move its own cursor; it cannot create a canonical fact, an allocation, an
  outbox event or another source's recovery point (row level security on the
  observation tables compares the source's principal with `session_user`).
- Verification costs one extra read per transfer and a second provider. That
  is the price of not trusting one.
- The evidence trail (observations, attestations, conflicts) is the audit
  record a dispute is settled with, so it is append-only and retained longer
  than delivery attempts.
