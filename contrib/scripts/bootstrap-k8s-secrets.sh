#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="centaur"
FORCE=0

usage() {
  cat <<'EOF'
Usage: scripts/bootstrap-k8s-secrets.sh [--namespace NAMESPACE] [--force]

Creates the required local-dev Kubernetes infra Secrets consumed by the Helm chart.
Requires OP_SERVICE_ACCOUNT_TOKEN, OP_VAULT, SLACK_BOT_TOKEN,
SLACK_SIGNING_SECRET, and SLACKBOT_API_KEY in the shell environment.

Optional 1Password Connect bootstrap (when ironProxy.manager.secretSource is
set to onepassword-connect in the Helm values):
  OP_CONNECT_CREDENTIALS_FILE  path to 1password-credentials.json; if set,
                               creates Secret centaur-onepassword-connect-credentials
  OP_CONNECT_TOKEN             Connect API token; added to centaur-infra-env

Optional local-dev admin key:
  LOCAL_DEV_API_KEY            seeded as the admin bearer for the API service
                               (envFrom centaur-infra-env). Re-run with --force
                               or kubectl patch to rotate.

Optional repo-cache GitHub token:
  GITHUB_TOKEN                 added to centaur-infra-env when present; the
                               repo-cache DaemonSet reads it (repoCache.githubToken
                               -> existingSecretName) to clone tool/overlay repos.
                               Updated on every run when set, so it rotates.

Optional Linear bot bootstrap (consumed when linearbot.enabled=true):
  LINEAR_ACCESS_TOKEN          actor=app OAuth token from the Linear agent
                               install; required together with the webhook
                               secret (partial config fails fast)
  LINEARBOT_WEBHOOK_SECRET     signing secret from the linearbot webhook's
                               settings page (distinct from the linear_webhook
                               workflow's LINEAR_WEBHOOK_SECRET — separate
                               Linear webhook, separate secret)
  LINEARBOT_API_KEY            bearer the bot sends to api-rs; auto-generated
                               when absent

Optional Discord ingress bootstrap (consumed when discordbot.enabled=true):
  DISCORD_BOT_TOKEN            when set, seeds the discordbot keys; requires
                               DISCORD_PUBLIC_KEY and DISCORD_APPLICATION_ID
                               (the script fails fast if either is missing).
                               DISCORD_* values are overwritten on every run so
                               they rotate.
  DISCORD_PUBLIC_KEY           Ed25519 public key from the Discord application
  DISCORD_APPLICATION_ID       Discord application id (doubles as the bot user id)
  DISCORDBOT_API_KEY           bearer the bot sends to api-rs; auto-generated
                               once when absent (never rotated in place)

Optional Teams ingress bootstrap (consumed when teamsbot.enabled=true):
  TEAMS_BOT_APP_ID             when set, seeds the teamsbot keys; requires
                               TEAMS_BOT_APP_PASSWORD and
                               TEAMS_BOT_APP_TENANT_ID (the script fails fast
                               if either is missing). TEAMS_BOT_* values are
                               overwritten on every run so they rotate.
  TEAMS_BOT_APP_PASSWORD       Bot Framework app client secret
  TEAMS_BOT_APP_TENANT_ID      Microsoft Entra tenant id for the Teams app
  TEAMSBOT_API_KEY             bearer the bot sends to api-rs; auto-generated
                               once when absent (never rotated in place)

Optional iron-control bootstrap (consumed when ironControl.enabled=true):
  IRON_CONTROL_DATABASE_URL    overrides the derived DSN (default points at the
                               bundled Postgres server with no database path, so
                               Rails resolves db names from its database.yml)
  IRON_CONTROL_INITIAL_USER_EMAIL
                               initial admin email (default admin@centaur.local)
  The initial password, API key, the three ActiveRecord encryption keys, and
  SECRET_KEY_BASE are auto-generated when absent (never rotated in place).
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --namespace|-n)
      NAMESPACE="${2:?--namespace requires a value}"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "FATAL: $name is required in the shell environment" >&2
    exit 1
  fi
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FATAL: required command not found: $1" >&2
    exit 1
  fi
}

secret_exists() {
  kubectl -n "$NAMESPACE" get secret "$1" >/dev/null 2>&1
}

