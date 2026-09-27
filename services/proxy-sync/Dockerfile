# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS builder
ARG SCCACHE_VERSION=0.18.0
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) archive_arch=x86_64; checksum=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89 ;; \
      arm64) archive_arch=aarch64; checksum=2b3284d5da3b46a47dc4229e75bb7b88ac4aa99c8d754fb7d2f84997e5a4354a ;; \
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
COPY crates/active-record-encryption/ crates/active-record-encryption/
COPY services/proxy-sync/ services/proxy-sync/
WORKDIR /build/services/proxy-sync
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,id=proxy-sync-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=proxy-sync-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=proxy-sync-sccache-${TARGETARCH},target=/var/cache/sccache,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release && cp target/release/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync ;; \
      debug|dev) cargo build && cp target/debug/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac && \
    sccache --show-stats

FROM debian:bookworm-slim
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-proxy-sync"]
