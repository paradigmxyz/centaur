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
    --mount=type=cache,target=/build/target \
    cargo build --release -p centaur-api-server \
    && cp target/release/centaur-api-server /usr/local/bin/centaur-api-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/centaur-api-server /usr/local/bin/centaur-api-server
WORKDIR /app
# Iron-proxy config the server reads at runtime. centaur-iron-proxy resolves
# these by walking up from the working directory (fragment::repo_relative_path),
# so keep the repo-relative layout under /app:
#   services/iron-proxy/infra.yaml         (infra fragment)
#   services/iron-proxy/harness/*.yaml     (per-harness fragments)
#   services/api/api/iron-proxy.base.yaml  (proxy base config)
COPY services/iron-proxy/infra.yaml services/iron-proxy/infra.yaml
COPY services/iron-proxy/harness/ services/iron-proxy/harness/
COPY services/api/api/iron-proxy.base.yaml services/api/api/iron-proxy.base.yaml
# Tool metadata drives the per-sandbox iron-proxy egress allow-list.
COPY tools/ tools/
ENV TOOL_DIRS=/app/tools
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-api-server"]
