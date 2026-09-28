# Backup and restore

PostgreSQL is the gateway's only state: every intent, quote, observation,
canonical transfer, allocation, decision, outbox event and audit row lives
there. This page covers how to back it up, how to restore it, what to check
after a restore, and the monthly drill that proves the procedure works.
Nothing here has been exercised against a production database yet; the drill
script below has been run against the local Compose database.

## What a database backup does not contain

Keep these in your secret manager, versioned, next to the backup policy:

- **The webhook master key** (`GATEWAY_WEBHOOK_MASTER_KEY`). The database
  holds only a fingerprint of each endpoint's derived secret, so a stolen
  backup cannot forge events, and a restore with a different master key makes
  the outbox refuse to sign rather than send signatures no merchant can
  verify.
- **Process credentials** and the login roles themselves when you restore a
  single database logically (roles belong to the cluster; a physical or
  point-in-time restore of the whole cluster brings them back).
- **Provider API keys** and the self-check configuration
  (`GATEWAY_EXPECTED_COLLECTORS`, `GATEWAY_EXPECTED_ASSETS`,
  `GATEWAY_CHAIN_ENVIRONMENT`): the restored database must match them, or
  every process refuses to start.

A backup does contain merchant API key hashes, webhook URLs and fingerprints,
collector and payer addresses and the full payment history. Encrypt it,
restrict who can restore it, and keep it out of the account that runs the
gateway.

## Recovery objectives

Both numbers are the owner's choice, written down before the first real
payment and checked by every drill:

