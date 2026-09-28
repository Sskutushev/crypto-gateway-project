#!/usr/bin/env bash
# Backup and restore drill against the local Compose PostgreSQL.
#
# Takes a logical dump of a source database, restores it into a scratch
# database, applies migrations to it with the gateway image in migrate-only
# mode, and checks that the restored books match the source and still add up.
# Prints one PASS or FAIL line per check and exits non-zero on any FAIL.
#
# The source database is only ever read (pg_dump). The scratch database is
# dropped and recreated; its name must contain "drill" so a typo cannot point
# the drop at a real database.
#
# psql, pg_dump and pg_restore run inside the PostgreSQL container through
# `docker exec`, so their version always matches the server. Set
# DRILL_PG_CONTAINER= (empty) to use local clients with PGHOST/PGPORT instead.
#
# Usage:
#   scripts/backup-drill.sh                 # full drill
#   scripts/backup-drill.sh --verify-only   # checks only, against an existing scratch database
#
# Environment (defaults in brackets):
#   DRILL_PG_CONTAINER   [crypto-gateway-project-postgres-1]
#   DRILL_SOURCE_DB      [gateway]
#   DRILL_SCRATCH_DB     [gateway_restore_drill]
#   PGUSER / PGPASSWORD  [gateway / gateway]
#   PGHOST / PGPORT      [127.0.0.1 / 54329]   only without a container
#   DRILL_IMAGE          [crypto-gateway-project:local]  image that runs the migrations
#   DRILL_MIGRATE_URL    [postgres://$PGUSER:$PGPASSWORD@host.docker.internal:$PGPORT/$DRILL_SCRATCH_DB]
#   DRILL_KEEP_SCRATCH   [0]  1 keeps the scratch database for inspection
set -euo pipefail

container=${DRILL_PG_CONTAINER-crypto-gateway-project-postgres-1}
source_db=${DRILL_SOURCE_DB:-gateway}
scratch_db=${DRILL_SCRATCH_DB:-gateway_restore_drill}
export PGUSER=${PGUSER:-gateway}
export PGPASSWORD=${PGPASSWORD:-gateway}
export PGHOST=${PGHOST:-127.0.0.1}
export PGPORT=${PGPORT:-54329}
image=${DRILL_IMAGE:-crypto-gateway-project:local}
migrate_url=${DRILL_MIGRATE_URL:-postgres://$PGUSER:$PGPASSWORD@host.docker.internal:$PGPORT/$scratch_db}
keep_scratch=${DRILL_KEEP_SCRATCH:-0}

verify_only=0
case "${1:-}" in
  "") ;;
  --verify-only) verify_only=1 ;;
  *) echo "usage: $0 [--verify-only]" >&2; exit 2 ;;
esac

if [[ ! "$scratch_db" =~ ^[a-z0-9_]*drill[a-z0-9_]*$ ]]; then
  echo "refusing: scratch database '$scratch_db' must be lower-case and contain 'drill'" >&2
  exit 2
fi
if [[ "$scratch_db" == "$source_db" ]]; then
  echo "refusing: scratch and source are the same database" >&2
  exit 2
fi

# Windows shells rewrite absolute paths in arguments to docker; the variable
# turns that off and is ignored everywhere else.
export MSYS_NO_PATHCONV=1

pg() {
  local tool=$1; shift
  if [[ -n "$container" ]]; then
    docker exec -i -e PGPASSWORD="$PGPASSWORD" "$container" "$tool" -U "$PGUSER" "$@"
  else
    "$tool" -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" "$@"
  fi
}

sql() {
  local db=$1 query=$2
  pg psql -d "$db" -X -v ON_ERROR_STOP=1 -Atq -c "$query" | tr -d '\r'
}

failures=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }

workdir=$(mktemp -d)
cleanup() {
  rm -rf "$workdir"
  if [[ $verify_only -eq 0 && "$keep_scratch" != "1" ]]; then
    pg psql -d postgres -X -q -c "DROP DATABASE IF EXISTS $scratch_db WITH (FORCE)" >/dev/null 2>&1 \
      || echo "warning: could not drop scratch database $scratch_db" >&2
  fi
}
trap cleanup EXIT

