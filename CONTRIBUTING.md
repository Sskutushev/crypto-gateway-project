# Contributing

## Toolchain

Rust 1.90 is pinned in `rust-toolchain.toml`; `rustup` installs it on first
use. PostgreSQL 16 runs from `compose.yaml` on `127.0.0.1:54329`.

Without Rust on the host, the pinned image works as a workstation:

```sh
docker run -d --name gateway-dev -v "$PWD":/workspace rust:1.90.0-bookworm sleep infinity
docker exec gateway-dev bash -c "cd /workspace && cargo test --workspace --locked"
```

## The gates

Every change runs all of them before it is opened as a pull request; CI runs
them again.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
GATEWAY_TEST_DATABASE_URL=postgres://gateway:gateway@127.0.0.1:54329/gateway \
  cargo test --workspace --locked -- --ignored
cargo deny check
```

The PostgreSQL scenarios share one schema and serialise themselves; run them
against a database you can throw away. Migrations are unreleased and edited
in place; after changing one, reset the schema with
`DROP SCHEMA public CASCADE; CREATE SCHEMA public;`.

## The rules, in contributor terms

- **Money is an integer.** No `f32`/`f64` near an amount; clippy denies float
  arithmetic. On the wire, amounts are decimal strings.
- **A provider's answer is an observation.** Nothing an observer wrote is a
  fact the payment path may act on; only the verifier writes canonical rows.
- **Nothing settles from one source.** Do not lower the independence a
  finality or settlement policy demands to make a test pass.
- **Identity is bytes.** Tokens by contract bytes, recipients by address
  bytes, never by symbol or display string.
- **One transfer, one obligation.** Keep the claim table's primary key doing
  its job.
- **Every state change is explicit and audited.** New states are added to
  the check constraints and the state machine, never implied.
- **Side effects go through the outbox**, in the same transaction as the money.
- **No silent fallback.** A missing value is an error the caller sees; an
  empty `catch`, an `unwrap_or_default` on money, or a retry that hides a
  failure will not pass review. `unwrap`, `expect` and `panic!` are denied
  outside tests.
- **Secrets never enter the repository or the logs.** Configuration examples
  carry placeholders; `Debug` on a secret prints "redacted".
- **Write the lowest test that sees the break.** A unit test for a decision, a
  PostgreSQL scenario for a constraint or a transaction, a property test for
  a parser.
- **Comments explain why, in English.** Not what the next line does.

Adding a dependency is a decision: say in the pull request what it is for and
run `cargo deny check` with it.

## Pull requests

- One concern per pull request, with the gates green.
- The description says what changed and how it was verified; the template
  asks for both.
- Update `docs/implementation-status.md` when the change completes or starts
  a slice, and `CHANGELOG.md` under *Unreleased*.
- Commits are signed off under the Developer Certificate of Origin
  (`git commit -s`), which certifies you have the right to contribute the
  change under the Apache-2.0 licence.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md).
