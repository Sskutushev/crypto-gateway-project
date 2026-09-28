# Webhook receiver (Python)

The same receiver as [`../webhook-receiver-typescript`](../webhook-receiver-typescript),
in Python 3.10+ with the standard library only (`http.server`, `hmac`,
`hashlib`).

1. Reads the raw body (at most 1 MiB, `Content-Length` required) and verifies
   `Gateway-Signature` against those exact bytes before parsing JSON.
2. Refuses a timestamp more than `WEBHOOK_TOLERANCE_SECONDS` (default 300)
   from the local clock, in either direction.
3. Accepts several `v1=` values and several local secrets; any match is
   valid. Comparison uses `hmac.compare_digest`.
4. Checks that `Gateway-Event-Id` equals the envelope `id`.
5. Deduplicates by event id and answers `2xx` only after the handler returns.
6. Fulfils an order only on `payment_intent.paid`.

## Run

```sh
WEBHOOK_SECRETS=<hex secret from the operator> PORT=8080 python server.py
python -m unittest -v
```

| Variable | Default | Meaning |
|---|---|---|
| `WEBHOOK_SECRETS` | required | Comma-separated hex secrets, newest first. |
| `WEBHOOK_TOLERANCE_SECONDS` | `300` | Replay window. |
| `WEBHOOK_PATH` | `/webhooks/gateway` | The only path that accepts events. |
| `PORT` | `8080` | Listen port. |

`http.server` is not hardened for the open internet. Put the process behind a
TLS-terminating reverse proxy; the gateway delivers only to `https://` URLs on
port 443 that resolve to public addresses.

## Signature

```
signed = "<t>" + "." + <raw body bytes>
v1     = hex(HMAC-SHA256(bytes.fromhex(secret), signed))
```

The key is the 32 bytes decoded from the hex secret, not the hex text. Event
types, delivery retries, key rotation and the test vector are described in
[`../webhook-receiver-typescript/README.md`](../webhook-receiver-typescript/README.md);
`test_receiver.py` uses the same vector.

## Production notes

- `InMemoryProcessedEvents` is lost on restart and not shared between
  processes. Persist event ids under a unique constraint, in the same
  transaction as the business effect.
- Key fulfilment by the payment intent id (`data.id`) as well as by event id.
