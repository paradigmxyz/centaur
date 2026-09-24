# syntax=docker/dockerfile:1.7
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY services/proxy-sync/ ./
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release && cp target/release/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync ;; \
      debug|dev) cargo build && cp target/debug/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac

FROM debian:bookworm-slim
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/centaur-proxy-sync /usr/local/bin/centaur-proxy-sync
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/centaur-proxy-sync"]
