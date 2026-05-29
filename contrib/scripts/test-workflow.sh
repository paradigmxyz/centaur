#!/usr/bin/env bash
# Trigger a workflow run on the local k8s cluster, poll until terminal,
# print the final state. Talks to the API by kubectl-exec'ing into the
# api pod and running curl from inside the cluster, so it works without
# port-forwards or external ingress.
#
# Usage:
#   test-workflow.sh [workflow_name] [input_json]
#
# Env:
#   CENTAUR_NAMESPACE  default centaur
#   CENTAUR_RELEASE    default centaur
#   CENTAUR_API_KEY    default $LOCAL_DEV_API_KEY (read from the pod)
#   TIMEOUT_S          max seconds to poll (default 60)

set -euo pipefail

workflow="${1:-echo}"
input="${2:-{\}}"
namespace="${CENTAUR_NAMESPACE:-centaur}"
release="${CENTAUR_RELEASE:-centaur}"
api_pod="deploy/${release}-centaur-api"
timeout_s="${TIMEOUT_S:-60}"
deadline=$(( $(date +%s) + timeout_s ))

if ! command -v kubectl >/dev/null; then
  echo "kubectl is required" >&2
  exit 2
fi
if ! command -v jq >/dev/null; then
  echo "jq is required on the host (for building the request body)" >&2
  exit 2
fi

body=$(jq -nc --arg name "$workflow" --argjson input "$input" \
  '{workflow_name: $name, input: $input, eager_start: true}')

# The api container has curl baked in and gets LOCAL_DEV_API_KEY from
# Helm's secret env. Stream the request body via stdin so we don't have
# to escape JSON into a shell command line.
exec_in_api() {
  kubectl exec -n "$namespace" "$api_pod" -c api -i -- sh -c "$1"
}

create=$(printf '%s' "$body" | exec_in_api '
  curl -fsS -X POST http://localhost:8000/workflows/runs \
    -H "Authorization: Bearer ${CENTAUR_API_KEY:-$LOCAL_DEV_API_KEY}" \
    -H "Content-Type: application/json" \
    --data-binary @-
')

run_id=$(printf '%s' "$create" | jq -r .run_id)
if [[ -z "$run_id" || "$run_id" == "null" ]]; then
  echo "failed to create run: $create" >&2
  exit 1
fi
echo "run_id=$run_id"

poll_cmd='
  curl -fsS "http://localhost:8000/workflows/runs/'"$run_id"'" \
    -H "Authorization: Bearer ${CENTAUR_API_KEY:-$LOCAL_DEV_API_KEY}"
'

state=""
status="unknown"
while (( $(date +%s) < deadline )); do
  if state=$(exec_in_api "$poll_cmd" 2>/dev/null); then
    status=$(printf '%s' "$state" | jq -r .status)
    echo "  status=$status"
    case "$status" in
      completed|failed|cancelled)
        printf '%s\n' "$state" | jq .
        exit 0
        ;;
    esac
  fi
  sleep 0.5
done

echo "timed out after ${timeout_s}s; last status=$status" >&2
printf '%s\n' "$state" | jq .
exit 1
