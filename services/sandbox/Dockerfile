# syntax=docker/dockerfile:1.7

FROM ubuntu:24.04 AS toolchain

ENV PYTHONDONTWRITEBYTECODE=1

# ── System packages (cached unless base image changes) ───────────────────────
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
    git curl wget ca-certificates gnupg lsb-release sudo \
    build-essential pkg-config cmake ninja-build \
    ffmpeg ghostscript dvisvgm file \
    libssl-dev libclang-dev protobuf-compiler \
    libcairo2-dev libpango1.0-dev \
    ripgrep fd-find jq tree tmux unzip xz-utils gettext-base vim \
    postgresql-client libpq-dev \
    python3 python3-pip python3-venv python3-dev \
    texlive texlive-latex-base texlive-latex-extra \
    texlive-latex-recommended texlive-fonts-recommended \
    texlive-plain-generic texlive-science tipa cm-super \
    procps htop \
    && ln -sf /usr/bin/fdfind /usr/local/bin/fd \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info

# ── Python deps ──────────────────────────────────────────────────────────────
RUN --mount=type=cache,target=/root/.cache/pip,sharing=locked \
    pip3 install --break-system-packages --no-compile \
    python-docx==1.2.0 lxml==6.0.2 docx-revisions==0.1.4 \
    matplotlib==3.10.8 numpy==2.4.4 pandas==3.0.2 \
    python-pptx==1.0.2 openpyxl==3.1.5 PyMuPDF==1.27.2 mammoth==1.12.0 \
    faster-whisper \
    asyncpg==0.30.0 \
    boto3>=1.40.0 \
    google-api-python-client>=2.100.0 \
    google-auth-httplib2>=0.2.0 \
    google-auth-oauthlib>=1.2.0 \
    feedparser>=6.0.0 \
    httplib2>=0.20.0 \
    httpx>=0.28.0 \
    opentelemetry-proto==1.42.1 \
    pysocks>=1.7.1 \
    rich>=13.0.0 \
    slack-sdk==3.39.0 \
    tomli-w==1.2.0 \
    && find /usr/local/lib/python3.12 /usr/lib/python3 -type d -name __pycache__ -prune -exec rm -rf '{}' +

# ── GitHub CLI + Node.js 24 (LTS) + Docker CLI (single layer, cached) ─────────────
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
    && curl -fsSL https://deb.nodesource.com/setup_24.x | bash - \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
      | gpg --dearmor -o /usr/share/keyrings/docker-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/docker-archive-keyring.gpg] https://download.docker.com/linux/ubuntu $(lsb_release -cs) stable" \
      | tee /etc/apt/sources.list.d/docker.list > /dev/null \
    && apt-get update && apt-get install -y --no-install-recommends gh nodejs docker-ce-cli \
    && rm -rf /usr/share/doc /usr/share/man /usr/share/info

# ── Non-root agent user ─────────────────────────────────────────────────────
RUN useradd -m -s /bin/bash -u 1001 agent \
    && echo "agent ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers.d/agent

ARG CLAUDE_CODE_VERSION=2.1.154
ARG CODEX_VERSION=0.139.0
ARG PI_CODING_AGENT_VERSION=0.67.2
ARG NUSHELL_VERSION=0.112.2
ARG KUBECTL_VERSION=v1.34.2
ARG KACHE_VERSION=v0.7.0
# Bootstrap pnpm for agents working in JS monorepos. The exact baked version
# doesn't constrain repos: when a repo's `packageManager` field declares a
# different version, pnpm downloads and runs that one (v11 `pmOnFail=download`
# default; `manage-package-manager-versions` in v10) over the same registry
# egress a dependency install already needs.
ARG PNPM_VERSION=11.5.3

# ── kubectl ──────────────────────────────────────────────────────────────────
RUN set -eux; \
    kubectl_arch="$(case "$(dpkg --print-architecture)" in \
      amd64) echo amd64 ;; \
      arm64) echo arm64 ;; \
      *) echo unsupported-architecture >&2; exit 1 ;; \
    esac)"; \
    curl -fsSL \
      "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${kubectl_arch}/kubectl" \
      -o /usr/local/bin/kubectl; \
    chmod +x /usr/local/bin/kubectl; \
    kubectl version --client=true | grep -F "${KUBECTL_VERSION#v}"

# ── AWS CLI v2 ─────────────────────────────────────────────────────────────
RUN set -eux; \
    aws_arch="$(case "$(dpkg --print-architecture)" in \
      amd64) echo x86_64 ;; \
      arm64) echo aarch64 ;; \
      *) echo unsupported-architecture >&2; exit 1 ;; \
    esac)"; \
    curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-${aws_arch}.zip" -o /tmp/awscliv2.zip; \
    unzip -q /tmp/awscliv2.zip -d /tmp; \
    /tmp/aws/install; \
    rm -rf /tmp/awscliv2.zip /tmp/aws; \
    aws --version

