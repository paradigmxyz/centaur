# syntax=docker/dockerfile:1.7
# Build context is the repo root (see Justfile `_build-api-rs`) so the runtime
# stage can bake the shared `tools/` tree used for iron-proxy fragment discovery.

FROM rust:1-bookworm AS builder
# aws-lc-rs (the rustls crypto provider) builds native code, so the builder
# needs cmake + a C toolchain. No DATABASE_URL is required at build time:
# centaur-session-sqlx only uses sqlx::migrate! (compile-time file embedding),
# not the query-checking macros.
RUN --mount=type=cache,id=api-rs-apt-cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,id=api-rs-apt-lib,target=/var/lib/apt,sharing=locked \
    printf '%s\n' "Acquire::Retries \"5\";" "Acquire::http::Timeout \"30\";" "Acquire::https::Timeout \"30\";" > /etc/apt/apt.conf.d/80-network-retries \
    && apt-get update && apt-get install -y --no-install-recommends \
        cmake perl build-essential pkg-config
ARG SCCACHE_VERSION=0.19.0
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) archive_arch=x86_64; checksum=aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd ;; \
      arm64) archive_arch=aarch64; checksum=3e47fc4a71b1ddd1b4294a0de9d34498922fdf41e3cd0bda5df3954e3d6deb9d ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 2 ;; \
    esac && \
    archive="sccache-v${SCCACHE_VERSION}-${archive_arch}-unknown-linux-musl.tar.gz" && \
    curl --fail --location --silent --show-error \
      --output "/tmp/${archive}" \
      "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${archive}" && \
    echo "${checksum}  /tmp/${archive}" | sha256sum --check - && \
    tar --extract --gzip --file "/tmp/${archive}" --strip-components=1 \
      --directory /usr/local/bin "sccache-v${SCCACHE_VERSION}-${archive_arch}-unknown-linux-musl/sccache" && \
    rm "/tmp/${archive}" && \
    sccache --version
ENV CARGO_INCREMENTAL=0 \
    RUSTC_WRAPPER=/usr/local/bin/sccache \
    SCCACHE_DIR=/var/cache/sccache
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# Embedded by centaur-sandbox-agent-k8s, which installs them from a chart hook.
COPY contrib/chart/charts/agent-sandbox/crds/ contrib/chart/charts/agent-sandbox/crds/
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,id=api-rs-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=api-rs-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=api-rs-sccache-${TARGETARCH},target=/var/cache/sccache,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release --locked -p centaur-api-server && cp target/release/centaur-api-server /usr/local/bin/centaur-api-server ;; \
      debug|dev) cargo build --locked -p centaur-api-server && cp target/debug/centaur-api-server /usr/local/bin/centaur-api-server ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac && \
    sccache --show-stats

FROM debian:bookworm-slim
RUN --mount=type=cache,id=api-rs-apt-cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,id=api-rs-apt-lib,target=/var/lib/apt,sharing=locked \
    printf '%s\n' "Acquire::Retries \"5\";" "Acquire::http::Timeout \"30\";" "Acquire::https::Timeout \"30\";" > /etc/apt/apt.conf.d/80-network-retries \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git python3 python3-pip
COPY --from=builder /usr/local/bin/centaur-api-server /usr/local/bin/centaur-api-server
WORKDIR /app
COPY centaur_sdk/ /app/centaur_sdk/
COPY services/workflow-python/ /app/workflow-python/
RUN --mount=type=cache,target=/root/.cache/pip,sharing=locked \
    pip3 install --break-system-packages --no-compile --ignore-installed "packaging>=24.2.0" \
    && pip3 install --break-system-packages --no-compile /app/centaur_sdk /app/workflow-python \
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
