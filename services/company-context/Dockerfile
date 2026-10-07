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
    curl --fail --location --silent --show-error --output "/tmp/${archive}" \
      "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${archive}" && \
    echo "${checksum}  /tmp/${archive}" | sha256sum --check - && \
    tar --extract --gzip --file "/tmp/${archive}" --strip-components=1 \
      --directory /usr/local/bin "sccache-v${SCCACHE_VERSION}-${archive_arch}-unknown-linux-musl/sccache" && \
    rm "/tmp/${archive}"
ENV CARGO_INCREMENTAL=0 \
    RUSTC_WRAPPER=/usr/local/bin/sccache \
    SCCACHE_DIR=/var/cache/sccache
WORKDIR /build
COPY crates/absurd-sdk/ crates/absurd-sdk/
COPY crates/active-record-encryption/ crates/active-record-encryption/
COPY services/company-context/ services/company-context/
WORKDIR /build/services/company-context
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,id=company-context-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=company-context-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=company-context-sccache-${TARGETARCH},target=/var/cache/sccache,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release --locked && cp target/release/centaur-company-context /usr/local/bin/centaur-company-context ;; \
      debug|dev) cargo build --locked && cp target/debug/centaur-company-context /usr/local/bin/centaur-company-context ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac

# Debian's pandoc predates the pptx and xlsx readers. The release binary is
# static, so it is fetched on the build platform for the target architecture.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS pandoc
ARG PANDOC_VERSION=3.12
ARG TARGETARCH
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
RUN case "$TARGETARCH" in \
      amd64) checksum=67d7d011fed8c8543306022b985b9b2499ab9b74818df91d8727c7e9ebc5ba06 ;; \
      arm64) checksum=6cefcf7100e23a99447c26f89d1ff5b253f3407fcef99a9e27ae06f3ed16cb82 ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 2 ;; \
    esac && \
    archive="pandoc-${PANDOC_VERSION}-linux-${TARGETARCH}.tar.gz" && \
    curl --fail --location --silent --show-error --output "/tmp/${archive}" \
      "https://github.com/jgm/pandoc/releases/download/${PANDOC_VERSION}/${archive}" && \
    echo "${checksum}  /tmp/${archive}" | sha256sum --check - && \
    tar --extract --gzip --file "/tmp/${archive}" --strip-components=2 \
      --directory /usr/local/bin "pandoc-${PANDOC_VERSION}/bin/pandoc" && \
    rm "/tmp/${archive}"

FROM debian:bookworm-slim
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=pandoc /usr/local/bin/pandoc /usr/local/bin/pandoc
COPY --from=builder /usr/local/bin/centaur-company-context /usr/local/bin/centaur-company-context
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-company-context"]
