#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# API container entrypoint — validate required infra env, then exec the command.
# ---------------------------------------------------------------------------

: "${DATABASE_URL:?DATABASE_URL is required}"
: "${SLACK_SIGNING_SECRET:?SLACK_SIGNING_SECRET is required}"
: "${SLACKBOT_API_KEY:?SLACKBOT_API_KEY is required}"

# Install dependencies from bind-mounted overlay tool directories.
# These aren't baked into the image — install at startup so tool loading doesn't fail.
if [[ -n "${TOOL_DIRS:-}" ]]; then
  IFS=':' read -ra _dirs <<< "$TOOL_DIRS"
  _extra_deps=""
  for _d in "${_dirs[@]}"; do
    [[ "$_d" == "/app/tools" ]] && continue  # already in image
    [[ -d "$_d" ]] || continue
    _extra_deps+=$(python3 -c "
import tomllib, pathlib
deps = set()
for p in pathlib.Path('$_d').glob('**/pyproject.toml'):
    deps.update(tomllib.load(open(p,'rb')).get('project',{}).get('dependencies',[]))
print('\n'.join(sorted(deps)))
" 2>/dev/null || true)
    _extra_deps+=$'\n'
  done
  if [[ -n "${_extra_deps}" ]]; then
    echo "$_extra_deps" | sort -u | grep -v '^$' > /tmp/_extra_deps.txt || true
    uv pip install -r /tmp/_extra_deps.txt --quiet 2>/dev/null || true
    rm -f /tmp/_extra_deps.txt
  fi
fi

# Bootstrap optional gcloud credentials for deployments that use gcloud-backed SSH tunneling.
if [[ "${CENTAUR_ENABLE_GCLOUD_BOOTSTRAP:-0}" =~ ^(1|true|yes)$ ]]; then
  : "${GCP_GCLOUD_CREDENTIAL:?GCP_GCLOUD_CREDENTIAL is required when CENTAUR_ENABLE_GCLOUD_BOOTSTRAP is enabled}"
  _gcp_cred="${GCP_GCLOUD_CREDENTIAL}"
  if [[ -n "$_gcp_cred" ]]; then
    _gcloud_dir="${HOME}/.config/gcloud"
    mkdir -p "$_gcloud_dir"
    echo "$_gcp_cred" > "$_gcloud_dir/application_default_credentials.json"
    _gcp_account=$(echo "$_gcp_cred" | python3 -c "import sys,json; print(json.load(sys.stdin).get('account',''))" 2>/dev/null || true)
    _gcp_project="${GCLOUD_PROJECT:-}"
    if [[ -z "$_gcp_project" ]]; then
      _gcp_project=$(echo "$_gcp_cred" | python3 -c "import sys,json; print(json.load(sys.stdin).get('project_id',''))" 2>/dev/null || true)
    fi
    if [[ -n "$_gcp_account" ]]; then
      python3 - "$_gcp_cred" "$_gcp_account" "$_gcloud_dir" <<'PYEOF'
import sqlite3, json, sys
cred_json, account, gcloud_dir = sys.argv[1], sys.argv[2], sys.argv[3]
cred = json.loads(cred_json)
cred.pop("account", None)
conn = sqlite3.connect(f"{gcloud_dir}/credentials.db")
conn.execute("CREATE TABLE IF NOT EXISTS credentials (account_id TEXT PRIMARY KEY, value TEXT)")
conn.execute("INSERT OR REPLACE INTO credentials VALUES (?, ?)", (account, json.dumps(cred)))
conn.commit()
conn.close()
PYEOF
      # Set active account + project when configured.
      gcloud config set core/account "$_gcp_account" --quiet 2>/dev/null || true
      if [[ -n "$_gcp_project" ]]; then
        gcloud config set core/project "$_gcp_project" --quiet 2>/dev/null || true
      fi
      echo "gcloud credentials bootstrapped for $_gcp_account" >&2
    fi
  fi
  unset _gcp_cred _gcp_account _gcp_project _gcloud_dir
fi

# Canonical env aliases
if [[ -z "${SLACK_BOT_TOKEN:-}" && -n "${SLACK_TOKEN:-}" ]]; then
  export SLACK_BOT_TOKEN="${SLACK_TOKEN}"
fi
if [[ -z "${GITHUB_TOKEN:-}" && -n "${GH_TOKEN:-}" ]]; then
  export GITHUB_TOKEN="${GH_TOKEN}"
fi
if [[ -z "${GITHUB_TOKEN:-}" && -n "${GITHUB_PAT:-}" ]]; then
  export GITHUB_TOKEN="${GITHUB_PAT}"
fi
if [[ -z "${ANTHROPIC_API_KEY:-}" && -n "${ANTHROPIC_KEY:-}" ]]; then
  export ANTHROPIC_API_KEY="${ANTHROPIC_KEY}"
fi
if [[ -z "${ANTHROPIC_API_KEY:-}" && -n "${CLAUDE_API_KEY:-}" ]]; then
  export ANTHROPIC_API_KEY="${CLAUDE_API_KEY}"
fi

exec "$@"