delete_if_forced() {
  local name="$1"
  if [[ "$FORCE" == "1" ]]; then
    kubectl -n "$NAMESPACE" delete secret "$name" --ignore-not-found >/dev/null
  fi
}

rand_hex() {
  openssl rand -hex 32 | tr -d '\n'
}

require_cmd kubectl
require_cmd openssl
require_env OP_SERVICE_ACCOUNT_TOKEN
require_env OP_VAULT
require_env SLACK_BOT_TOKEN
require_env SLACK_SIGNING_SECRET
require_env SLACKBOT_API_KEY

# Linear config is optional but must be complete: a token without the webhook
# secret (or vice versa) deploys a linearbot that boots and then rejects every
# delivery, which reads as silence.
if [[ -n "${LINEAR_ACCESS_TOKEN:-}" || -n "${LINEARBOT_WEBHOOK_SECRET:-}" ]]; then
  require_env LINEAR_ACCESS_TOKEN
  require_env LINEARBOT_WEBHOOK_SECRET
fi

# Discord keys are optional as a group, but partial configuration would silently
# seed empty values and crashloop the bot at deploy time instead of failing here.
if [[ -n "${DISCORD_BOT_TOKEN:-}" ]]; then
  require_env DISCORD_PUBLIC_KEY
  require_env DISCORD_APPLICATION_ID
fi

# Teams keys are optional as a group, but partial configuration would silently
# seed empty values and crashloop the bot at deploy time instead of failing here.
if [[ -n "${TEAMS_BOT_APP_ID:-}${TEAMS_BOT_APP_PASSWORD:-}${TEAMS_BOT_APP_TENANT_ID:-}" ]]; then
  require_env TEAMS_BOT_APP_ID
  require_env TEAMS_BOT_APP_PASSWORD
  require_env TEAMS_BOT_APP_TENANT_ID
fi

kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

delete_if_forced centaur-infra-env
delete_if_forced centaur-firewall-ca
delete_if_forced centaur-firewall-ca-key
delete_if_forced centaur-onepassword-connect-credentials

secret_key_present() {
  local key="$1"
  local value
  value="$(kubectl -n "$NAMESPACE" get secret centaur-infra-env \
    -o "jsonpath={.data.${key}}" 2>/dev/null || true)"
  [[ -n "$value" ]]
}