# ── Nushell ─────────────────────────────────────────────────────────────────
RUN set -eux; \
    nu_arch="$(case "$(dpkg --print-architecture)" in \
      amd64) echo x86_64 ;; \
      arm64) echo aarch64 ;; \
      *) echo unsupported-architecture >&2; exit 1 ;; \
    esac)"; \
    curl -fsSL \
      "https://github.com/nushell/nushell/releases/download/${NUSHELL_VERSION}/nu-${NUSHELL_VERSION}-${nu_arch}-unknown-linux-gnu.tar.gz" \
      -o /tmp/nu.tar.gz; \
    mkdir -p /tmp/nu; \
    tar -xzf /tmp/nu.tar.gz -C /tmp/nu --strip-components=1; \
    install -m 0755 /tmp/nu/nu /usr/local/bin/nu; \
    rm -rf /tmp/nu /tmp/nu.tar.gz; \
    nu --version | grep -F "${NUSHELL_VERSION}"

# ── Global npm CLIs (root) ──────────────────────────────────────────────────
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    npm install -g --prefer-online --force \
      "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
      "@openai/codex@${CODEX_VERSION}" \
      "@mariozechner/pi-coding-agent@${PI_CODING_AGENT_VERSION}" \
      "pnpm@${PNPM_VERSION}" \
      agent-browser@0.26.0 \
    && find /usr/lib/node_modules -type f \( -name '*.map' -o -name '*.tsbuildinfo' \) -delete \
    && claude --version | grep -F "${CLAUDE_CODE_VERSION}" \
    && codex --version | grep -F "${CODEX_VERSION}" \
    && pnpm --version | grep -F "${PNPM_VERSION}"

# ── agent-browser system deps (must run as root before USER agent) ───────────
# Chrome for Testing doesn't provide Linux ARM64 builds; fall back to system chromium
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    if [ "$(dpkg --print-architecture)" = "amd64" ]; then \
      apt-get update && apt-get install -y --no-install-recommends \
        libxcb-shm0 libx11-xcb1 libx11-6 libxcb1 libxext6 libxrandr2 \
        libxcomposite1 libxcursor1 libxdamage1 libxfixes3 libxi6 \
        libgtk-3-0t64 libpangocairo-1.0-0 libpango-1.0-0 libatk1.0-0t64 \
        libcairo-gobject2 libcairo2 libgdk-pixbuf-2.0-0 libxrender1 \
        libasound2t64 libfreetype6 libfontconfig1 libdbus-1-3 libnss3 \
        libnspr4 libatk-bridge2.0-0t64 libdrm2 libxkbcommon0 \
        libatspi2.0-0t64 libcups2t64 libxshmfence1 libgbm1 \
        fonts-noto-color-emoji fonts-noto-cjk fonts-freefont-ttf; \
    else \
      echo "Skipping browser install: Chrome for Testing does not publish Linux ARM64 builds"; \
    fi

USER agent
WORKDIR /home/agent

ENV PATH="/home/agent/.cargo/bin:/home/agent/.local/bin:/home/agent/.foundry/bin:/home/agent/.bun/bin:${PATH}"

ARG FOUNDRY_VERSION=v1.7.0
ARG AMP_CLI_VERSION=0.0.1774529752-g94f44d
ARG MANIM_VERSION=0.20.1
ARG BUN_VERSION=bun-v1.3.13

# ── Agent-scoped toolchains ─────────────────────────────────────────────────
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --component rustfmt --component clippy \
    && rustc --version \
    && cargo --version \
    && rustfmt --version \
    && clippy-driver --version \
    && rm -rf "$HOME/.rustup/toolchains"/*/share/doc "$HOME/.rustup/toolchains"/*/share/man
RUN --mount=type=cache,target=/home/agent/.cargo/registry,uid=1001,gid=1001,sharing=locked \
    --mount=type=cache,target=/home/agent/.cargo/git,uid=1001,gid=1001,sharing=locked \
    --mount=type=cache,target=/home/agent/.cache/kache-install-target,uid=1001,gid=1001,sharing=locked \
    CARGO_TARGET_DIR=/home/agent/.cache/kache-install-target \
    cargo install --git https://github.com/kunobi-ninja/kache --tag "${KACHE_VERSION}" --locked kache \
    && kache --version
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
RUN curl -fsSL https://bun.sh/install | bash -s "${BUN_VERSION}"
RUN --mount=type=cache,target=/home/agent/.cache/uv,uid=1001,gid=1001,sharing=locked \
    uv tool install "manim==${MANIM_VERSION}" \
    && manim --version
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /home/agent/.foundry/bin/foundryup --install ${FOUNDRY_VERSION} \
    && for bin in forge cast anvil chisel; do \
      rm -f "$HOME/.foundry/bin/$bin"; \
      ln -s "$HOME/.foundry/versions/${FOUNDRY_VERSION}/$bin" "$HOME/.foundry/bin/$bin"; \
    done