| Parameter | Meaning | What decides it | Starting point, not a recommendation |
|---|---|---|---|
| RPO | how much committed history a restore may lose | WAL archiving frequency (`archive_timeout`) or the provider's PITR granularity | 5 minutes |
| RTO | how long from the decision to restore until the gateway takes payments again | base backup size, WAL to replay, and the checks in [After a restore](#after-a-restore) | 4 hours |
| Retention | how far back a restore can go | PITR window and base backup retention | 35 days, so a monthly drill can target any day of the previous month |

Losing history is not the same as losing money here. The chain keeps every
transfer; after a restore the observers re-read from their restored cursors,
and a payment settled after the restore point is observed, verified and
settled again (see [why that is safe](#why-a-re-run-does-not-pay-twice)).
What a restore does lose is everything that exists only in the database:
API writes, operator actions and delivery state after the restore point.

## Taking backups

### Managed PostgreSQL

Preferred. Enable point-in-time recovery with the retention above, automated
daily snapshots, and a copy of the backups to another region or account.
Restores always go to a new instance; never restore over the instance that
failed, it is the evidence.

### Self-managed PostgreSQL

Continuous WAL archiving plus periodic base backups, with a tool that
verifies what it stores (pgBackRest or WAL-G):

```
# postgresql.conf
wal_level = replica
archive_mode = on
archive_timeout = 60s
archive_command = 'pgbackrest --stanza=gateway archive-push %p'
```

- A full base backup weekly and a differential daily
  (`pgbackrest --stanza=gateway --type=full backup`), or
  `pg_basebackup -D <dir> -Ft -z -X stream -c fast` if you manage files yourself.
- The repository is object storage in another account, versioned or under an
  object lock, encrypted.
- Alert on archiving: `pg_stat_archiver.failed_count` increasing, or
  `now() - last_archived_time` above the RPO.

### Logical dumps

`pg_dump` captures one moment and cannot replay to another, so it does not meet
an RPO measured in minutes. It is useful to move a database, to keep an extra
independent copy, and for the local drill below.

## Restoring

1. **Stop every gateway process** that points at the damaged database, the
   outbox first: nothing should deliver or settle while the restore target is
   decided.
2. **Choose the target time** and restore to a new instance
   (`recovery_target_time`, `recovery_target_action = 'promote'`, or the
   provider's PITR form). For a logical restore, create the roles first
   (`db/roles/00_roles.sql` and the login roles), then restore.
3. **Migrate only**, with the image version that was running or a newer one
   the [compatibility matrix](releasing.md#compatibility-matrix) allows, then
   apply `db/roles/10_grants.sql`.
4. **Start the API** against the new instance. `/health/ready` must answer
   `200`: the self-check recomputes collector and asset pins, the chain
   environment, finality policies, cursor sanity and clock skew.
5. **Start the reconciler alone** and wait for one run. It must be quiet (see
   below) before anything moves money.
6. **Start the observers, the verifier, settlement and expiry**; let the
   observers catch up from their restored cursors.
7. **Do the manual checks below**, tell merchants the restore window, then
   start the outbox last.

## After a restore

### Checked by the system

- **The self-check passes** on every process (`/health/ready` is `200`).
- **The reconciliation run is quiet**: `gateway_reconciliation_last_run_status`
  is `0`, no new discrepancy, `gateway_rail_stops_open` unchanged. A
  `hard_stop` after a restore means the restore itself is inconsistent; stop
  and investigate before starting settlement.
- **The books add up** in the restored database: no transfer allocated
  beyond its amount, every paid intent has exactly one fulfilment claim, every
  fulfilment has a settlement decision, no transfer claimed by two intents.
  These are the queries in `scripts/backup-drill.sh`.

### Why a re-run does not pay twice

A restore rewinds the database to a consistent moment. The processes then
redo the work after that moment, and each step is guarded by a key the
restored rows already carry:

- **Chain facts are identified by the chain.** A canonical transfer is unique
  by chain, network, environment, transaction hash and event index, and an
  observation by its source and assertion. Re-reading the same blocks cannot
  create a second transfer for the same event.
- **A transfer is claimed once.** `chain_transfer_intent_claims` is keyed by
  the transfer, and the database refuses an allocation that exceeds its
  transfer.
- **An intent is fulfilled once.** `payment_fulfillments` is keyed by the
  payment intent and is written in the same transaction as the settlement. A
  claim exists in the restored database exactly when its settlement committed
  before the restore point; otherwise neither exists and the settlement runs
  once more.
- **The outbox is part of the same transaction.** An event exists exactly
  when the change it announces does. Its delivery state is what can be lost.

### Checked by a person

- **Webhooks delivered after the restore point are sent again with the same
  event id.** An event written before the restore point but delivered after
  it has lost its `delivered_at` and is delivered again. Merchants deduplicate
  by event id, as the [integration guide](merchant-integration.md#4-the-webhook)
  already requires; confirm with them that they do.
- **Payments settled after the restore point are settled again with a new
  event id.** Event ids are generated when the settlement commits, so the
  re-run writes a new `payment_intent.paid` for the same `payment_intent_id`.
  A merchant that deduplicates only by event id would see a second payment.
  Tell every merchant the restore window, and that fulfilment must also be
  idempotent on the payment intent id.
- **API writes after the restore point are gone.** Intents and quotes created
  after it do not exist; their ids answer `404`, and a merchant's retry with
  the same `Idempotency-Key` creates a new intent. A payer who paid a quote
  issued after the restore point paid an amount nothing reserves: the transfer
  lands in `GET /v1/operator/unmatched-transfers` and needs an operator's
  `honor` or `reject`.
- **Operator and admin actions after the restore point are gone**, including
  security actions. Re-apply from your own records and the audit trail of the
  old instance: API key revocations (a key revoked after the restore point is
  valid again), webhook secret rotations, collector retirements, rail stops
  opened or cleared, manual resolutions, price and rail-health evidence.
- **Observers can still read what they missed.** The restored cursors point
  at the restore point; confirm each provider still serves blocks that old,
  and that the late-payment windows of affected quotes have not already ended.

## Monthly drill

A restore nobody has practised is an assumption. Once a month:

1. **Pick a target time** at random inside the retention window, not "latest".
2. **Restore to a scratch instance** in an isolated network. Never start the
   outbox against it (it would deliver real webhooks) and never point a
   production process at it.
3. **Migrate only** with the image production runs now, then apply the grants.
4. **Run the checks**: the verification queries of `scripts/backup-drill.sh`,
   the API self-check (`/health/ready`), and one reconciler run.
5. **Compare with production**: for append-only tables (`payment_events`,
   `domain_events`, `audit_events`, `chain_transfers`), the restored count
   equals the production count of rows created at or before the target time.
6. **Record** the date, the target time, the achieved RPO (the target time
   minus the newest `created_at` in `payment_events` of the restored
   database, against the recorded activity just before it), the measured RTO,
   every check with PASS or FAIL, and who ran it. A FAIL is an incident.
7. **Drop the scratch instance.**

### The local drill script

`scripts/backup-drill.sh` runs the logical variant against the development
PostgreSQL from `compose.yaml`:

1. `pg_dump` of the source database (read only; default `gateway`);
2. restore into a scratch database (default `gateway_restore_drill`; the name
   must contain `drill`, so a typo cannot drop a real database);
3. a migrate-only run of the gateway image against the scratch database
   (`GATEWAY_RUN_MIGRATIONS=true`, `GATEWAY_MIGRATE_ONLY=true`);
4. PASS or FAIL per check: row counts of every money table equal the source,
   the schema version is not behind the source, no allocation exceeds its
   transfer, every paid intent has exactly one fulfilment claim, every
   fulfilment has a settlement decision, no transfer is claimed twice, no
   outbox event is both delivered and dead-lettered;
5. drops the scratch database unless `DRILL_KEEP_SCRATCH=1`, and exits
   non-zero on any FAIL.

`psql`, `pg_dump` and `pg_restore` run inside the PostgreSQL container
through `docker exec` (`DRILL_PG_CONTAINER`, default
`crypto-gateway-project-postgres-1`), so the client always matches the server
version. With `DRILL_PG_CONTAINER=` (empty) it uses local clients and
`PGHOST`/`PGPORT` instead. The migrate step runs `DRILL_IMAGE` (default
`crypto-gateway-project:local`) and reaches the database at
`host.docker.internal`; override `DRILL_MIGRATE_URL` elsewhere. The image must
contain every migration the source has applied, or the migrate step fails,
which is the same refusal a real restore would meet.

```sh
docker build -t crypto-gateway-project:local .
DRILL_SOURCE_DB=gateway scripts/backup-drill.sh
# checks only, against a scratch database kept by an earlier run
DRILL_KEEP_SCRATCH=1 scripts/backup-drill.sh && scripts/backup-drill.sh --verify-only
```

The row counts are compared with the source right after the dump, so the
source must be quiet during the run; against a database taking writes the
count check fails by design. That is why the monthly production drill compares
counts up to the target time instead.