if [[ $verify_only -eq 0 ]]; then
  dump="$workdir/source.dump"
  echo "== dump $source_db"
  pg pg_dump -d "$source_db" --format=custom --no-owner --no-privileges > "$dump"
  echo "   $(wc -c < "$dump" | tr -d ' ') bytes"

  echo "== restore into $scratch_db"
  pg psql -d postgres -X -q -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS $scratch_db WITH (FORCE)" \
    -c "CREATE DATABASE $scratch_db"
  pg pg_restore -d "$scratch_db" --no-owner --no-privileges --exit-on-error --single-transaction < "$dump"
  pass "restore completed without errors"

  echo "== migrate-only with $image"
  if docker run --rm \
      --add-host host.docker.internal:host-gateway \
      -e GATEWAY_DATABASE_URL="$migrate_url" \
      -e GATEWAY_RUN_MIGRATIONS=true \
      -e GATEWAY_MIGRATE_ONLY=true \
      -e RUST_LOG=info \
      "$image"; then
    pass "migrate-only run against the restored database"
  else
    fail "migrate-only run against the restored database (an image older than the backup refuses migrations it does not know)"
  fi
fi

echo "== verify"

# Tables whose rows are money or the record of money moving. A restore that
# loses any of them loses history nobody can recompute.
money_tables=(
  payment_intents payment_quotes payment_attempts amount_leases amount_lease_history
  chain_transfers chain_transfer_intent_claims payment_allocations
  payment_settlement_decisions payment_fulfillments payment_events domain_events
  webhook_deliveries manual_resolution_requests overpayment_remainder_dispositions
  reconciliation_runs reconciliation_discrepancies rail_stops audit_events
)
for table in "${money_tables[@]}"; do
  src=$(sql "$source_db" "SELECT count(*) FROM $table")
  dst=$(sql "$scratch_db" "SELECT count(*) FROM $table")
  if [[ "$src" == "$dst" ]]; then
    pass "row count $table: $dst"
  else
    fail "row count $table: source $src, restored $dst"
  fi
done

src=$(sql "$source_db" "SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success")
dst=$(sql "$scratch_db" "SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success")
failed=$(sql "$scratch_db" "SELECT count(*) FROM _sqlx_migrations WHERE NOT success")
if [[ "$failed" == "0" && "$dst" -ge "$src" ]]; then
  pass "schema version: source $src, restored $dst, no failed migration"
else
  fail "schema version: source $src, restored $dst, failed migrations $failed"
fi

check_zero() {
  local label=$1 query=$2 n
  n=$(sql "$scratch_db" "$query")
  if [[ "$n" == "0" ]]; then pass "$label"; else fail "$label: $n offending rows"; fi
}

check_zero "no transfer is allocated beyond its amount" "
  SELECT count(*) FROM (
    SELECT t.id FROM chain_transfers t
    JOIN payment_allocations a ON a.transfer_id = t.id
    GROUP BY t.id, t.amount_raw
    HAVING sum(a.allocated_raw) > t.amount_raw) x"

check_zero "every paid intent has a fulfilment claim" "
  SELECT count(*) FROM payment_intents i
  WHERE i.status = 'paid'
    AND NOT EXISTS (SELECT 1 FROM payment_fulfillments f WHERE f.payment_intent_id = i.id)"

check_zero "no intent has more than one fulfilment claim" "
  SELECT count(*) FROM (
    SELECT payment_intent_id FROM payment_fulfillments
    GROUP BY payment_intent_id HAVING count(*) > 1) x"

check_zero "every fulfilment has a settlement decision behind it" "
  SELECT count(*) FROM payment_fulfillments f
  WHERE NOT EXISTS (
    SELECT 1 FROM payment_settlement_decisions d
    WHERE d.payment_intent_id = f.payment_intent_id)"

check_zero "no transfer is claimed by two intents" "
  SELECT count(*) FROM (
    SELECT transfer_id FROM chain_transfer_intent_claims
    GROUP BY transfer_id HAVING count(*) > 1) x"

check_zero "no outbox event is both delivered and dead-lettered" "
  SELECT count(*) FROM domain_events
  WHERE delivered_at IS NOT NULL AND dead_lettered_at IS NOT NULL"

echo
if [[ $failures -eq 0 ]]; then
  echo "drill: PASS"
else
  echo "drill: FAIL ($failures failed checks)"
  exit 1
fi
