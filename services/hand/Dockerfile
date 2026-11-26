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
ARG RUST_BUILD_PROFILE=release
RUN --mount=type=cache,id=hand-cargo-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=hand-cargo-git-${TARGETARCH},target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=hand-sccache-${TARGETARCH},target=/var/cache/sccache,sharing=locked \
    case "$RUST_BUILD_PROFILE" in \
      release) cargo build --release --locked -p centaur-hand && cp target/release/centaur-hand /usr/local/bin/centaur-hand ;; \
      debug|dev) cargo build --locked -p centaur-hand && cp target/debug/centaur-hand /usr/local/bin/centaur-hand ;; \
      *) echo "unsupported RUST_BUILD_PROFILE=$RUST_BUILD_PROFILE" >&2; exit 2 ;; \
    esac && \
    sccache --show-stats

FROM ubuntu:24.04
ARG DEBIAN_FRONTEND=noninteractive
RUN --mount=type=cache,id=hand-apt-cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,id=hand-apt-lib,target=/var/lib/apt,sharing=locked \
    printf '%s\n' "Acquire::Retries \"5\";" "Acquire::http::Timeout \"30\";" "Acquire::https::Timeout \"30\";" > /etc/apt/apt.conf.d/80-network-retries \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        bash ca-certificates curl git jq less procps ripgrep tini unzip xz-utils \
    && curl --fail --location --silent --show-error \
        --output /usr/share/keyrings/githubcli-archive-keyring.gpg \
        https://cli.github.com/packages/githubcli-archive-keyring.gpg \
    && chmod 0644 /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/* /usr/share/doc /usr/share/man /usr/share/info

RUN groupadd --gid 1001 agent \
    && useradd --uid 1001 --gid 1001 --create-home --home-dir /home/agent --shell /bin/bash agent \
    && install --directory --owner=agent --group=agent \
        /home/agent/workspace /home/agent/.centaur /etc/centaur-hand

# Every shell sees the lease's placeholder environment, and Git and gh present
# those placeholders; the sidecar proxy replaces them on the wire. Git is forced
# to HTTPS so a clone never tries a key the hand does not have.
RUN printf '%s\n' \
    '[url "https://github.com/"]' \
    '    insteadOf = git@github.com:' \
    '    insteadOf = ssh://git@github.com/' \
    '[credential "https://github.com"]' \
    '    helper = !gh auth git-credential' \
    > /etc/gitconfig \
    && printf '%s\n' \
    '# Source the hand lease environment for login shells.' \
    'if [ -n "${HOME:-}" ] && [ -r "$HOME/.centaur/env.sh" ]; then' \
    '    . "$HOME/.centaur/env.sh"' \
    'fi' \
    > /etc/profile.d/centaur-env.sh

COPY --from=builder /usr/local/bin/centaur-hand /usr/local/bin/centaur-hand
COPY --chmod=755 services/hand/entrypoint.sh /entrypoint.sh

ENV HOME=/home/agent \
    BASH_ENV=/home/agent/.centaur/env.sh
WORKDIR /home/agent/workspace
USER agent
EXPOSE 7070 7071
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
