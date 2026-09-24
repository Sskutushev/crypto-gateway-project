# Deployment

The gateway is one image with two entry points and seven processes: the API
and one worker per role. Every process has its own database credential and
only the privileges its role needs; the database is outside the application
hosts and reached only over TLS.

## Topology

```
                     TLS terminates here
   merchants ───────► edge proxy / ingress ───► gateway-api (2 replicas)
   operators                                     │  reads/writes as gateway_api
                                                 ▼
                                          PostgreSQL (managed, TLS only)
                                                 ▲
   worker-expiry      gateway_payment ───────────┤
   worker-observer    <source's own role> ───────┤ ◄─── chain provider A (HTTPS)
   worker-verifier    gateway_verifier ──────────┤ ◄─── chain provider B (HTTPS)
   worker-settlement  gateway_payment ───────────┤
   worker-outbox      gateway_payment ───────────┤ ───► merchant webhooks (HTTPS)
   worker-reconciler  gateway_reconciler ────────┘
```

- `gateway-api` serves merchants and operators. It reaches nothing but the
  database. It does not run migrations and does not run the expiry sweep in
  production; its role cannot.
- `worker-observer` reads one chain provider and records what it saw. One
  Deployment per provider, each under its own login role named after its
  `chain_sources.db_principal`. Two observers of the same provider group add
  no independence.
- `worker-verifier` re-reads every claimed transfer through a provider of a
  different group and is the only process that writes canonical facts.
- `worker-settlement`, `worker-expiry` and `worker-outbox` share the
  `gateway_payment` group: they move money against facts they can only read.
  The outbox is the only process that reaches merchant endpoints.
- `worker-reconciler` reads everything, records findings, publishes component
  state and may close a rail. It cannot clear one; only a person can.

## The network and TLS boundary

- TLS terminates at the edge proxy or ingress controller. The API listens on
  plain HTTP on a private address; nothing else may reach it.
- PostgreSQL is reached only with `sslmode=verify-full`. Every example env file
  and every Secret carries that URL shape.
- `/metrics` is served by the API under the operator `read` scope and is
  scraped from inside the cluster (the `monitoring` namespace in the network
  policy); it is not exposed through the edge.
- Egress: observers and the verifier reach HTTPS providers; the outbox reaches
  HTTPS merchant endpoints; everything reaches the database; nothing reaches
  anything else. Private ranges are excluded from the internet egress so a
  provider URL cannot be pointed at a cluster service.

## Order of operations, first deployment

1. **Roles.** As the database administrator, apply `db/roles/00_roles.sql`
   and create the login roles it documents, one per process, with passwords
   from the secret manager. For each chain source, register its
   `chain_sources` row and apply `db/roles/20_observer_source_role.sql.template`
   with the placeholders filled: the login role name is the source's
   `db_principal`.
2. **Migrations.** Run them as a login role in `gateway_migrator`
   (`GATEWAY_RUN_MIGRATIONS=true` on a one-off `gateway-api` process with that
   role's URL, or `sqlx migrate run` against `db/migrations`). Then apply
   `db/roles/10_grants.sql`; it moves every table under `gateway_migrator`
   and grants each group exactly what its code touches. Re-apply it after
   every migration.
3. **Reference data.** Insert the asset, collector address, finality policy,
   settlement policy and quote policy rows for the environment. The collector
   is a public address only; the gateway never holds a key.
4. **Self-check configuration.** Set `GATEWAY_CHAIN_ENVIRONMENT`,
   `GATEWAY_EXPECTED_COLLECTORS` and `GATEWAY_EXPECTED_ASSETS` to what step 3
   inserted. A process whose database disagrees with these refuses to start,
   and `/health/ready` says which row disagreed.
5. **Roll the API**, then confirm `/health/ready` answers 200 through the edge.
6. **Roll the workers**: expiry, observer(s), verifier, settlement, outbox,
   reconciler. Each takes its lease only after its own self-check passes.
7. **Prices and rail health.** Feed the first price readings and a healthy
   rail-health snapshot through the operator API; until then every quote is
   refused, by design.

## Compose

`deploy/compose.production.yaml` runs the topology on one host against an
external PostgreSQL. Create `deploy/env/<service>.env` from each
`.env.example`, then:

```
docker compose -f deploy/compose.production.yaml config --quiet
docker compose -f deploy/compose.production.yaml up -d
```

Every container runs read-only, as UID 10001, with all capabilities dropped
and `no-new-privileges`. The API's healthcheck is `/health/ready`, so a
container whose database stopped describing its configuration is reported
unhealthy rather than serving.

## Kubernetes

`deploy/k8s` is a kustomization: one Deployment per role, a Service and a
PodDisruptionBudget for the API, a ConfigMap for non-secret configuration,
and the network policies above. Secrets are created separately from
`deploy/k8s/secrets.example.yaml` (names and keys, no values).

```
kubectl kustomize deploy/k8s | less     # review
kubectl apply -k deploy/k8s
```

The namespace enforces the `restricted` Pod Security Standard; every pod runs
non-root, read-only, without capabilities, under the runtime seccomp profile.
Readiness is the self-check re-evaluated at most every ten seconds; liveness
is process presence.

## Scaling a role

- The API scales horizontally; it holds no state between requests.
- Workers hold a fenced lease per role. A second replica of a role is safe
  (the loser of the lease waits) but is not what the `Recreate` strategy
  intends: the way to more throughput is a larger batch or a shorter interval
  in that role's `GATEWAY_<ROLE>_*` settings, not a second copy.
- A second chain provider is a second observer Deployment with its own
  source, secret and login role, and a chain source row in a different
  provider group.

## Rotation

- Database credentials: create the new login role in the same group, switch
  the Secret, roll the process, drop the old role. Observers are the
  exception: the role name is the source's identity, so a new observer role
  means a new `chain_sources` row and the old source retired.
- The webhook master key derives every endpoint's signing secret. Rotating it
  changes every signature at once; merchants must be re-issued their secrets
  and every endpoint's fingerprint re-registered before the new key serves.
- Collector addresses rotate through `active → receiving_only → retired` in
  the database and `GATEWAY_EXPECTED_COLLECTORS` in the configuration; the
  self-check refuses a process whose list disagrees with the database in
  either direction.
