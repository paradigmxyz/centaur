# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS builder
ARG SCCACHE_VERSION=0.19.0
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) archive_arch=x86_64; checksum=aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd ;; \
      arm64) archive_arch=aarch64; checksum=3e47fc4a71b1ddd1b4294a0de9d34498922fdf41e3cd0bda5df3954e3d6deb9d ;; \
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
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,id=company-context-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=company-context-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=company-context-sccache-${TARGETARCH},target=/var/cache/sccache,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release --locked -p centaur-company-context && cp target/release/centaur-company-context /usr/local/bin/centaur-company-context ;; \
      debug|dev) cargo build --locked -p centaur-company-context && cp target/debug/centaur-company-context /usr/local/bin/centaur-company-context ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac

# Debian's pandoc predates the pptx and xlsx readers. The release binary is
# static, so it is fetched on the build platform for the target architecture.
FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS pandoc
ARG PANDOC_VERSION=3.13
ARG TARGETARCH
RUN --mount=type=cache,id=company-context-apt-cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,id=company-context-apt-lib,target=/var/lib/apt,sharing=locked \
    printf '%s\n' "Acquire::Retries \"5\";" "Acquire::http::Timeout \"30\";" "Acquire::https::Timeout \"30\";" > /etc/apt/apt.conf.d/80-network-retries \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
RUN case "$TARGETARCH" in \
      amd64) checksum=38aa33b325a2bc96a4c39409631212db55702f564250bcdbead180b3bf646389 ;; \
      arm64) checksum=21e1d4ba09bd0b334590d204580fd4f6629b21be4ab6a32b26cc98982e9e0c68 ;; \
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
RUN --mount=type=cache,id=company-context-apt-cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,id=company-context-apt-lib,target=/var/lib/apt,sharing=locked \
    printf '%s\n' "Acquire::Retries \"5\";" "Acquire::http::Timeout \"30\";" "Acquire::https::Timeout \"30\";" > /etc/apt/apt.conf.d/80-network-retries \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=pandoc /usr/local/bin/pandoc /usr/local/bin/pandoc
COPY --from=builder /usr/local/bin/centaur-company-context /usr/local/bin/centaur-company-context
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-company-context"]
