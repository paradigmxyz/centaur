#!/usr/bin/env bash

set -euo pipefail

chart_dir="${1:-contrib/chart}"
rendered_console_manifest="$(mktemp)"
invalid_value_output="$(mktemp)"
expected_allowed_hosts="console.example.test,console.centaur-system.svc.cluster.local"

cleanup() {
  rm -f "$rendered_console_manifest" "$invalid_value_output"
}
trap cleanup EXIT

if helm template console-invalid-allowed-hosts "$chart_dir" \
  --set console.enabled=true \
  --set-string console.allowedHosts=console.example.test \
  > /dev/null 2>"$invalid_value_output"; then
  echo "expected schema validation to reject scalar console.allowedHosts" >&2
  exit 1
fi
grep -Fq '/console/allowedHosts' "$invalid_value_output"
grep -Fq 'array' "$invalid_value_output"

helm template console-allowed-hosts "$chart_dir" \
  --set console.enabled=true \
  --set-string 'console.allowedHosts[0]=console.example.test' \
  --set-string 'console.allowedHosts[1]=console.centaur-system.svc.cluster.local' \
  >"$rendered_console_manifest"

grep -Fxc "$expected_allowed_hosts" <(
  yq eval-all '
    select(.kind == "Deployment")
    | .spec.template.spec.containers[]
    | select(.name == "console")
    | .env[]
    | select(.name == "CENTAUR_CONSOLE_ALLOWED_HOSTS")
    | .value
  ' "$rendered_console_manifest"
)

if yq eval-all '
  select(.kind == "Deployment")
  | .spec.template.spec.containers[]
  | select(.name == "console-worker")
  | .env[]
  | select(.name == "CENTAUR_CONSOLE_ALLOWED_HOSTS")
  | .value
' "$rendered_console_manifest" | grep -Fq .; then
  echo "CENTAUR_CONSOLE_ALLOWED_HOSTS must not be rendered into the Console worker" >&2
  exit 1
fi

echo "Console allowed-hosts render passed"
