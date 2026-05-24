#!/bin/bash
set -e

HOME_DIR="$(eval echo ~)"
FIREWALL_HOSTNAME="${FIREWALL_HOST:-firewall}"

mkdir -p "$HOME_DIR/.config/amp"

# ── Write harness configs (no MCP — adds ~10s startup overhead) ───────────────
cat > "$HOME_DIR/.config/amp/settings.json" <<EOF
{
  "amp.experimental.compaction": 95,
  "amp.proxy": "http://${FIREWALL_HOSTNAME}:8080",
  "amp.git.commit.coauthor.enabled": false
}
EOF

# ── Mock Google ADC for sandbox-only SDK initialization ─────────────────────
# Some Google client libraries refuse to initialize without ADC, even when the
# per-sandbox proxy is responsible for attaching the real auth headers.
if [ -z "${GOOGLE_APPLICATION_CREDENTIALS:-}" ]; then
    GOOGLE_APPLICATION_CREDENTIALS="$HOME_DIR/.config/gcloud/application_default_credentials.json"
    export GOOGLE_APPLICATION_CREDENTIALS
    mkdir -p "$(dirname "$GOOGLE_APPLICATION_CREDENTIALS")"
    if [ ! -f "$GOOGLE_APPLICATION_CREDENTIALS" ]; then
        # Some SDKs parse ADC into service-account credentials locally before any
        # outbound request reaches the proxy, so the stub must look real enough
        # to pass key loading.
        _mock_gcp_private_key="$(openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 2>/dev/null)"
        MOCK_GCP_PRIVATE_KEY="$_mock_gcp_private_key" python3 - "$GOOGLE_APPLICATION_CREDENTIALS" <<'PYEOF'
import json
import os
import sys

path = sys.argv[1]
client_email = "mock@creds.com"

with open(path, "w") as f:
    json.dump(
        {
            "type": "service_account",
            "project_id": "centaur-sandbox",
            "private_key_id": "0000000000000000000000000000000000000000",
            "private_key": os.environ["MOCK_GCP_PRIVATE_KEY"].rstrip("\n") + "\n",
            "client_email": client_email,
            "client_id": "100000000000000000000",
            "auth_uri": "https://accounts.google.com/o/oauth2/auth",
            "token_uri": "https://oauth2.googleapis.com/token",
            "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
            "client_x509_cert_url": f"https://www.googleapis.com/robot/v1/metadata/x509/{client_email.replace('@', '%40')}",
            "universe_domain": "googleapis.com",
        },
        f,
        indent=2,
    )
    f.write("\n")
PYEOF
        unset _mock_gcp_private_key
    fi
fi

# ── Codex settings ──────────────────────────────────────────────────────────
mkdir -p "$HOME_DIR/.codex"
if [ -n "${CENTAUR_TRACE_ID:-}" ]; then
    printf '%s' "$CENTAUR_TRACE_ID" > "$HOME_DIR/.trace_id"
fi

toml_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

HARNESS_CONFIG_DIR="${CENTAUR_HARNESS_CONFIG_DIR:-$HOME_DIR/harness}"
if [ -f "$HARNESS_CONFIG_DIR/codex/config.toml" ]; then
    cp "$HARNESS_CONFIG_DIR/codex/config.toml" "$HOME_DIR/.codex/config.toml"
else
    echo "missing Codex harness config: $HARNESS_CONFIG_DIR/codex/config.toml" >&2
    exit 1
fi

codex_laminar_trace_endpoint="${CODEX_OTEL_LAMINAR_ENDPOINT:-}"
if [ -z "$codex_laminar_trace_endpoint" ]; then
    codex_laminar_base="${CODEX_OTEL_LAMINAR_BASE_URL:-${LMNR_BASE_URL:-}}"
    if [ -n "$codex_laminar_base" ]; then
        codex_laminar_base="${codex_laminar_base%/}"
        case "$codex_laminar_base" in
            */v1/traces) codex_laminar_trace_endpoint="$codex_laminar_base" ;;
            *) codex_laminar_trace_endpoint="$codex_laminar_base/v1/traces" ;;
        esac
    fi
