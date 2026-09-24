# syntax=docker/dockerfile:1.7
FROM rust:1.90.0-bookworm AS builder
WORKDIR /source
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/source/target,sharing=locked \
    cargo build --locked --release --bin gateway-api --bin gateway-worker \
    && cp /source/target/release/gateway-api /tmp/gateway-api \
    && cp /source/target/release/gateway-worker /tmp/gateway-worker

FROM debian:bookworm-slim AS runtime
# curl exists for the container healthcheck only; the binaries use rustls.
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --create-home --uid 10001 gateway
COPY --from=builder /tmp/gateway-api /usr/local/bin/gateway-api
# One image, two entry points: a deployment that runs background roles
# overrides the entrypoint with `gateway-worker`. One image means the API and
# every worker were built from the same commit, so a schema or contract change
# cannot ship to half of the system.
COPY --from=builder /tmp/gateway-worker /usr/local/bin/gateway-worker
USER gateway
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/gateway-api"]
