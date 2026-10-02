# 8. Process metrics are served on a listener of their own, without a key

Date: 2026-10-02

## Status

Accepted. Closes the gap stated in `0007`.

## Context

`0007` kept the database-backed series as the operator surface and named
what they cannot say: how long a request, a batch or a provider call takes
inside one process. Those numbers are per process by nature, they reveal
nothing about a merchant, and the `read` operator key that guards `/metrics`
is a credential a monitoring system should not have to hold.

## Decision

Every process keeps a registry of its own histograms, counters and gauges
and serves them as Prometheus text on `GATEWAY_METRICS_BIND_ADDRESS`, a
second listener with no authentication. The deployment definitions keep
that port off the Service and off the ingress; the network policy admits
the monitoring namespace alone, and a `PodMonitor` scrapes every pod.

The registry is written in the repository (`crates/gateway-telemetry`), not
taken from a metrics framework: a histogram is a handful of atomics and the
text format is a few lines, while a framework is another audit surface in a
binary that moves money. Durations are kept in whole microseconds and
rendered as seconds without floating-point arithmetic.

What is measured:

- `gateway_http_request_duration_seconds{method,route,status}` and
  `gateway_http_requests_in_flight`: the route label is the matched pattern,
  never the path, so an id cannot grow the registry.
- `gateway_worker_run_duration_seconds{worker,outcome}`,
  `gateway_worker_batch_duration_seconds{worker}` and
  `gateway_worker_items_processed_total{worker}`, the expiry sweep included.
- `gateway_chain_source_request_duration_seconds{provider,endpoint,outcome}`:
  the provider is the host of the base URL, the endpoint the first two path
  segments, so an address in a path cannot become a label.
- `gateway_outbox_first_attempt_delay_seconds` and
  `gateway_webhook_delivery_duration_seconds{outcome}`.
- `gateway_db_pool_connections{state}` and
  `gateway_db_pool_max_connections`, sampled every five seconds, and
  `gateway_db_pool_acquire_wait_seconds{path,outcome}`: the hand-over wait
  at every transaction's start (`path="transaction"`) and for one probe
  acquisition per sample (`path="probe"`).

## Consequences

- The database-backed series and their alert rules are unchanged; the new
  rules on latency are warnings that point at a cause, not pages.
- `sqlx` offers no acquire hook, so the wait for a connection is measured
  where this code reaches the pool: every transaction begins through one
  method that times the hand-over, and the sampler probes one acquisition
  per tick. Single statements executed straight on the pool are not timed
  individually; the probe stands in for them.
- A capacity statement can now cite the gateway's own measurements. None is
  made until a sustained run has been measured.
