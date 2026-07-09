# syntax=docker/dockerfile:1.7
# Build context is the repo root (see Justfile `_build-api-rs`) so the runtime
# stage can bake the shared `tools/` tree used for iron-proxy fragment discovery.

FROM rust:1-bookworm AS builder
# aws-lc-rs (the rustls crypto provider) builds native code, so the builder
# needs cmake + a C toolchain. No DATABASE_URL is required at build time:
# centaur-session-sqlx only uses sqlx::migrate! (compile-time file embedding),
# not the query-checking macros.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
        cmake perl build-essential pkg-config
WORKDIR /build
COPY services/api-rs/ ./
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release -p centaur-api-server && cp target/release/centaur-api-server /usr/local/bin/centaur-api-server ;; \
      debug|dev) cargo build -p centaur-api-server && cp target/debug/centaur-api-server /usr/local/bin/centaur-api-server ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac

FROM debian:bookworm-slim
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates curl python3 python3-pip
COPY --from=builder /usr/local/bin/centaur-api-server /usr/local/bin/centaur-api-server
WORKDIR /app
COPY centaur_sdk/ /app/centaur_sdk/
COPY services/workflow-python/ /app/workflow-python/
RUN --mount=type=cache,target=/root/.cache/pip,sharing=locked \
    pip3 install --break-system-packages --no-compile /app/centaur_sdk /app/workflow-python \
    && python3 -c "import boto3, botocore" \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info
# api-rs discovers tool secret metadata from pyproject.toml at startup so it can
# register the full tool secret set with iron-control before sandboxes claim.
COPY tools/ /app/tools/
COPY workflows/ /app/workflows/
ENV PYTHON_WORKFLOW_HOST_PATH=/app/workflow-python/workflow_host.py
ENV WORKFLOW_DIRS=/app/workflows
# Infra + harness auth secrets are embedded in the binary at compile time
# (include_str! of the in-crate infra.yaml); tool secrets are discovered from
# /app/tools plus any mounted overlay tools.
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-api-server"]