if secret_exists centaur-infra-env; then
  patch_data=()
  if [[ -n "${OP_CONNECT_TOKEN:-}" ]]; then
    patch_data+=("\"OP_CONNECT_TOKEN\":\"$(printf '%s' "$OP_CONNECT_TOKEN" | base64 | tr -d '\n')\"")
  fi
  # Top-up IRON_BROKER_TOKEN for clusters bootstrapped before iron-token-broker
  # support landed. Only generated when absent so we don't rotate it out from
  # under cached iron-proxy access tokens on every script run.
  if ! secret_key_present IRON_BROKER_TOKEN; then
    patch_data+=("\"IRON_BROKER_TOKEN\":\"$(rand_hex | base64 | tr -d '\n')\"")
  fi
  if [[ -n "${LOCAL_DEV_API_KEY:-}" ]]; then
    patch_data+=("\"LOCAL_DEV_API_KEY\":\"$(printf '%s' "$LOCAL_DEV_API_KEY" | base64 | tr -d '\n')\"")
  fi
  # GITHUB_TOKEN for the repo-cache DaemonSet. Set whenever present so it can be
  # rotated; harmless when repoCache is disabled.
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    patch_data+=("\"GITHUB_TOKEN\":\"$(printf '%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')\"")
  fi
  # Discord ingress (discordbot) keys: added when DISCORD_BOT_TOKEN is in the env. DISCORD_* are
  # overwritten on each run (so rotation works); DISCORDBOT_API_KEY is generated once if absent.
  if [[ -n "${DISCORD_BOT_TOKEN:-}" ]]; then
    patch_data+=("\"DISCORD_BOT_TOKEN\":\"$(printf '%s' "$DISCORD_BOT_TOKEN" | base64 | tr -d '\n')\"")
    patch_data+=("\"DISCORD_PUBLIC_KEY\":\"$(printf '%s' "$DISCORD_PUBLIC_KEY" | base64 | tr -d '\n')\"")
    patch_data+=("\"DISCORD_APPLICATION_ID\":\"$(printf '%s' "$DISCORD_APPLICATION_ID" | base64 | tr -d '\n')\"")
    if ! secret_key_present DISCORDBOT_API_KEY; then
      patch_data+=("\"DISCORDBOT_API_KEY\":\"$(printf '%s' "${DISCORDBOT_API_KEY:-$(rand_hex)}" | base64 | tr -d '\n')\"")
    fi
  fi
  # Teams ingress (teamsbot) keys: added when TEAMS_BOT_APP_ID is in the env.
  # TEAMS_BOT_* are overwritten on each run; TEAMSBOT_API_KEY is generated once
  # if absent.
  if [[ -n "${TEAMS_BOT_APP_ID:-}" ]]; then
    patch_data+=("\"TEAMS_BOT_APP_ID\":\"$(printf '%s' "$TEAMS_BOT_APP_ID" | base64 | tr -d '\n')\"")
    patch_data+=("\"TEAMS_BOT_APP_PASSWORD\":\"$(printf '%s' "$TEAMS_BOT_APP_PASSWORD" | base64 | tr -d '\n')\"")
    patch_data+=("\"TEAMS_BOT_APP_TENANT_ID\":\"$(printf '%s' "$TEAMS_BOT_APP_TENANT_ID" | base64 | tr -d '\n')\"")
    if ! secret_key_present TEAMSBOT_API_KEY; then
      patch_data+=("\"TEAMSBOT_API_KEY\":\"$(printf '%s' "${TEAMSBOT_API_KEY:-$(rand_hex)}" | base64 | tr -d '\n')\"")
    fi
  fi
  # iron-control keys: top up only when absent so we never rotate them out from
  # under a running pod (its ActiveRecord-encrypted data would become
  # undecryptable). Generated values mirror the create path.
  if ! secret_key_present IRON_CONTROL_DATABASE_URL; then
    if [[ -n "${IRON_CONTROL_DATABASE_URL:-}" ]]; then
      ic_db_url="$IRON_CONTROL_DATABASE_URL"
    else
      # Reuse the same Postgres host/credentials as the API's DATABASE_URL but
      # strip the database path, so Rails resolves the database name from the
      # image's database.yml. Avoids decoding the password ourselves.
      existing_db_url="$(kubectl -n "$NAMESPACE" get secret centaur-infra-env \
        -o 'jsonpath={.data.DATABASE_URL}' | openssl base64 -d -A)"
      ic_db_url="${existing_db_url%/ai_v2}"
    fi
    patch_data+=("\"IRON_CONTROL_DATABASE_URL\":\"$(printf '%s' "$ic_db_url" | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_INITIAL_USER_EMAIL; then
    ic_email="${IRON_CONTROL_INITIAL_USER_EMAIL:-admin@centaur.local}"
    patch_data+=("\"IRON_CONTROL_INITIAL_USER_EMAIL\":\"$(printf '%s' "$ic_email" | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_INITIAL_USER_PASSWORD; then
    patch_data+=("\"IRON_CONTROL_INITIAL_USER_PASSWORD\":\"$(rand_hex | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_INITIAL_API_KEY; then
    patch_data+=("\"IRON_CONTROL_INITIAL_API_KEY\":\"$(printf 'iak_%s' "$(rand_hex)" | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY; then
    patch_data+=("\"IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY\":\"$(rand_hex | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_AR_ENCRYPTION_DETERMINISTIC_KEY; then
    patch_data+=("\"IRON_CONTROL_AR_ENCRYPTION_DETERMINISTIC_KEY\":\"$(rand_hex | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT; then
    patch_data+=("\"IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT\":\"$(rand_hex | base64 | tr -d '\n')\"")
  fi
  if ! secret_key_present IRON_CONTROL_SECRET_KEY_BASE; then
    patch_data+=("\"IRON_CONTROL_SECRET_KEY_BASE\":\"$(printf '%s%s' "$(rand_hex)" "$(rand_hex)" | base64 | tr -d '\n')\"")
  fi
  # Linear bot credentials. Set whenever present so the OAuth token can be
  # rotated; the api-rs bearer is generated once and kept stable.
  if [[ -n "${LINEAR_ACCESS_TOKEN:-}" ]]; then
    patch_data+=("\"LINEAR_ACCESS_TOKEN\":\"$(printf '%s' "$LINEAR_ACCESS_TOKEN" | base64 | tr -d '\n')\"")
    patch_data+=("\"LINEARBOT_WEBHOOK_SECRET\":\"$(printf '%s' "$LINEARBOT_WEBHOOK_SECRET" | base64 | tr -d '\n')\"")
    if [[ -n "${LINEARBOT_API_KEY:-}" ]]; then
      patch_data+=("\"LINEARBOT_API_KEY\":\"$(printf '%s' "$LINEARBOT_API_KEY" | base64 | tr -d '\n')\"")
    elif ! secret_key_present LINEARBOT_API_KEY; then
      patch_data+=("\"LINEARBOT_API_KEY\":\"$(rand_hex | base64 | tr -d '\n')\"")
    fi
  fi
  if [[ "${#patch_data[@]}" -gt 0 ]]; then
    patch_json="{\"data\":{$(IFS=,; echo "${patch_data[*]}")}}"
    kubectl -n "$NAMESPACE" patch secret centaur-infra-env --type merge -p "$patch_json" >/dev/null
    echo "Updated optional keys in Secret centaur-infra-env in namespace $NAMESPACE"
  fi
  echo "Secret centaur-infra-env already exists in namespace $NAMESPACE; leaving unchanged"
else
  POSTGRES_PASSWORD="$(rand_hex)"
  DATABASE_URL="postgresql://tempo:${POSTGRES_PASSWORD}@centaur-centaur-postgres:5432/ai_v2"
  # iron-control runs against a dedicated logical DB on the same Postgres. The
  # URL carries connection info only (no database path) so Rails resolves each
  # connection's database name from the image's database.yml. Override via the
  # IRON_CONTROL_DATABASE_URL env var to point at an external server.
  IRON_CONTROL_DATABASE_URL="${IRON_CONTROL_DATABASE_URL:-postgresql://tempo:${POSTGRES_PASSWORD}@centaur-centaur-postgres:5432}"
  IRON_CONTROL_INITIAL_USER_EMAIL="${IRON_CONTROL_INITIAL_USER_EMAIL:-admin@centaur.local}"
  secret_args=(
    -n "$NAMESPACE" create secret generic centaur-infra-env
    --from-literal=IRON_MANAGEMENT_API_KEY="$(rand_hex)"
    --from-literal=IRON_BROKER_TOKEN="$(rand_hex)"
    --from-literal=SANDBOX_SIGNING_KEY="$(rand_hex)"
    --from-literal=OP_SERVICE_ACCOUNT_TOKEN="$OP_SERVICE_ACCOUNT_TOKEN"
    --from-literal=OP_VAULT="$OP_VAULT"
    --from-literal=SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN"
    --from-literal=SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET"
    --from-literal=SLACKBOT_API_KEY="$SLACKBOT_API_KEY"
    --from-literal=POSTGRES_PASSWORD="$POSTGRES_PASSWORD"
    --from-literal=DATABASE_URL="$DATABASE_URL"
    --from-literal=IRON_CONTROL_DATABASE_URL="$IRON_CONTROL_DATABASE_URL"
    --from-literal=IRON_CONTROL_INITIAL_USER_EMAIL="$IRON_CONTROL_INITIAL_USER_EMAIL"
    --from-literal=IRON_CONTROL_INITIAL_USER_PASSWORD="$(rand_hex)"
    --from-literal=IRON_CONTROL_INITIAL_API_KEY="iak_$(rand_hex)"
    --from-literal=IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY="$(rand_hex)"
    --from-literal=IRON_CONTROL_AR_ENCRYPTION_DETERMINISTIC_KEY="$(rand_hex)"
    --from-literal=IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT="$(rand_hex)"
    --from-literal=IRON_CONTROL_SECRET_KEY_BASE="$(rand_hex)$(rand_hex)"
  )
  if [[ -n "${DISCORD_BOT_TOKEN:-}" ]]; then
    secret_args+=(
      --from-literal=DISCORD_BOT_TOKEN="$DISCORD_BOT_TOKEN"
      --from-literal=DISCORD_PUBLIC_KEY="$DISCORD_PUBLIC_KEY"
      --from-literal=DISCORD_APPLICATION_ID="$DISCORD_APPLICATION_ID"
      --from-literal=DISCORDBOT_API_KEY="${DISCORDBOT_API_KEY:-$(rand_hex)}"
    )
  fi
  if [[ -n "${TEAMS_BOT_APP_ID:-}" ]]; then
    secret_args+=(
      --from-literal=TEAMS_BOT_APP_ID="$TEAMS_BOT_APP_ID"
      --from-literal=TEAMS_BOT_APP_PASSWORD="$TEAMS_BOT_APP_PASSWORD"
      --from-literal=TEAMS_BOT_APP_TENANT_ID="$TEAMS_BOT_APP_TENANT_ID"
      --from-literal=TEAMSBOT_API_KEY="${TEAMSBOT_API_KEY:-$(rand_hex)}"
    )
  fi
  if [[ -n "${OP_CONNECT_TOKEN:-}" ]]; then
    secret_args+=(--from-literal=OP_CONNECT_TOKEN="$OP_CONNECT_TOKEN")
  fi
  if [[ -n "${LOCAL_DEV_API_KEY:-}" ]]; then
    secret_args+=(--from-literal=LOCAL_DEV_API_KEY="$LOCAL_DEV_API_KEY")
  fi
  if [[ -n "${GITHUB_TOKEN:-}" ]]; then
    secret_args+=(--from-literal=GITHUB_TOKEN="$GITHUB_TOKEN")
  fi
  if [[ -n "${LINEAR_ACCESS_TOKEN:-}" ]]; then
    secret_args+=(--from-literal=LINEAR_ACCESS_TOKEN="$LINEAR_ACCESS_TOKEN")
    secret_args+=(--from-literal=LINEARBOT_WEBHOOK_SECRET="$LINEARBOT_WEBHOOK_SECRET")
    secret_args+=(--from-literal=LINEARBOT_API_KEY="${LINEARBOT_API_KEY:-$(rand_hex)}")
  fi
  kubectl "${secret_args[@]}" >/dev/null
  echo "Created Secret centaur-infra-env in namespace $NAMESPACE"
fi

if secret_exists centaur-firewall-ca && secret_exists centaur-firewall-ca-key; then
  echo "Firewall CA Secrets already exist in namespace $NAMESPACE; leaving unchanged"
else
  TMPDIR="$(mktemp -d)"
  trap 'rm -rf "$TMPDIR"' EXIT
  CA_KEY="$TMPDIR/ca-key.pem"
  CA_CERT="$TMPDIR/ca-cert.pem"

  openssl genrsa -out "$CA_KEY" 4096 >/dev/null 2>&1
  openssl req -x509 -new -nodes \
    -key "$CA_KEY" -sha256 -days 3650 \
    -subj "/CN=centaur iron-proxy CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign" \
    -out "$CA_CERT" >/dev/null 2>&1

  kubectl -n "$NAMESPACE" create secret generic centaur-firewall-ca \
    --from-file=ca-cert.pem="$CA_CERT" >/dev/null
  kubectl -n "$NAMESPACE" create secret generic centaur-firewall-ca-key \
    --from-file=ca-cert.pem="$CA_CERT" \
    --from-file=ca-key.pem="$CA_KEY" >/dev/null
  echo "Created firewall CA Secrets in namespace $NAMESPACE"
fi

if [[ -n "${OP_CONNECT_CREDENTIALS_FILE:-}" ]]; then
  if [[ ! -r "$OP_CONNECT_CREDENTIALS_FILE" ]]; then
    echo "FATAL: OP_CONNECT_CREDENTIALS_FILE=$OP_CONNECT_CREDENTIALS_FILE is not readable" >&2
    exit 1
  fi
  if secret_exists centaur-onepassword-connect-credentials; then
    echo "Secret centaur-onepassword-connect-credentials already exists in namespace $NAMESPACE; leaving unchanged"
  else
    kubectl -n "$NAMESPACE" create secret generic centaur-onepassword-connect-credentials \
      --from-file=1password-credentials.json="$OP_CONNECT_CREDENTIALS_FILE" >/dev/null
    echo "Created Secret centaur-onepassword-connect-credentials in namespace $NAMESPACE"
  fi
fi
