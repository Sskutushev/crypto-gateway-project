# Crypto Gateway Project

An open-source, non-custodial gateway for accepting and verifying inbound
stablecoin payments.

The project is being built around four properties:

- exact integer accounting;
- independent blockchain evidence;
- idempotent merchant APIs and signed webhooks;
- a permanent, reconcilable audit trail.

The gateway never stores treasury private keys and does not implement
withdrawals or customer crypto balances.

## Status

Foundation work is in progress. No network rail is production-ready yet. See
[`docs/implementation-status.md`](docs/implementation-status.md) for the live
handoff and [`docs/architecture.md`](docs/architecture.md) for the target
design.

## Planned first release

The first production rail is USDT on TRON. Ethereum USDT and TON USDT follow
only after the generic observer/verifier interface is proven by the first
rail.

## Development

Prerequisites:

- Rust stable (pinned in `rust-toolchain.toml`)
- Docker with Compose
- PostgreSQL 16+

The first executable vertical slice exposes authenticated create/read payment
intent endpoints with merchant-scoped idempotency. It is still a development
foundation: no blockchain rail is production-ready.

Start the local stack with `docker compose up --build`. Compose binds the API
and PostgreSQL only to the host loopback interface. The API serves plain HTTP
for local development; any non-local deployment must terminate TLS before the
API and must not expose PostgreSQL publicly. Production database credentials
must be unique, least-privileged, and supplied outside this repository.

Core checks for a local Rust toolchain are:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

The PostgreSQL-backed API scenario is intentionally ignored by the default
test command. After starting the Compose PostgreSQL service, run it explicitly
with `GATEWAY_TEST_DATABASE_URL` set to the disposable database and pass
`-- --ignored` to `cargo test -p gateway-http`.

## License

Licensed under the Apache License, Version 2.0.
