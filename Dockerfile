# syntax=docker/dockerfile:1.7
# Pinned by digest: a tag can be re-pointed, the digest is the image. Renovate
# moves the digest in its own pull request.
FROM rust:1.90.0-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f AS builder
WORKDIR /source
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/source/target,sharing=locked \
    cargo build --locked --release --bin gateway-api --bin gateway-worker \
    && cp /source/target/release/gateway-api /tmp/gateway-api \
    && cp /source/target/release/gateway-worker /tmp/gateway-worker

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime
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