fi

if [ -n "$codex_laminar_trace_endpoint" ] && [ -n "${CENTAUR_TRACE_ID:-}" ]; then
    codex_otel_environment="${CODEX_OTEL_ENVIRONMENT:-${DEPLOY_ENV:-${ENVIRONMENT:-dev}}}"
    codex_otel_headers="\"x-trace-id\" = \"$(toml_escape "${CENTAUR_TRACE_ID:-}")\", \"x-centaur-thread-key\" = \"$(toml_escape "${CENTAUR_THREAD_KEY:-}")\""
    if [ -n "${LMNR_PROJECT_API_KEY:-}" ]; then
        codex_otel_headers="$codex_otel_headers, \"authorization\" = \"Bearer $(toml_escape "$LMNR_PROJECT_API_KEY")\""
    fi
    cat >> "$HOME_DIR/.codex/config.toml" <<EOF

[otel]
environment = "$(toml_escape "$codex_otel_environment")"
log_user_prompt = false
trace_exporter = { otlp-http = { endpoint = "$(toml_escape "$codex_laminar_trace_endpoint")", protocol = "binary", headers = { $codex_otel_headers } } }
EOF
fi

# ── Claude Code settings ────────────────────────────────────────────────────
mkdir -p "$HOME_DIR/.claude"
if [ -f "$HARNESS_CONFIG_DIR/claude/settings.json" ]; then
    cp "$HARNESS_CONFIG_DIR/claude/settings.json" "$HOME_DIR/.claude/settings.json"
fi

# ── Pi-mono settings ─────────────────────────────────────────────────────────
mkdir -p "$HOME_DIR/.pi/agent/extensions"
cat > "$HOME_DIR/.pi/agent/settings.json" <<EOF
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-20250514",
  "thinkingLevel": "medium",
  "autoCompaction": true
}
EOF

# ── Hermes settings ──────────────────────────────────────────────────────────
# Hermes resolves its provider/model from ~/.hermes/config.yaml.
# Default to Anthropic + Claude so it works out of the box through iron-proxy
# using the stubbed ANTHROPIC_API_KEY; HERMES_PROVIDER/HERMES_MODEL override
# fleet-wide or per-session (the model is set per spawn by the API).
mkdir -p "$HOME_DIR/.hermes"
HERMES_PROVIDER_VALUE="${HERMES_PROVIDER:-anthropic}"
HERMES_MODEL_VALUE="${HERMES_MODEL:-claude-sonnet-4-20250514}"
cat > "$HOME_DIR/.hermes/config.yaml" <<EOF
model:
  provider: "$(toml_escape "$HERMES_PROVIDER_VALUE")"
  default: "$(toml_escape "$HERMES_MODEL_VALUE")"
EOF

# ── Per-session workspace clone (no shared worktree metadata) ────────────────
WORKSPACE_DIR="$HOME_DIR/workspace"
if [ -n "${AGENT_REPO:-}" ]; then
    REPO_PATH="$HOME_DIR/github/$AGENT_REPO"
    if ! git -C "$REPO_PATH" rev-parse --git-dir >/dev/null 2>&1; then
        echo "AGENT_REPO is not a valid git repository: $REPO_PATH" >&2
        exit 1
    fi

    rm -rf "$WORKSPACE_DIR"
    if ! git clone --quiet --shared "$REPO_PATH" "$WORKSPACE_DIR"; then
        echo "shared clone failed for $REPO_PATH; retrying with regular clone" >&2
        rm -rf "$WORKSPACE_DIR"
        git clone --quiet "$REPO_PATH" "$WORKSPACE_DIR"
    fi

    BRANCH="agent-$(date +%s)-${RANDOM}-${RANDOM}"
    git -C "$WORKSPACE_DIR" checkout -q -b "$BRANCH" || true
else
    mkdir -p "$WORKSPACE_DIR"
fi

# ── Ensure uploads directory exists ──────────────────────────────────────────
mkdir -p "$HOME_DIR/uploads"

