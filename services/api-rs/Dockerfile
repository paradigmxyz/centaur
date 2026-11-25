# syntax=docker/dockerfile:1.7
# Context is the repo root (see Justfile `_build-api-rs`) so the runtime stage can
# bake the shared `tools/` tree used for iron-proxy fragment and secret discovery.

# Build the Console SPA: the shell and hashed assets into
# services/console/public/spa. The runtime image serves both from disk; api-rs
# embeds no web bundle.
FROM docker.io/library/node:24-slim AS web
WORKDIR /repo
RUN npm install -g pnpm@10.34.9
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY patches/ patches/
COPY services/console/web/package.json services/console/web/package.json
RUN --mount=type=cache,target=/root/.local/share/pnpm/store,sharing=locked \
    pnpm install --filter @centaur/console-web --frozen-lockfile
COPY services/console/web/ services/console/web/
RUN pnpm -C services/console/web build \
    && find services/console/public/spa/assets -type f -name '*.map' -delete \
    && test -f services/console/public/spa/index.html

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
# Embedded with include_bytes!/include_str! by the Rust Console: the
# server-rendered pages' stylesheet/font/logo. The built SPA assets and shell are
# copied from the web stage below.
COPY services/console/public/assets/oauth-page/ services/console/public/assets/oauth-page/
COPY services/console/public/icon.svg services/console/public/icon.svg
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
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info
COPY --from=builder /usr/local/bin/centaur-api-server /usr/local/bin/centaur-api-server
WORKDIR /app
# api-rs discovers tool secret metadata from pyproject.toml at startup so it can
# register the full tool secret set with iron-control before sandboxes claim.
COPY tools/ /app/tools/
# No Python runs here: the workflow discovery service and workflow-host
# sandboxes run workflow code from the agent image.
# The built Console web app and the root public files its shell fetches. Source
# maps are removed in the web stage. Only the Console shell route serves
# index.html, with the CSP; /spa/assets serves the assets directory alone.
COPY --from=web /repo/services/console/public/spa/ /app/services/console/public/spa/
COPY services/console/public/icon-dark.svg \
     services/console/public/icon-light.svg \
     services/console/public/robots.txt \
     /app/services/console/public/
# The Console static paths default to a checkout; the image points them at the
# baked copies above. Override any of them through apiRs.extraEnv when needed.
ENV CENTAUR_CONSOLE_SPA_SHELL=/app/services/console/public/spa/index.html
ENV CENTAUR_CONSOLE_SPA_ASSETS=/app/services/console/public/spa/assets
ENV CENTAUR_CONSOLE_PUBLIC_ROOT=/app/services/console/public
# Infra + harness auth secrets are embedded in the binary at compile time
# (include_str! of the in-crate infra.yaml); tool secrets are discovered from
# /app/tools plus any mounted overlay tools.
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-api-server"]
