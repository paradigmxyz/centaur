# syntax=docker/dockerfile:1
# Build context is the repo root (see Justfile `_build-api-rs`) so the runtime
# stage can bake the shared `tools/` tree used for iron-proxy fragment discovery.

FROM rust:1-bookworm AS builder
# aws-lc-rs (the rustls crypto provider) builds native code, so the builder
# needs cmake + a C toolchain. No DATABASE_URL is required at build time:
# centaur-session-sqlx only uses sqlx::migrate! (compile-time file embedding),
# not the query-checking macros.
RUN apt-get update && apt-get install -y --no-install-recommends \
        cmake perl build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY services/api-rs/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p centaur-api-server \
    && cp target/release/centaur-api-server /usr/local/bin/centaur-api-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/centaur-api-server /usr/local/bin/centaur-api-server
WORKDIR /app
# api-rs discovers tool secret metadata from pyproject.toml at startup so it can
# register the full tool secret set with iron-control before sandboxes claim.
COPY tools/ /app/tools/
# Infra + harness auth secrets are embedded in the binary at compile time
# (include_str! of the in-crate infra.yaml); tool secrets are discovered from
# /app/tools plus any mounted overlay tools.
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-api-server"]