# ── Copy project skills into workspace (so `skill` tool discovers them) ──────
BAKED_IN_CENTAUR_SKILLS="$HOME_DIR/.agents/skills"
MOUNTED_CENTAUR_SKILLS="$HOME_DIR/centaur-skills"
MOUNTED_ORG_SKILLS="$HOME_DIR/centaur-overlay-skills"
OVERLAY_TREE_SKILLS=""
if [ -n "${CENTAUR_OVERLAY_DIR:-}" ] && [ -d "${CENTAUR_OVERLAY_DIR}/.agents/skills" ]; then
    OVERLAY_TREE_SKILLS="${CENTAUR_OVERLAY_DIR}/.agents/skills"
fi
CENTAUR_SKILLS=""
if [ -d "$HOME_DIR/github" ]; then
    CENTAUR_SKILLS="$(find "$HOME_DIR/github" -path '*/centaur/.agents/skills' -type d -print -quit 2>/dev/null || true)"
fi
WS_SKILLS="$WORKSPACE_DIR/.agents/skills"
for SKILLS_SRC in "$BAKED_IN_CENTAUR_SKILLS" "$MOUNTED_CENTAUR_SKILLS" "$CENTAUR_SKILLS" "$MOUNTED_ORG_SKILLS" "$OVERLAY_TREE_SKILLS"; do
    if [ -d "$SKILLS_SRC" ]; then
        mkdir -p "$WS_SKILLS"
        cp -r "$SKILLS_SRC"/. "$WS_SKILLS"/
    fi
done

if [ -d "$WS_SKILLS" ]; then
    mkdir -p "$WORKSPACE_DIR/.claude"
    rm -rf "$WORKSPACE_DIR/.claude/skills"
    ln -sf "$WS_SKILLS" "$WORKSPACE_DIR/.claude/skills"
fi

# ── Assemble system prompt from bind mounts ──────────────────────────────────
# Base prompt: mounted as AGENTS_BASE.md when present, fallback to baked-in AGENTS.md.
# Org/persona overlays are mounted alongside the base prompt when present.
TARGET_PROMPT="$HOME_DIR/workspace/AGENTS.md"
if [ -f "$HOME_DIR/AGENTS_BASE.md" ]; then
    cp "$HOME_DIR/AGENTS_BASE.md" "$TARGET_PROMPT"
elif [ -f "$HOME_DIR/AGENTS.md" ]; then
    cp "$HOME_DIR/AGENTS.md" "$TARGET_PROMPT"
fi

if [ -f "$HOME_DIR/AGENTS_OVERLAY.md" ] && [ -f "$TARGET_PROMPT" ]; then
    printf '\n\n---\n\n' >> "$TARGET_PROMPT"
    cat "$HOME_DIR/AGENTS_OVERLAY.md" >> "$TARGET_PROMPT"
fi

# Persona prompt injection is done by the API when it writes AGENTS_BASE.md.

# Switch to workspace so the harness reads workspace/AGENTS.md (with persona overlay)
cd "$WORKSPACE_DIR"

HARNESS_ADAPTER="${CENTAUR_HARNESS_ADAPTER:-/usr/local/bin/harness-adapter}"
if [ -x "$HARNESS_ADAPTER" ]; then
    "$HARNESS_ADAPTER" "${1:-}" "$TARGET_PROMPT"
fi

# Codex reads its auth file when the app server starts. Complete this before
# signaling readiness, otherwise warm pods can be claimed with no auth loaded.
CODEX_KEY="${CODEX_API_KEY:-${OPENAI_API_KEY:-}}"
if [ -n "$CODEX_KEY" ]; then
    echo "$CODEX_KEY" | codex login --with-api-key 2>/dev/null || true
fi

# Signal readiness
touch "$HOME_DIR/.ready"

# ── Background: slow auth tasks ─────────────────────────────────────────────
{
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        git config --global credential.helper store
        printf 'https://oauth2:%s@github.com\n' "$GITHUB_TOKEN" > "$HOME_DIR/.git-credentials"
        echo "${GITHUB_TOKEN}" | gh auth login --with-token 2>/dev/null || true
        gh auth setup-git 2>/dev/null || true
    fi
} &

exec "$@"
