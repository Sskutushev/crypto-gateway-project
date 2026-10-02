# 7. Operator metrics are read from the database; process metrics are the next slice

Date: 2026-10-02

## Status

Accepted, with a stated gap

## Context

`/metrics` answers from the database on every scrape: component state,
intents by status, outbox depth, open conflicts, reservation occupancy. That
is what a dashboard needs to say whether money is moving, and it is the same
truth every process and every replica sees, with no per-process registry to
aggregate. The expiry and quote counters are the exception: they are
in-process and only meaningful on the API that served them.

What this does not give is latency and throughput inside a process: request
duration by route, worker batch duration, provider call latency, lock wait.
Those cannot be read back from the database, and without them a load test
measures from the outside only.

## Decision

Keep the database-backed series as the operator surface: they are the
numbers an alert rule should page on, and they are correct across replicas.
Scrape them at 30 seconds, not tighter, because each scrape is a set of
aggregate queries on the money's store.

Add process-level instrumentation as its own slice (a metrics registry per
process, exported on a separate internal listener without an operator key):
HTTP latency by route and status, worker tick and batch durations, provider
call latency per source, outbox time-to-first-attempt, pool wait. Until it
lands, capacity statements are not made from measurements the gateway did
not take.

## Consequences

- Alert rules ship against the database-backed series now
  (`deploy/k8s/monitoring`), and keep working when process metrics arrive.
- The operator key for scraping is a credential handed to the monitoring
  system; the separate listener in the next slice removes that hand-over.
- A benchmark published before process metrics exist would be a number
  without a cause; none is published.