RUN set -eux; \
    amp_platform="$(case "$(dpkg --print-architecture)" in \
      amd64) echo linux-x64 ;; \
      arm64) echo linux-arm64 ;; \
      *) echo unsupported-architecture >&2; exit 1 ;; \
    esac)"; \
    mkdir -p "$HOME/.amp/bin" "$HOME/.local/bin"; \
    curl -fsSL "https://static.ampcode.com/cli/${AMP_CLI_VERSION}/${amp_platform}-amp.sha256" -o /tmp/amp.sha256; \
    curl -fsSL "https://static.ampcode.com/cli/${AMP_CLI_VERSION}/amp-${amp_platform}" -o "$HOME/.amp/bin/amp"; \
    echo "$(cat /tmp/amp.sha256)  $HOME/.amp/bin/amp" | sha256sum -c -; \
    chmod +x "$HOME/.amp/bin/amp"; \
    ln -sf "$HOME/.amp/bin/amp" "$HOME/.local/bin/amp"; \
    rm -f /tmp/amp.sha256; \
    amp --version >/dev/null

# ── agent-browser: download Chrome binary (runs as agent, deps already installed) ─
RUN if [ "$(dpkg --print-architecture)" = "amd64" ]; then \
      agent-browser install; \
    fi

# ── Agent skills + workspace setup (rarely changes) ─────────────────────────
RUN --mount=type=cache,target=/home/agent/.npm,uid=1001,gid=1001,sharing=locked \
    npx -y skills add vercel-labs/agent-browser
RUN mkdir -p ~/.amp/bin ~/.config/amp ~/.codex ~/.pi/agent ~/github ~/workspace \
    && git config --global init.defaultBranch main \
    && git config --global push.autoSetupRemote true \
    && git config --global --add safe.directory '*' \
    && git config --global user.name "Centaur AI" \
    && git config --global user.email "ai@centaur.local"

# ==============================================================================
# Final thin stage: only frequently-changing local files below.
# Changing harness_session.py / SYSTEM_PROMPT.md / entrypoint.sh rebuilds only
# the COPY layers — the entire toolchain stage stays cached.
# ==============================================================================

FROM toolchain AS sandbox

USER root
RUN rm -f /etc/sudoers.d/agent
# BuildKit's COPY --link creates parent directories without an explicit mode,
# which can leave /etc/centaur without the execute bit for non-root users.
# Pre-create it as 0755 so the agent user can traverse into it at runtime.
RUN install -d -m 0755 /etc/centaur /opt/centaur
COPY --link centaur_sdk/ /opt/centaur/centaur_sdk/
COPY --link --chown=1001:1001 tools/ /opt/centaur/tools/
COPY --link --chmod=644 services/sandbox/codex-auth.json /etc/centaur/codex-auth.default.json
COPY --link --chmod=644 services/sandbox/claude-credentials.json /etc/centaur/claude-credentials.default.json
COPY --link --chmod=755 services/sandbox/md2docx.py /usr/local/bin/md2docx
COPY --link --chmod=755 services/workflow-python/workflow_host.py /usr/local/bin/workflow-host
COPY --link services/workflow-python/api/ /usr/local/bin/api/
COPY --link --chmod=755 services/sandbox/git-branch.sh /usr/local/bin/git-branch
COPY --link --chmod=755 services/sandbox/install_tool_shims.py /usr/local/bin/install-tool-shims
COPY --link --chmod=755 services/sandbox/centaur_tool_host.py /usr/local/bin/centaur-tool-host
COPY --link --chmod=755 services/sandbox/repo_cache_sync.py /usr/local/bin/repo-cache-sync
COPY --link --chmod=755 services/sandbox/repo_cache_watch.py /usr/local/bin/repo-cache-watch
COPY --link --chmod=755 services/sandbox/entrypoint.sh /entrypoint.sh

USER agent
WORKDIR /home/agent

# Build from a bind mount instead of COPY: COPY --link into /tmp rebuilds the
# parent directory chain in its own layer, clobbering /tmp's root:root 1777
# sticky metadata with agent:agent 0755, which broke /tmp writes for any
# non-1001 user (e.g. the root-with-dropped-caps repo-cache). A bind mount
# leaves no trace in the image and caches on source content just like COPY.
RUN --mount=type=bind,source=crates/harness-server,target=/harness-server-src \
    --mount=type=cache,target=/home/agent/.cargo/registry,uid=1001,gid=1001,sharing=locked \
    --mount=type=cache,target=/home/agent/.cargo/git,uid=1001,gid=1001,sharing=locked \
    --mount=type=cache,target=/home/agent/.cache/harness-server-target,uid=1001,gid=1001,sharing=locked \
    CARGO_TARGET_DIR=/home/agent/.cache/harness-server-target \
    cargo install --locked --path /harness-server-src --root /home/agent/.local

# No RUN instructions after these — all repo-local content at the very end.
COPY --link --chown=1001:1001 .agents/skills/ /home/agent/.agents/skills/
COPY --link --chown=1001:1001 harness/ /home/agent/harness/
COPY --link --chown=1001:1001 workflows/ /opt/centaur/workflows/
COPY --link --chown=1001:1001 services/sandbox/SYSTEM_PROMPT.md /home/agent/AGENTS.md

ENTRYPOINT ["/entrypoint.sh"]
# Default CMD uses Rust blocks mode; pass --mode=jsonrpc for raw App Server JSON-RPC.
CMD ["harness-server", "codex"]
