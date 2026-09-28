#!/usr/bin/env bash
# End-to-end run of the unmodified gateway against the chain simulator.
#
# Testing only. It resets the database it is given, so it refuses to run unless
# E2E_RESET_DATABASE=yes and the database name contains "sim" or "e2e".
#
#   GATEWAY_DATABASE_URL=postgres://gateway:gateway@127.0.0.1:54329/gateway_sim \
#   E2E_RESET_DATABASE=yes tools/chain-simulator/e2e.sh
#
# Needs: the gateway-api and gateway-worker binaries (BIN, default
# target/debug; `cargo build --locked -p gateway-api -p gateway-worker`),
# psql, python3, openssl, curl. Everything listens on 127.0.0.1.
#
# The webhook leg: the gateway delivers only to https URLs on port 443 that
# resolve to public addresses, so a receiver on this machine cannot be reached
# directly. Set WEBHOOK_URL to a public https URL that forwards to
# RECEIVER_PORT (a tunnel) and the run also waits for the receiver to verify
# the signed payment_intent.paid event. Without it the endpoint is registered
# on a reserved, unresolvable name and the run shows the outbox attempting
# delivery and refusing to resolve it.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${BIN:-$ROOT/target/debug}
WORK=${E2E_WORKDIR:-$(mktemp -d)}
API_PORT=${API_PORT:-18080}
SIM_PORT=${SIM_PORT:-18090}
RECEIVER_PORT=${RECEIVER_PORT:-18081}
WEBHOOK_URL=${WEBHOOK_URL:-https://merchant-receiver.invalid/webhooks/gateway}
DB=${GATEWAY_DATABASE_URL:?GATEWAY_DATABASE_URL names a disposable database}

db_name=${DB##*/}
db_name=${db_name%%\?*}
if [[ ${E2E_RESET_DATABASE:-} != yes || ! $db_name =~ (sim|e2e) ]]; then
  echo "refused: this run drops every table in $db_name; set E2E_RESET_DATABASE=yes on a database named *sim* or *e2e*" >&2
  exit 2
fi

pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
json() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
sql() { PGOPTIONS=--client-min-messages=warning psql "$DB" -X -q -At -v ON_ERROR_STOP=1 "$@"; }

export GATEWAY_DATABASE_URL=$DB
export GATEWAY_CHAIN=tron GATEWAY_NETWORK=simulator GATEWAY_CHAIN_ENVIRONMENT=testnet
export GATEWAY_EXPECTED_COLLECTORS=TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe
export GATEWAY_EXPECTED_ASSETS=tron:simulator:TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN
GATEWAY_WEBHOOK_MASTER_KEY=$(openssl rand -hex 32)
SIMULATOR_CONTROL_TOKEN=$(openssl rand -hex 16)
OPERATOR_KEY=$(openssl rand -hex 24)
export GATEWAY_WEBHOOK_MASTER_KEY SIMULATOR_CONTROL_TOKEN OPERATOR_KEY

step "reset $db_name, migrate, seed the simulator rail"
sql -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
GATEWAY_RUN_MIGRATIONS=true GATEWAY_MIGRATE_ONLY=true "$BIN/gateway-api" >"$WORK/migrate.log" 2>&1
sql -f "$ROOT/scripts/seed-simulator-rail.sql"
sql -v key_id=00000000-0000-7000-8000-000000000003 -v api_key_prefix=cgop_sim \
  -v api_key_sha256_hex="$(printf '%s' "$OPERATOR_KEY" | sha256sum | cut -d' ' -f1)" \
  -f "$ROOT/scripts/create-dev-operator.sql"
echo "migrated and seeded: asset USDT-SIM TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN, collector TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe"

step "merchant, API key and webhook endpoint through the admin CLI"
admin() { "$BIN/gateway-worker" admin "$@"; }
MERCHANT_ID=$(admin merchant-create --actor e2e --external-id sim-shop --name 'Simulator shop' \
  --collector-policy shared | json '["merchant_id"]')
MERCHANT_KEY=$(admin api-key-issue --actor e2e --merchant "$MERCHANT_ID" --label simulator | json '["secret"]')
export MERCHANT_KEY
endpoint=$(admin webhook-add --actor e2e --merchant "$MERCHANT_ID" --url "$WEBHOOK_URL")
ENDPOINT_ID=$(json '["endpoint_id"]' <<<"$endpoint")
WEBHOOK_SECRET=$(json '["signing_secret"]' <<<"$endpoint")
echo "merchant $MERCHANT_ID, endpoint $ENDPOINT_ID -> $WEBHOOK_URL (secrets not printed)"

step "simulator, receiver, API, workers"
python3 "$ROOT/tools/chain-simulator/simulator.py" --port "$SIM_PORT" \
  --block-interval-seconds "${SIM_BLOCK_INTERVAL_SECONDS:-1}" >"$WORK/simulator.log" 2>&1 &
pids+=($!)
(cd "$ROOT/examples/webhook-receiver-python" && WEBHOOK_SECRETS=$WEBHOOK_SECRET PORT=$RECEIVER_PORT \
  exec python3 server.py) >"$WORK/receiver.log" 2>&1 &
pids+=($!)
GATEWAY_BIND_ADDRESS=127.0.0.1:$API_PORT GATEWAY_EXPIRY_ENABLED=false RUST_LOG=gateway_api=info \
  "$BIN/gateway-api" >"$WORK/api.log" 2>&1 &
pids+=($!)
chain="http://127.0.0.1:$SIM_PORT"
export RUST_LOG=gateway_worker=info,gateway_scheduler=info,gateway_tron=info,gateway_webhook=info
worker() {
  local name=$1; shift
  env GATEWAY_INSTANCE_NAME="$name" "$@" "$BIN/gateway-worker" >"$WORK/$name.log" 2>&1 &
  pids+=($!)
}
# Two observers in different provider groups, one per lane, and the verifier
# under its own source. All read the same simulator: their independence is
# simulated.
worker sim-observer-a GATEWAY_WORKER_ROLES=observer GATEWAY_OBSERVER_SOURCE_KEY=sim-index \
  GATEWAY_TRON_BASE_URL="$chain" GATEWAY_TRON_LANE=address_index GATEWAY_OBSERVER_INTERVAL_SECONDS=2
worker sim-observer-b GATEWAY_WORKER_ROLES=observer GATEWAY_OBSERVER_SOURCE_KEY=sim-node \
  GATEWAY_TRON_BASE_URL="$chain" GATEWAY_TRON_LANE=block_range GATEWAY_OBSERVER_INTERVAL_SECONDS=2 \
  GATEWAY_TRON_BOOTSTRAP_LOOKBACK_BLOCKS=20
worker sim-verifier GATEWAY_WORKER_ROLES=verifier GATEWAY_VERIFIER_SOURCE_KEY=sim-verifier \
  GATEWAY_VERIFIER_TRON_BASE_URL="$chain" GATEWAY_VERIFIER_INTERVAL_SECONDS=2
worker sim-payment GATEWAY_WORKER_ROLES=expiry,settlement,outbox GATEWAY_SETTLEMENT_INTERVAL_SECONDS=2 \
  GATEWAY_OUTBOX_INTERVAL_SECONDS=2 GATEWAY_EXPIRY_INTERVAL_SECONDS=30
worker sim-reconciler GATEWAY_WORKER_ROLES=reconciler GATEWAY_RECONCILER_INTERVAL_SECONDS=30 \
  GATEWAY_RECONCILER_BATCH_LIMIT=1 GATEWAY_RECONCILER_MAX_BATCHES_PER_TICK=1

for _ in $(seq 1 30); do
  curl -sf "http://127.0.0.1:$API_PORT/health/ready" >/dev/null && break
  sleep 1
done
curl -sf "http://127.0.0.1:$API_PORT/health/ready" | json '["ready"]' | sed 's/^/api ready: /'
sleep 3
for log in "$WORK"/sim-*.log; do
  grep -q '"gateway worker running"' "$log" || { echo "worker failed to start: $log" >&2; cat "$log" >&2; exit 1; }
done
echo "5 worker processes running; logs in $WORK"

export GATEWAY_API_URL="http://127.0.0.1:$API_PORT" SIMULATOR_URL=$chain
pay() { python3 "$ROOT/scripts/simulate-payment.py" "$@"; }

step "exact payment: intent -> quote -> simulated transfer -> paid"
pay --create --amount-minor 4999 --feed-evidence --timeout 180 | tee "$WORK/exact.out"
PAID_INTENT=$(sed -n 's/.*intent \([0-9a-f-]*\) created.*/\1/p' "$WORK/exact.out")

step "webhook for $PAID_INTENT"
for _ in $(seq 1 30); do
  delivered=$(sql -c "SELECT count(*) FROM domain_events WHERE event_type = 'payment_intent.paid'
                      AND aggregate_id = '$PAID_INTENT' AND (delivered_at IS NOT NULL OR attempts > 0)")
  [[ $delivered -gt 0 ]] && break
  sleep 2
done
sql -F ' ' -c "SELECT 'domain_event', event_type, 'attempts=' || attempts,
                      CASE WHEN delivered_at IS NOT NULL THEN 'delivered'
                           WHEN dead_lettered_at IS NOT NULL THEN 'dead_lettered'
                           ELSE 'pending' END,
                      coalesce(last_error, '')
                 FROM domain_events WHERE aggregate_id = '$PAID_INTENT'"
sql -F ' ' -c "SELECT 'delivery attempt', d.attempt, coalesce(d.response_status::text, '-'), coalesce(d.error, '')
                 FROM webhook_deliveries d JOIN domain_events e ON e.id = d.event_id
                WHERE e.aggregate_id = '$PAID_INTENT' ORDER BY d.attempt"
if grep -q 'webhook_handled .*type=payment_intent.paid' "$WORK/receiver.log"; then
  grep 'webhook_handled' "$WORK/receiver.log" | sed 's/^/receiver: /'
elif [[ $WEBHOOK_URL == *.invalid/* ]]; then
  echo "no public receiver URL was given: delivery to $WEBHOOK_URL is refused by the sender, as designed"
else
  echo "the receiver has not verified the event yet; see $WORK/receiver.log" >&2
  exit 1
fi

step "wrong amount: the transfer is unmatched and queued for an operator"
pay --create --amount-minor 1250 --mode wrong-amount --timeout 180

step "failed transaction: a reverted transfer pays nothing"
pay --create --amount-minor 700 --mode failed --timeout 180

step "the verifier's verdict on every chain event"
sql -F ' ' -c "SELECT tx_hash, verdict, coalesce(reason, '-'), 'evidence=' || evidence_count,
                      'independent_groups=' || independent_groups
                 FROM chain_event_verdicts ORDER BY decided_at"

step "operator overview"
curl -sf "http://127.0.0.1:$API_PORT/v1/operator/overview" -H "Authorization: Bearer $OPERATOR_KEY" \
  | python3 -m json.tool --compact
echo
echo "end-to-end run complete; logs in $WORK"
