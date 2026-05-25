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
  openssl rand -hex 32
}

require_cmd kubectl
require_cmd openssl
require_env OP_SERVICE_ACCOUNT_TOKEN
require_env OP_VAULT
require_env SLACK_BOT_TOKEN
require_env SLACK_SIGNING_SECRET
require_env SLACKBOT_API_KEY

kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f - >/dev/null

delete_if_forced centaur-infra-env
delete_if_forced centaur-firewall-ca
delete_if_forced centaur-firewall-ca-key
delete_if_forced centaur-onepassword-connect-credentials

if secret_exists centaur-infra-env; then
  patch_data=()
  if [[ -n "${OP_CONNECT_TOKEN:-}" ]]; then
    patch_data+=("\"OP_CONNECT_TOKEN\":\"$(printf '%s' "$OP_CONNECT_TOKEN" | base64 | tr -d '\n')\"")
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
  secret_args=(
    -n "$NAMESPACE" create secret generic centaur-infra-env
    --from-literal=IRON_MANAGEMENT_API_KEY="$(rand_hex)"
    --from-literal=SANDBOX_SIGNING_KEY="$(rand_hex)"
    --from-literal=OP_SERVICE_ACCOUNT_TOKEN="$OP_SERVICE_ACCOUNT_TOKEN"
    --from-literal=OP_VAULT="$OP_VAULT"
    --from-literal=SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN"
    --from-literal=SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET"
    --from-literal=SLACKBOT_API_KEY="$SLACKBOT_API_KEY"
    --from-literal=POSTGRES_PASSWORD="$POSTGRES_PASSWORD"
    --from-literal=DATABASE_URL="$DATABASE_URL"
  )
  if [[ -n "${OP_CONNECT_TOKEN:-}" ]]; then
    secret_args+=(--from-literal=OP_CONNECT_TOKEN="$OP_CONNECT_TOKEN")
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
