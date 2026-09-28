# Chain simulator

> **Testing the integration only.** The simulator is an in-memory TRON node
> that answers exactly the calls the gateway makes. A payment that settles on
> it proves that your wiring works: API keys, quotes, the worker processes,
> the database, the webhook path. **It proves nothing about the real chain**:
> not that TronGrid answers the same way, not that a real USDT transfer is
> detected, not that finality holds. **Never point a mainnet deployment at
> it.** The "two independent sources" of a simulator run both read this one
> process, so their independence is simulated; the provider groups exercise
> the gateway's counting, not any real disagreement between providers. For
> the real chain path, use the Nile testnet (see the main README).

`simulator.py` is Python 3.10+, standard library only. It serves:

| Call | What it returns |
|---|---|
| `POST /wallet/getnowblock`, `POST /walletsolidity/getnowblock` | the head block, and the solidified head `solid_lag` blocks behind it |
| `POST /wallet/getblockbynum {"num": n}` | block `n` once it is produced; `{}` before |
| `POST /wallet/gettransactioninfobyblocknum {"num": n}` | every transaction info in block `n` |
| `POST /wallet/gettransactioninfobyid {"value": id}` | one transaction info; `{}` for an unknown id |
| `GET /v1/accounts/{T...}/transactions/trc20?only_to=true&limit=..&min_timestamp=..` | the address index: successful transfers to the address, oldest first |

The `walletsolidity/` variants of the block and transaction calls answer only
up to the solidified head, as a node does. A transaction info carries a TRC20
`Transfer` log (event signature, zero-padded `from` and `to`, the amount as one
32-byte word, the contract without its `41` prefix): the shape the gateway's
decoder (`crates/gateway-tron/src/event.rs`) reads and its fixtures pin.

## The chain

- Block `n` exists once `genesis + n * interval` has passed, so the head
  advances by itself (default every 3 s) and every block's id, parent hash and
  millisecond timestamp are a function of the seed and the configuration. The
  first 16 hex digits of a block id are its number, as on TRON.
- The solidified head trails the head by `--solid-lag` blocks (default 19).
- A transfer submitted through the control API is mined into the block after
  the current head. A block's contents are frozen before anyone can read it.
- The same seed and the same sequence of submissions produce the same chain,
  the same block ids and the same transaction ids.
- No reorganisations, no fees, no balances: anyone can "pay" any amount.
- A reverted transaction (`"status": "failed"`) keeps its `Transfer` log,
  which a real node does not emit. The simulator keeps it so the gateway's
  refusal of a failed execution is exercised instead of never being reached.

## Control API

On the same port, under `/simulator/`. It answers a request from a loopback
address, or one carrying `X-Simulator-Token: <SIMULATOR_CONTROL_TOKEN>`;
anyone else gets `403`. The chain endpoints above are open, as a node's are.

```
POST /simulator/transfers
{"to": "T...", "amount_raw": "49990000", "contract": "T...", "from": "T...", "status": "success"}
```

`amount_raw` is a decimal string; a JSON number, a fraction, zero or more than
256 bits is refused. `contract` defaults to the simulator's own token
(`TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN` for the default seed) and `from` to a
derived payer; `status` is `success` (default) or `failed`. The real USDT
contracts (mainnet `TR7NHq...Lj6t`, Nile `TXYZop...keBf`) are refused: a
simulator never produces a reading of a real token. The answer names the
transaction id, its block and the block at which it will be solidified.

`GET /simulator/state` returns the heads and every transaction submitted.

## Run it

```sh
python3 tools/chain-simulator/simulator.py --port 8090 --block-interval-seconds 3 --solid-lag 19
python3 -m unittest -v          # in tools/chain-simulator
```

| Flag | Environment | Default |
|---|---|---|
| `--host` | `SIMULATOR_HOST` | `127.0.0.1` |
| `--port` | `SIMULATOR_PORT` | `8090` |
| `--block-interval-seconds` | `SIMULATOR_BLOCK_INTERVAL_SECONDS` | `3` |
| `--solid-lag` | `SIMULATOR_SOLID_LAG` | `19` |
| `--start-height` | `SIMULATOR_START_HEIGHT` | `100` |
| `--seed` | `SIMULATOR_SEED` | `chain-simulator` |
|  | `SIMULATOR_CONTROL_TOKEN` | none: loopback only |

