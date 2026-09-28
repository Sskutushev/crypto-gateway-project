# Crypto Gateway: open-source, non-custodial USDT TRC20 payment gateway

A self-hosted payment gateway, written in Rust with PostgreSQL as its only
source of truth, that accepts and verifies incoming USDT payments on TRON for
a merchant's orders without ever holding a private key. A merchant creates a
payment intent, quotes it in USDT, and shows the payer an exact amount and an
address. The gateway watches the chain through independent providers,
verifies the transfer, settles the order exactly once, and tells the merchant
with a signed webhook.

> **Status: not production-ready.** Every layer from payment intent to signed
> webhook exists and is verified by unit tests, PostgreSQL scenarios and
> seeded property tests. No release has been tagged, no rail has run a
> sustained testnet soak with two genuinely independent providers, there has
> been no mainnet pilot, and no external security audit has been done. Do not
> put real money through it until the steps in
> [Owner setup](docs/owner-setup.md) are complete. Details:
> [Implementation status](docs/implementation-status.md).

## Whose wallet receives the money

The merchant's. The gateway is **non-custodial**: it holds no private keys,
signs nothing, sends nothing and keeps no balances. It only watches public
addresses.

- New merchants default to the `own` collector policy: quotes use only
  addresses registered to that merchant, and an address is accepted only on a
  signature from the wallet that holds it (TronLink `signMessageV2`).
- An operator may instead run a merchant on `shared` addresses the operator
  owns, and settles with the merchant outside the gateway. There is no
  fallback between the two policies; the database refuses a quote on another
  merchant's address.
- A stolen gateway host can lie about receipts, and nothing else: there is no
  key on it to steal.

See [Who owns the receiving address](docs/merchant-integration.md#who-owns-the-receiving-address).

## Supported network

**USDT TRC20 on TRON only**, on mainnet or the Nile testnet, with one
allowlisted token contract per asset. Payments are incoming only: no payouts,
withdrawals or automatic refunds. ERC20 and TON are planned behind the same
observer and verifier interface; neither exists yet.
Full table: [Scope and limits](docs/scope-and-limits.md).

## What happens when a payment is not exact

A quote is an exact amount at an address inside a time window, and only an
exact, final, verified transfer settles automatically. Everything else is
kept, never absorbed, and waits for a person:

| Case | Result |
|---|---|
| Underpayment | The transfer is `unmatched`; an operator may honour it, the intent becomes `partially_paid`. Never fulfil on it. |
| Overpayment | `unmatched`; honoured for the outstanding amount, the intent becomes `paid` and an `OVERPAID` event names the remainder, which the operator records as refunded or credited outside the gateway. |
| Late payment | Inside the late-payment window it is held for an operator to honour; after the window it can only be rejected and handled outside the gateway. |
| Wrong network | Never seen: observers read only the configured chain. Recovery is between the payer and whoever holds the address's key. |
| Wrong token | Recorded as an observation with no asset; never becomes a payment. |

Every case, with what the merchant sees: [Scope and limits](docs/scope-and-limits.md#exceptional-cases-on-tron).

## Integrate

Two HTTP calls, a status read and one signed webhook:
[Merchant integration](docs/merchant-integration.md) and the
[API reference](docs/openapi.json). Working webhook receivers in TypeScript
and Python, a create-payment script and a Postman collection are in
[`examples/`](examples/).

## Install

The whole system is one container image with seven processes (the API and
one worker per role) and an admin CLI, against a PostgreSQL 16 you run with
backups and TLS.

- **Try it locally** in about ten minutes with Docker Compose: the
  [quickstart in the README](README.md#quickstart-about-ten-minutes).
- **Deploy** with the production Compose file or the Kubernetes
  kustomization: [Deployment](docs/deployment.md).
- **Before real money**, every account, key and secret an owner provides, in
  order: [Owner setup](docs/owner-setup.md).

## Update and restore

- **Upgrading** is a migrate-only step with the new image, then a roll
  forward of the API and the workers. Versioning, the compatibility matrix,
  rollback rules and how to verify an image's signature:
  [Releasing and upgrading](docs/releasing.md).
- **Backups** are PostgreSQL's: point-in-time recovery, recovery objectives
  the owner chooses, and a monthly restore drill with a script that checks the
  restored books add up: [Backup and restore](docs/backup-and-restore.md).

## Security model

- **Exact integer money.** Fiat minor units and 256-bit token units, decimal
  strings on the wire, floating point forbidden by the linter.
- **Independent blockchain evidence.** A provider's answer is an observation;
  a payment becomes a fact only when independent providers and the gateway's
  own re-read agree.
- **Idempotent API, signed webhooks.** Every write carries an idempotency key;
  every event is signed with a per-endpoint secret derived from a master key
  the database never sees.
- **Least privilege.** Each process has its own PostgreSQL role, and row level
  security binds every observer to the chain source it speaks for.
- **Fail closed.** Missing or stale evidence refuses a quote; a process whose
  database disagrees with its configuration refuses to start; money that does
  not add up closes the rail until a person clears it.

Read the [Threat model](docs/threat-model.md), the
[Money invariants](docs/money-invariants.md) with the tests that exercise each
one, and the [Architecture](docs/architecture.md). Report a vulnerability
privately as described in [`SECURITY.md`](SECURITY.md).

## License

Apache License 2.0. Source, issues and releases on
[GitHub](https://github.com/Sskutushev/crypto-gateway-project).