## Point the gateway at it

Nothing in the gateway changes. The TRON source already accepts an `http://`
base URL (a deployment's own node is usually reached that way), so every
observer and the verifier take the simulator's URL in
`GATEWAY_TRON_BASE_URL` / `GATEWAY_VERIFIER_TRON_BASE_URL`.

The rail lives on a database of its own and is described by
[`scripts/seed-simulator-rail.sql`](../../scripts/seed-simulator-rail.sql):
network `simulator`, environment `testnet`, the simulator's token as the asset
and a derived collector address, both pinned by the self-check:

```
GATEWAY_NETWORK=simulator
GATEWAY_CHAIN_ENVIRONMENT=testnet
GATEWAY_EXPECTED_COLLECTORS=TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe
GATEWAY_EXPECTED_ASSETS=tron:simulator:TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN
```

Its finality policy requires a reading the source calls `finalized`
(solidified) from two independent groups, and `min_confirmations` 0. A
reading's confirmations are counted from the solidified head the source
reported, and each lane reads a block about when it solidifies and does not
read it again; the verifier re-reads an event once. So a policy that asks for
confirmations beyond the solidified head is not reached by fresh evidence, and
a payment waits at `confirmed` indefinitely. That is what a first run with 19
did; the Nile seed (`scripts/seed-dev-rail.sql`) asks for 19 as well.

### Compose

The `simulator` profile in [`compose.yaml`](../../compose.yaml) creates the
`gateway_sim` database, migrates and seeds it, and runs the simulator, an API
on port 8081 and the workers:

```sh
echo "SIMULATOR_CONTROL_TOKEN=$(openssl rand -hex 16)" >> .env
echo "GATEWAY_WEBHOOK_MASTER_KEY=$(openssl rand -hex 32)" >> .env
docker compose --profile simulator up -d chain-simulator gateway-api-sim \
  sim-observer-a sim-observer-b sim-verifier sim-payment

export DB=postgres://gateway:gateway@127.0.0.1:54329/gateway_sim
OPERATOR_KEY=$(openssl rand -hex 24)
psql "$DB" -v key_id=00000000-0000-7000-8000-000000000003 -v api_key_prefix=cgop_sim \
  -v api_key_sha256_hex=$(printf '%s' "$OPERATOR_KEY" | sha256sum | cut -d' ' -f1) \
  -f scripts/create-dev-operator.sql
sim_admin() { docker compose run --rm -T --no-deps --entrypoint /usr/local/bin/gateway-worker \
  gateway-api-sim admin "$@"; }
MERCHANT=$(sim_admin merchant-create --actor "$USER" --external-id sim-shop \
  --name 'Simulator shop' --collector-policy shared | jq -r .merchant_id)
MERCHANT_KEY=$(sim_admin api-key-issue --actor "$USER" --merchant "$MERCHANT" \
  --label simulator | jq -r .secret)

export $(grep SIMULATOR_CONTROL_TOKEN .env) OPERATOR_KEY MERCHANT_KEY
export GATEWAY_API_URL=http://127.0.0.1:8081 SIMULATOR_URL=http://127.0.0.1:8090
python3 scripts/simulate-payment.py --create --amount-minor 4999 --feed-evidence
```

A request from the host reaches the simulator through the Docker network, not
loopback, so the control API needs the token there.

### scripts/simulate-payment.py

Given `--intent <id>`, or `--create` to make one, it asks for a quote (with
the idempotency key `simulate-quote-<intent id>`, so a second run replays the
same quote), refuses one that names a mainnet asset, pays the quoted
`amount_raw` of the quoted contract to the quoted collector through the
control API, and follows the outcome:

| `--mode` | Pays | Expected outcome |
|---|---|---|
| `exact` (default) | `amount_raw` | the intent becomes `paid`; otherwise the status it is stuck in is reported and the exit status is 1 |
| `wrong-amount` | `amount_raw - 1`, or `--amount-raw` | the transfer appears in `GET /v1/operator/unmatched-transfers` (needs `OPERATOR_KEY`) |
| `failed` | `amount_raw`, reverted | after the point a good payment would have settled, the intent is still unpaid |

`--feed-evidence` posts simulated price and rail-health evidence from two
groups first, with `OPERATOR_KEY`.

### The webhook leg

The gateway delivers a webhook only to an `https` URL on port 443 that
resolves to public addresses, over TLS verified against the public roots, and
the admin CLI refuses to register anything else. A receiver on your machine is
not such an address, and the simulator does not change that: it is the
gateway's protection against being used to reach internal hosts. To see the
signed `payment_intent.paid` verified by
[`examples/webhook-receiver-python`](../../examples/webhook-receiver-python),
put the receiver behind a public https URL (a tunnel) and register that URL.
Without one, the run below registers `https://merchant-receiver.invalid/...`
(a reserved name that never resolves) and shows the outbox attempting the
delivery and recording why it failed.

## End to end, in one command

[`e2e.sh`](e2e.sh) runs the whole unmodified pipeline on one machine: it
resets a disposable database, migrates and seeds it, creates an operator key,
a merchant, an API key and a webhook endpoint through the admin CLI, starts
the simulator, the receiver, the API and five worker processes (two observers
in different provider groups, one per lane, the verifier under its own
source, settlement with outbox and expiry, the reconciler), and then runs the
three modes above.

```sh
cargo build --locked -p gateway-api -p gateway-worker
GATEWAY_DATABASE_URL=postgres://gateway:gateway@127.0.0.1:54329/gateway_sim \
  E2E_RESET_DATABASE=yes tools/chain-simulator/e2e.sh
# optional: WEBHOOK_URL=https://<public tunnel to 127.0.0.1:18081>/webhooks/gateway
```

It refuses a database whose name does not contain `sim` or `e2e`. CI runs it
on every push (`simulator-e2e` in `.github/workflows/ci.yml`).

### Expected output

A run on 2026-09-28 (one-second blocks, solidified 19 blocks behind; no
public receiver URL; secrets are never printed):

```
$ GATEWAY_DATABASE_URL=postgres://gateway:gateway@host.docker.internal:54329/gateway_sim \n    E2E_RESET_DATABASE=yes E2E_WORKDIR=/tmp/e2e-run tools/chain-simulator/e2e.sh

== reset gateway_sim, migrate, seed the simulator rail
migrated and seeded: asset USDT-SIM TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN, collector TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe

== merchant, API key and webhook endpoint through the admin CLI
merchant 01a0e733-8431-76b3-881d-b55a7894fc16, endpoint 01a0e733-8613-7684-b2c1-af60764d43f2 -> https://merchant-receiver.invalid/webhooks/gateway (secrets not printed)

== simulator, receiver, API, workers
api ready: True
5 worker processes running; logs in /tmp/e2e-run

== exact payment: intent -> quote -> simulated transfer -> paid
[08:48:33] fed simulated price evidence (2 groups, 10000 raw units per cent) and rail health
[08:48:33] intent 01a0e733-976b-7232-ab3f-dc45e4140c5e created: 4999 USD minor units, status=requires_quote
[08:48:33] quote 01a0e733-97aa-7423-b0b8-6417f95bbc3b: pay amount_raw=49990000 of TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN (USDT-SIM, simulator/testnet) to TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe, expires 2026-09-28T09:03:33.194065Z
[08:48:33] simulator mined tx 3d4ef135ad403e0a9f453f3d07a2ec384df66462c693f81bd3d86427d72a00ea in block 105 (amount_raw=49990000, status=success; solidified at block 124)
[08:48:33] intent 01a0e733-976b-7232-ab3f-dc45e4140c5e status=awaiting_payment
[08:48:57] intent 01a0e733-976b-7232-ab3f-dc45e4140c5e status=paid

== webhook for 01a0e733-976b-7232-ab3f-dc45e4140c5e
domain_event payment_intent.paid attempts=1 pending delivery_failed
delivery attempt 1 - webhook endpoint hostname could not be resolved
no public receiver URL was given: delivery to https://merchant-receiver.invalid/webhooks/gateway is refused by the sender, as designed

== wrong amount: the transfer is unmatched and queued for an operator
[08:49:06] intent 01a0e734-1a89-74bf-bb27-0eb914dc9532 created: 1250 USD minor units, status=requires_quote
[08:49:06] quote 01a0e734-1abb-70ab-8c4b-4edb9ebf02a0: pay amount_raw=12500000 of TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN (USDT-SIM, simulator/testnet) to TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe, expires 2026-09-28T09:04:06.746851Z
[08:49:06] simulator mined tx bca6c2e2c88a61a652166639abbae54b3d5fd4c0b32a15448374cb99af10171a in block 138 (amount_raw=12499999, status=success; solidified at block 157)
[08:49:33] unmatched transfer 01a0e734-24f5-7077-ae73-78ddaf276590: tx bca6c2e2c88a61a652166639abbae54b3d5fd4c0b32a15448374cb99af10171a amount_raw=12499999 finality=finalized; intent 01a0e734-1a89-74bf-bb27-0eb914dc9532 status=awaiting_payment

== failed transaction: a reverted transfer pays nothing
[08:49:33] intent 01a0e734-825e-706c-9469-319c9ea2acd6 created: 700 USD minor units, status=requires_quote
[08:49:33] quote 01a0e734-828a-7789-aff0-c615dbcd61c8: pay amount_raw=7000000 of TRjounaPuqUPZa1mN7yseWTZbqNpSUfXsN (USDT-SIM, simulator/testnet) to TELhoqbrn7hQiiBMkLAfn63dX7SsPLyfTe, expires 2026-09-28T09:04:33.322937Z
[08:49:33] simulator mined tx f06943e27d1ce128ba0e832590a2009376b649b749d56018b0ff44769a5f2a43 in block 165 (amount_raw=7000000, status=failed; solidified at block 184)
[08:50:45] solid head passed block 206; intent 01a0e734-825e-706c-9469-319c9ea2acd6 status=awaiting_payment (a reverted transfer pays nothing)

== the verifier's verdict on every chain event
3d4ef135ad403e0a9f453f3d07a2ec384df66462c693f81bd3d86427d72a00ea verified finalized evidence=4 independent_groups=3
bca6c2e2c88a61a652166639abbae54b3d5fd4c0b32a15448374cb99af10171a verified finalized evidence=4 independent_groups=3
f06943e27d1ce128ba0e832590a2009376b649b749d56018b0ff44769a5f2a43 rejected failed_execution evidence=2 independent_groups=0

== operator overview
{"components":[{"component":"reconciler","state":"ok","detail":null,"since":"2026-09-28T08:48:29.23321Z","updated_at":"2026-09-28T08:50:29.236398Z"}],"open_rail_stops":[],"latest_reconciliation":[{"id":"01a0e735-5ce9-7502-a2fa-c33215cb953c","kind":"incremental","status":"ok","started_at":"2026-09-28T08:50:29.179397Z","finished_at":"2026-09-28T08:50:29.225553Z"}],"open_discrepancies":[],"transfers_by_processing_state":[{"state":"settled","count":1},{"state":"unmatched","count":1}],"payment_intents_by_status":[{"state":"awaiting_payment","count":2},{"state":"paid","count":1}],"outbox_pending":2,"outbox_dead_lettered":0,"observation_conflicts_open":0}

end-to-end run complete; logs in /tmp/e2e-run
```

What it shows: the exact payment went from `awaiting_payment` to `paid` in
about 24 seconds (19 one-second blocks to solidify, then the scan, verify and
settle intervals); the verifier recorded it as verified and finalized on four
readings from three provider groups; settlement wrote `payment_intent.paid` to
the outbox and the outbox tried to deliver it and recorded why it could not.
The underpayment of one raw unit became an unmatched, finalized transfer in the
operator queue and left its intent unpaid. The reverted transaction was
rejected as `failed_execution` and paid nothing.
